//! Search column: field (debounced), scope always in words, filters, status line, results.

use super::{DateRange, ScopeCtx, ScopeKind, connected_hosts, fmt, host_label, note, scope_ctx};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::theme::{self, Seg, c, font, icon_font, medium, ty};
use egui::{CornerRadius, Margin, RichText, Sense, Stroke};
use runwick_core::api::projects::ById;
use runwick_core::api::records::{
    Anchor, CaptureState, ContextQuery, Hit, RecordsCollect, RecordsContext, RecordsSearch, RecordsStatus, Role, Scope,
    SearchQuery, SearchResult, StatusQuery,
};
use runwick_core::api::sessions::Tool;
use runwick_core::rpc::topic;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(250);
const PAGE: u32 = 50;

/// One computer's answer: result or error message.
type HostResult = (HostKey, Option<std::sync::Arc<SearchResult>>, Option<String>);

/// The scope actually searched, in words. A remembered scope that does not apply here falls
/// back to the next wider one.
pub(super) fn effective(kind: ScopeKind, sc: &ScopeCtx) -> (ScopeKind, Scope, String) {
    match (kind, &sc.session, &sc.project) {
        (ScopeKind::Session, Some((id, title)), _) => {
            (ScopeKind::Session, Scope::Session { id: *id }, format!("현재 세션 · {title}"))
        }
        (ScopeKind::Session | ScopeKind::Project, _, Some((id, name))) => {
            (ScopeKind::Project, Scope::Project { id: *id }, name.clone())
        }
        _ => (ScopeKind::All, Scope::All, "전체 기록".into()),
    }
}

fn from_time(d: DateRange) -> Option<runwick_core::Ts> {
    let now = runwick_core::now();
    match d {
        DateRange::All => None,
        DateRange::Today => {
            let midnight = chrono::Local::now().date_naive().and_hms_opt(0, 0, 0)?;
            midnight.and_local_timezone(chrono::Local).earliest().map(|t| t.with_timezone(&chrono::Utc))
        }
        DateRange::Week => Some(now - chrono::Duration::days(7)),
        DateRange::Month => Some(now - chrono::Duration::days(30)),
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let sc = scope_ctx(app);
    egui::Frame::new().inner_margin(Margin { left: 12, right: 12, top: 8, bottom: 10 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        field(app, ui);
        scope_switch(app, ui, &sc);
        filters(app, ui);
    });
    let (kind, scope, label) = effective(app.feat.history.scope, &sc);
    let st = &app.feat.history;
    let empty = st.sent.is_empty() && !st.pinned_only;
    let footer_h = 38.0;
    let body = ui.available_rect_before_wrap();
    let list_rect =
        egui::Rect::from_min_max(body.min, egui::pos2(body.right(), (body.bottom() - footer_h).max(body.top())));
    ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect).id_salt("history-results"), |ui| {
        if empty {
            idle(app, ui, kind, &sc, &label);
        } else {
            results(app, ui, kind, scope, &sc, &label);
        }
    });
    let foot = egui::Rect::from_min_max(egui::pos2(body.left(), list_rect.bottom()), body.max);
    ui.painter().hline(foot.x_range(), foot.top() + 0.5, Stroke::new(1.0, c::LINE));
    ui.scope_builder(egui::UiBuilder::new().max_rect(foot.shrink2(egui::vec2(14.0, 9.0))), |ui| {
        ui.add(
            egui::Label::new(
                RichText::new("지금 터미널 화면만 찾으려면 ⌘F. 여기서는 지나간 대화까지 찾습니다.")
                    .font(font(ty::SMALL))
                    .color(c::TEXT_3),
            )
            .truncate(),
        );
    });
}

fn field(app: &mut App, ui: &mut egui::Ui) {
    let id = ui.make_persistent_id("history-search-field");
    let st = &mut app.feat.history;
    let before = st.text.clone();
    let resp = theme::search_field(ui, &mut st.text, "대화 기록 검색 · ⌘⇧F", id, 34.0);
    if st.focus_search {
        resp.request_focus();
        st.focus_search = false;
    }
    if st.text != before {
        st.edited = Some(Instant::now());
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let due = st.edited.is_some_and(|t| t.elapsed() >= DEBOUNCE);
    if (enter || due) && st.text.trim() != st.sent {
        st.sent = st.text.trim().to_string();
        st.pages = 1;
        st.edited = None;
    } else if let Some(t) = st.edited {
        if due {
            st.edited = None;
        } else {
            ui.ctx().request_repaint_after(DEBOUNCE.saturating_sub(t.elapsed()));
        }
    }
}

fn scope_switch(app: &mut App, ui: &mut egui::Ui, sc: &ScopeCtx) {
    let (kind, _, _) = effective(app.feat.history.scope, sc);
    let sel = match kind {
        ScopeKind::Session => 0,
        ScopeKind::Project => 1,
        ScopeKind::All => 2,
    };
    let w = ui.available_width();
    ui.allocate_ui(egui::vec2(w, 30.0), |ui| {
        if let Some(i) =
            theme::segmented(ui, &[Seg::text("현재 세션"), Seg::text("프로젝트"), Seg::text("전체")], sel, 26.0)
        {
            let want = [ScopeKind::Session, ScopeKind::Project, ScopeKind::All][i];
            let missing = match want {
                ScopeKind::Session => {
                    sc.session.is_none().then_some("열린 세션 탭이 없어 현재 세션으로 좁힐 수 없어요")
                }
                ScopeKind::Project => sc.project.is_none().then_some("선택한 프로젝트가 없어요"),
                ScopeKind::All => None,
            };
            match missing {
                Some(m) => app.toast_info(m),
                None => {
                    app.feat.history.scope = want;
                    app.feat.history.pages = 1;
                }
            }
        }
    });
}

fn filters(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        let st = &mut app.feat.history;
        let date = fmt::chip_button(ui, &format!("날짜: {}", st.date.label()), st.date != DateRange::All, true);
        egui::Popup::menu(&date).show(|ui| {
            for d in [DateRange::All, DateRange::Today, DateRange::Week, DateRange::Month] {
                if ui.selectable_label(st.date == d, d.label()).clicked() {
                    st.date = d;
                    st.pages = 1;
                    ui.close();
                }
            }
        });
        let tool_label = st.tool.map(Tool::label).unwrap_or("전체");
        let tool = fmt::chip_button(ui, &format!("도구: {tool_label}"), st.tool.is_some(), true);
        egui::Popup::menu(&tool).show(|ui| {
            for t in [None, Some(Tool::Claude), Some(Tool::Codex)] {
                if ui.selectable_label(st.tool == t, t.map(Tool::label).unwrap_or("전체")).clicked() {
                    st.tool = t;
                    st.pages = 1;
                    ui.close();
                }
            }
        });
        if fmt::chip_button(ui, "핀만 보기", st.pinned_only, false).clicked() {
            st.pinned_only = !st.pinned_only;
            st.pages = 1;
        }
    });
}

/// Nothing typed: what can be searched, and the collection state of the scope.
fn idle(app: &mut App, ui: &mut egui::Ui, kind: ScopeKind, sc: &ScopeCtx, label: &str) {
    egui::ScrollArea::vertical().id_salt("history-idle").auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::symmetric(12, 0)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            note(ui, &format!("{label}에서 찾아요. 한국어·영문·코드 일부도 찾을 수 있어요."));
            ui.add_space(4.0);
            match (kind, &sc.session) {
                (ScopeKind::Session, Some((id, _))) => {
                    capture_line(app, ui, sc.host, StatusQuery { session_id: Some(*id), project_id: None });
                    recent(app, ui, sc.host, *id);
                }
                _ => {
                    let project_id = match kind {
                        ScopeKind::Project => sc.project.as_ref().map(|p| p.0),
                        _ => None,
                    };
                    capture_line(app, ui, sc.host, StatusQuery { session_id: None, project_id });
                }
            }
        });
    });
}

/// Collection state of the scope's sessions, with problems listed.
fn capture_line(app: &mut App, ui: &mut egui::Ui, host: HostKey, q: StatusQuery) {
    let res = app.hub.query::<RecordsStatus>(host, &q);
    let Some(list) = res.data else {
        if let Some(e) = res.error {
            note(ui, &format!("수집 상태를 읽지 못했어요: {}", e.message));
        }
        return;
    };
    let agents: Vec<_> = list.iter().filter(|s| s.state != CaptureState::NotApplicable).collect();
    if q.session_id.is_some() {
        if let Some(s) = list.first() {
            let mut line = format!("기록 {} · 메시지 {}개", s.state.label(), s.message_count);
            if let Some(t) = s.last_collected_at {
                line.push_str(&format!(" · 마지막 수집 {}", crate::shell::data::ago(t)));
            }
            status_chip(ui, s.state, &line);
            if let Some(e) = &s.last_error {
                note(ui, e);
            }
        }
        return;
    }
    let count = |st: CaptureState| agents.iter().filter(|s| s.state == st).count();
    let (collecting, missing, errors) =
        (count(CaptureState::Collecting), count(CaptureState::NoFile), count(CaptureState::Error));
    let total: u64 = agents.iter().map(|s| s.message_count).sum();
    note(ui, &format!("대화 세션 {}개 · 메시지 {total}개 수집됨", agents.len()));
    if collecting > 0 {
        status_chip(
            ui,
            CaptureState::Collecting,
            &format!("수집 중 {collecting}개 · 실행 중인 세션은 계속 따라 읽어요"),
        );
    }
    if missing + errors > 0 {
        status_chip(ui, CaptureState::Error, &format!("기록을 읽지 못한 세션 {}개", missing + errors));
        let sessions = crate::shell::data::all_sessions(app, host).data;
        for s in agents.iter().filter(|s| matches!(s.state, CaptureState::NoFile | CaptureState::Error)).take(5) {
            let title = sessions
                .as_ref()
                .and_then(|l| l.iter().find(|x| x.session.id == s.session_id).map(|x| x.session.title.clone()))
                .unwrap_or_else(|| "세션".into());
            ui.horizontal(|ui| {
                let why = s.last_error.clone().unwrap_or_else(|| s.state.label().to_string());
                theme::truncated(ui, format!("{title} · {why}"), font(ty::SMALL), c::TEXT_2);
                if fmt::small_button(ui, "다시 읽기").clicked() {
                    app.hub.call::<RecordsCollect>(host, ById { id: s.session_id }, |app, res| match res {
                        Ok(st) => app.toast_info(&format!("기록 상태: {}", st.state.label())),
                        Err(e) => app.toast_error(&e.message),
                    });
                }
            });
        }
    }
}

fn state_tone(s: CaptureState) -> theme::Tone {
    match s {
        CaptureState::Collecting => theme::Tone::Running,
        CaptureState::UpToDate => theme::Tone::Neutral,
        CaptureState::NoFile | CaptureState::WaitingForId => theme::Tone::Check,
        CaptureState::NotApplicable => theme::Tone::Waiting,
        CaptureState::Error => theme::Tone::Recover,
    }
}

fn status_chip(ui: &mut egui::Ui, s: CaptureState, text: &str) {
    ui.horizontal_wrapped(|ui| {
        theme::chip(ui, state_tone(s), text);
    });
}

/// The newest messages of the current session (no query yet).
fn recent(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: runwick_core::api::sessions::SessionId) {
    let q = ContextQuery { anchor: Anchor::SessionEnd { id: session }, before: 20, after: 0 };
    let res = app.hub.query::<RecordsContext>(host, &q);
    let Some(cx) = res.data else {
        if let Some(e) = res.error {
            note(ui, &format!("기록을 읽지 못했어요: {}", e.message));
        } else {
            note(ui, "기록을 불러오는 중…");
        }
        return;
    };
    if cx.messages.is_empty() {
        note(ui, "아직 수집된 대화가 없어요. CLI에 메시지를 보내면 몇 초 안에 여기에 남아요.");
        return;
    }
    ui.add_space(4.0);
    fmt::label(ui, "이 세션의 최근 대화");
    if fmt::quiet_link(ui, "전체 대화 열기").clicked() {
        super::open_session_records(app, host, session);
    }
    for m in cx.messages.iter().rev().filter(|m| m.role != Role::Tool) {
        let who = if m.role == Role::User { "나".to_string() } else { cx.tool.label().to_string() };
        let text: String = m.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect();
        let resp = card(ui, false, |ui| {
            ui.label(RichText::new(format!("{who} · {}", fmt::when_full(m.at))).font(font(11.5)).color(c::TEXT_3));
            ui.add(egui::Label::new(RichText::new(text).font(font(12.5)).color(c::TEXT)).wrap());
        });
        if resp.clicked() {
            super::open_message(app, host, m.id, Vec::new());
        }
    }
}

/// A clickable card row. Returns the click response.
fn card(ui: &mut egui::Ui, selected: bool, body: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let fill = if selected { c::CONTROL } else { c::ROW };
    let inner = egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 5.0;
            body(ui);
        },
    );
    let rect = inner.response.rect;
    let resp = ui.interact(rect, ui.scope_id().with(("card", rect.min.y as i32)), Sense::click());
    if resp.hovered() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0, c::LINE_STRONG),
            egui::StrokeKind::Inside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if selected {
        ui.painter().rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, c::TEXT_OFF), egui::StrokeKind::Inside);
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(8));
    resp
}

fn results(app: &mut App, ui: &mut egui::Ui, kind: ScopeKind, scope: Scope, sc: &ScopeCtx, label: &str) {
    let st = &app.feat.history;
    let pages = st.pages.max(1);
    let q = SearchQuery {
        text: st.sent.clone(),
        scope,
        from: from_time(st.date),
        to: None,
        tool: st.tool,
        pinned_only: st.pinned_only,
        limit: PAGE * pages,
    };
    let hosts: Vec<HostKey> = if kind == ScopeKind::All { connected_hosts(app) } else { vec![sc.host] };
    let multi = hosts.len() > 1;
    let mut per_host: Vec<HostResult> = Vec::new();
    for h in &hosts {
        let r = app.hub.query::<RecordsSearch>(*h, &q);
        per_host.push((*h, r.data, r.error.map(|e| e.message)));
    }
    let filter = app.feat.history.host_filter.filter(|h| hosts.contains(h));
    let shown: Vec<&HostResult> = per_host.iter().filter(|(h, ..)| filter.is_none_or(|f| f == *h)).collect();
    let mut hits: Vec<(HostKey, &Hit)> =
        shown.iter().flat_map(|(h, r, ..)| r.iter().flat_map(move |r| r.hits.iter().map(move |x| (*h, x)))).collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.1.message.at));
    let total: u64 = shown.iter().filter_map(|(_, r, ..)| r.as_ref().map(|r| r.total)).sum();
    let loaded: usize = shown.iter().filter_map(|(_, r, ..)| r.as_ref().map(|r| r.hits.len())).sum();
    let first_load = shown.iter().all(|(_, r, e)| r.is_none() && e.is_none());
    let errors: Vec<String> =
        shown.iter().filter_map(|(h, _, e)| e.as_ref().map(|e| format!("{}: {e}", host_label(app, *h)))).collect();
    let collecting: u32 = shown.iter().filter_map(|(_, r, ..)| r.as_ref().map(|r| r.collecting)).sum();
    let incomplete: u32 = shown.iter().filter_map(|(_, r, ..)| r.as_ref().map(|r| r.incomplete)).sum();
    let scanned = shown.iter().any(|(_, r, ..)| r.as_ref().is_some_and(|r| r.scanned));
    let index_error = shown.iter().find_map(|(_, r, ..)| r.as_ref().and_then(|r| r.index_error.clone()));
    let unreachable = app.hub.hosts().filter(|c| !c.state().is_connected()).count();
    let terms = fmt::query_terms(&q.text);
    let open_id = match app.feat.history.reader.open.as_ref().map(|o| o.anchor) {
        Some(Anchor::Message { id }) => Some(id),
        _ => None,
    };

    egui::Frame::new().inner_margin(Margin { left: 12, right: 12, top: 0, bottom: 8 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        if multi {
            host_chips(app, ui, &per_host);
        }
        if kind == ScopeKind::All && unreachable > 0 {
            ui.horizontal_wrapped(|ui| {
                theme::chip(ui, theme::Tone::Check, &format!("연결되지 않은 기기 {unreachable}곳은 검색하지 못했어요"));
            });
        }
        if first_load {
            note(ui, "찾는 중…");
            return;
        }
        let mut line = if total == 0 {
            format!("{label}에서 찾지 못했어요.")
        } else if multi && filter.is_none() {
            format!("기기 {}곳에서 {total}건 찾음", shown.len())
        } else {
            format!("{label}에서 {total}건 찾음")
        };
        if collecting > 0 {
            line.push_str(&format!(" · 세션 {collecting}개는 아직 수집 중이라 최신 내용이 빠졌을 수 있어요."));
        }
        if incomplete > 0 {
            line.push_str(&format!(" · 기록을 읽지 못한 세션 {incomplete}개는 검색에 빠졌어요."));
        }
        note(ui, &line);
        if scanned && index_error.is_none() {
            note(ui, "세 글자보다 짧은 단어는 색인 없이 훑어서 찾았어요.");
        }
        if let Some(e) = index_error {
            ui.horizontal_wrapped(|ui| {
                theme::chip(ui, theme::Tone::Check, "색인 다시 만드는 중");
            });
            note(ui, &e);
        }
        for e in &errors {
            ui.horizontal_wrapped(|ui| {
                theme::chip(ui, theme::Tone::Recover, "검색 실패");
                ui.label(RichText::new(e).font(font(ty::SMALL)).color(c::TEXT_2));
            });
        }
        if !errors.is_empty() && fmt::small_button(ui, "다시 시도").clicked() {
            for h in &hosts {
                app.hub.invalidate(*h, &[topic::MESSAGES]);
            }
        }
        if total == 0 && errors.is_empty() && kind != ScopeKind::All {
            note(ui, "범위를 '전체'로 넓히거나 다른 단어로 찾아보세요.");
        }
    });
    if first_load || hits.is_empty() {
        return;
    }
    let mut clicked = None;
    let mut more = false;
    egui::ScrollArea::vertical().id_salt("history-hits").auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 0, bottom: 8 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (host, hit) in &hits {
                let device = multi.then(|| host_label(app, *host));
                let resp = card(ui, open_id == Some(hit.message.id), |ui| hit_body(ui, hit, device.as_deref()));
                if resp.clicked() {
                    clicked = Some((*host, hit.message.id));
                }
            }
            if (loaded as u64) < total {
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                    if fmt::small_button(ui, &format!("더 보기 · {}건 남음", total - loaded as u64)).clicked() {
                        more = true;
                    }
                });
            }
        });
    });
    if more {
        app.feat.history.pages = pages + 1;
    }
    if let Some((host, id)) = clicked {
        super::open_message(app, host, id, terms);
    }
}

fn hit_body(ui: &mut egui::Ui, hit: &Hit, device: Option<&str>) {
    let color = if hit.message.role == Role::User { c::TEXT_STRONG } else { c::TEXT };
    let job = fmt::highlighted(&hit.snippet, &hit.highlights, font(12.5), color);
    ui.add(egui::Label::new(job).wrap());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if let Some(d) = device {
            fmt::device_badge(ui, d, false);
        }
        let who = match hit.message.role {
            Role::User => "나",
            Role::Assistant => hit.tool.label(),
            Role::Tool => "도구",
        };
        let w = ui.available_width();
        ui.scope(|ui| {
            ui.set_max_width((w * 0.5).max(80.0));
            theme::truncated(ui, hit.session_title.clone(), font(11.5), c::TEXT_2);
        });
        ui.label(RichText::new(who).font(font(11.5)).color(c::TEXT_3));
        ui.label(RichText::new(fmt::when(hit.message.at)).font(font(11.5)).color(c::TEXT_3));
        if !hit.project_name.is_empty() {
            ui.scope(|ui| {
                ui.set_max_width((w * 0.3).max(60.0));
                theme::truncated(ui, hit.project_name.clone(), font(11.5), c::TEXT_3);
            });
        }
        if hit.message.sidechain {
            ui.label(RichText::new("하위 에이전트").font(font(11.5)).color(c::TEXT_3));
        }
        if hit.pinned {
            ui.label(RichText::new(icons::PIN).font(icon_font(11.0)).color(c::TEXT));
            ui.label(RichText::new("핀").font(medium(11.5)).color(c::TEXT));
        }
    });
}

/// "검색한 곳": all computers or one, with counts.
fn host_chips(app: &mut App, ui: &mut egui::Ui, per_host: &[HostResult]) {
    egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 8)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("검색한 곳 · 눌러서 그 기기 결과만 보기").font(font(ty::SMALL)).color(c::TEXT_3));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                let all: u64 = per_host.iter().filter_map(|(_, r, ..)| r.as_ref().map(|r| r.total)).sum();
                let cur = app.feat.history.host_filter;
                if fmt::chip_button(ui, &format!("모두 {all}"), cur.is_none(), false).clicked() {
                    app.feat.history.host_filter = None;
                }
                for (h, r, e) in per_host {
                    let n = match (r, e) {
                        (Some(r), _) => r.total.to_string(),
                        (None, Some(_)) => "실패".into(),
                        _ => "…".into(),
                    };
                    let label = format!("{} {n}", host_label(app, *h));
                    if fmt::chip_button(ui, &label, cur == Some(*h), false).clicked() {
                        app.feat.history.host_filter = Some(*h);
                    }
                }
            });
        },
    );
}
