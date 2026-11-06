//! S06 전체 활동 (Activity.dc.html, RemoteActivity.dc.html): everything that needs the person or
//! runs, grouped 내 답변·승인 필요 → 복구 필요 → 새 결과 → 실행 중 → 대기, with automatic jobs
//! as their own kind. Grouping is `runwick_core::api::activity`, shared with the collaboration
//! panel and `runwick status`, so a run is never counted differently in two places. Devices that
//! cannot be reached are "상태 확인 불가", never zero.

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, data, widgets};
use crate::state::{Modal, RightPanel, SettingsSection};
use crate::theme::{self, Tone, c, font, icon, medium};
use crate::views::collab::{self, look, place};
use egui::{CornerRadius, Margin, RichText, Sense};
use runwick_core::Ts;
use runwick_core::api::activity::{Group, headline_counts, row_group, session_group};
use runwick_core::api::collab::{CollabList, ReqStatus, Request, RequestQuery};
use runwick_core::api::jobs::{JobQuery, JobStatus, JobsList};
use runwick_core::api::projects::ProjectId;
use runwick_core::api::sessions::{Attention, Phase, SessionId, SessionInfo};

#[derive(Default)]
pub struct State {
    project: Option<ProjectId>,
    kind: Option<Kind>,
    group: Option<Group>,
    by_device: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Session,
    Collab,
    Auto,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Session => "CLI 세션",
            Kind::Collab => "협업",
            Kind::Auto => "자동 작업",
        }
    }
}

#[derive(Clone)]
enum Go {
    Peek(HostKey, SessionId, Option<Attention>),
    Result(HostKey, uuid::Uuid),
    Collab(HostKey, SessionId),
    Open(HostKey, SessionId),
    Automation,
    Resume,
    Devices,
}

struct Item {
    group: Group,
    /// Its process is alive (counts as 실행 in the title bar, whatever its group).
    alive: bool,
    kind: Kind,
    title: String,
    state: String,
    when: String,
    where_: String,
    why: String,
    act: &'static str,
    go: Go,
    project: Option<ProjectId>,
    device: String,
    at: Ts,
}

struct Unreachable {
    device: String,
    /// The person disconnected it on purpose (`연결 끊음`), as opposed to not reachable.
    off: bool,
    reason: String,
}

/// Everything listed, from every host this window knows.
struct Gathered {
    items: Vec<Item>,
    unreachable: Vec<Unreachable>,
    /// This computer's sessions are not loaded yet.
    loading: bool,
    multi: bool,
}

fn gather(app: &mut App) -> Gathered {
    let hosts: Vec<HostKey> = {
        let mut v: Vec<HostKey> = app.hub.hosts().map(|c| c.key).collect();
        v.sort();
        v
    };
    let mut g = Gathered { items: Vec::new(), unreachable: Vec::new(), loading: false, multi: hosts.len() > 1 };
    for host in &hosts {
        // The name the person registered the device under, not the computer's own name.
        let device = crate::remote::host_label(app, *host);
        if let HostKey::Device(id) = *host
            && !app.hub.state(*host).is_connected()
        {
            let off = matches!(crate::remote::status(app, id), crate::remote::Status::Off);
            g.unreachable.push(Unreachable {
                device,
                off,
                reason: if off {
                    "연결 끊음 · 다시 연결하면 실제 상태를 가져와요".into()
                } else {
                    "연결 안 됨 · 연결되면 실제 상태를 가져와요. 실패로 보지 않아요".into()
                },
            });
            continue;
        }
        g.loading |= collect(app, *host, &device, &mut g.items);
    }
    g
}

/// Title bar counts, from the same list as the panel: things that need the person (답변·승인,
/// 복구, 새 결과) and things observed running. `None` until this computer's sessions are known.
pub fn counts(app: &mut App) -> Option<(usize, usize)> {
    data::all_sessions(app, HostKey::Local).data.as_ref()?;
    let g = gather(app);
    Some(headline_counts(g.items.iter().map(|i| (Some(i.group), i.alive))))
}

pub fn show_panel(app: &mut App, ui: &mut egui::Ui) {
    let Gathered { mut items, unreachable, loading, multi } = gather(app);
    let st = &app.feat.activity;
    let (fp, fk, fg, by_device) = (st.project, st.kind, st.group, st.by_device && multi);
    items.retain(|i| {
        fp.is_none_or(|p| i.project == Some(p)) && fk.is_none_or(|k| i.kind == k) && fg.is_none_or(|g| i.group == g)
    });
    items.sort_by(|a, b| a.group.cmp(&b.group).then(b.at.cmp(&a.at)));

    let mut go: Option<Go> = None;
    let mut filters = FilterChange::default();
    egui::Frame::new().inner_margin(Margin { left: 12, right: 10, top: 10, bottom: 10 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        filter_row(app, ui, multi, &mut filters);
    });
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, c::LINE));
    let footer_h = 40.0;
    egui::ScrollArea::vertical()
        .max_height((ui.available_height() - footer_h).max(80.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new().inner_margin(Margin::symmetric(8, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 3.0;
                if items.is_empty() && unreachable.is_empty() {
                    if loading {
                        widgets::empty_note(ui, "불러오는 중…");
                    } else {
                        empty(ui);
                    }
                    return;
                }
                if by_device {
                    let mut devices: Vec<String> = items.iter().map(|i| i.device.clone()).collect();
                    devices.dedup();
                    for d in devices {
                        section(ui, &d, c::TEXT_2, None);
                        for it in items.iter().filter(|i| i.device == d) {
                            if let Some(g) = row(ui, it, false) {
                                go = Some(g);
                            }
                        }
                        ui.add_space(11.0);
                    }
                    unreachable_rows(ui, &unreachable, &mut go);
                    return;
                }
                for g in Group::ORDER {
                    let rows: Vec<&Item> = items.iter().filter(|i| i.group == g).collect();
                    if g == Group::Recover {
                        unreachable_rows(ui, &unreachable, &mut go);
                    }
                    if rows.is_empty() {
                        continue;
                    }
                    section(ui, g.label(), group_tone(g).fg(), Some(rows.len()));
                    for it in rows {
                        if let Some(x) = row(ui, it, multi) {
                            go = Some(x);
                        }
                    }
                    ui.add_space(11.0);
                }
            });
        });
    footer(app, ui, &mut go);
    filters.apply(app);
    if let Some(g) = go {
        follow(app, g);
    }
}

fn group_tone(g: Group) -> Tone {
    match g {
        Group::Attention => Tone::Attention,
        Group::Recover => Tone::Recover,
        Group::Fresh => Tone::Info,
        Group::Running => Tone::Running,
        Group::Waiting => Tone::Waiting,
    }
}

/// Add the items of one host. Returns whether its data is still loading.
fn collect(app: &mut App, host: HostKey, device: &str, out: &mut Vec<Item>) -> bool {
    let sq = data::all_sessions(app, host);
    let Some(sessions) = sq.data else {
        return sq.error.is_none();
    };
    let reqs = app.hub.query::<CollabList>(host, &RequestQuery { limit: 400, ..Default::default() }).data;
    let reqs: Vec<Request> = reqs.map(|v| v.as_ref().clone()).unwrap_or_default();
    let direct = collab::overview(app, host).data.map(|o| o.direct_workers.clone()).unwrap_or_default();
    let now = runwick_core::now();
    for s in sessions.iter().filter(|s| !s.session.archived) {
        let live = data::live_of(app, host, s);
        let alive = live.as_ref().is_some_and(|l| matches!(l.phase, Phase::Running | Phase::Starting));
        let latest = reqs.iter().find(|r| r.worker_id == s.session.id);
        let pl = place(app, host, &s.session);
        let where_ = format!("{} / {}", pl.project, pl.workspace);
        let sg = session_group(&s.session, live.as_ref());
        let attention = live.as_ref().and_then(|l| l.attention.clone());
        let parent_title = |id: Option<SessionId>| {
            id.and_then(|p| sessions.iter().find(|x| x.session.id == p)).map(|p| p.session.title.clone())
        };
        // A request that needs nothing any more (answered and seen) leaves the session's own state.
        let latest = latest.filter(|r| row_group(&s.session, live.as_ref(), Some(r)).is_some());
        let item = match latest {
            Some(r) => {
                let Some(group) = row_group(&s.session, live.as_ref(), Some(r)) else {
                    continue;
                };
                let lk = look(r, direct.contains(&s.session.id));
                let state = sg.filter(|g| g.0 == Group::Attention).map(|g| g.1.to_string()).unwrap_or(lk.label);
                let why = match parent_title(r.parent_id) {
                    Some(p) => format!("{p}에서 위임 · {}", r.tool.label()),
                    None => format!("내가 맡긴 일 · {}", r.tool.label()),
                };
                let (act, go) = match (group, r.status) {
                    (Group::Attention, _) => ("보기", Go::Peek(host, s.session.id, attention.clone())),
                    (_, ReqStatus::Uncertain) => match r.parent_id {
                        Some(p) => ("확인", Go::Collab(host, p)),
                        None => ("확인", Go::Peek(host, s.session.id, None)),
                    },
                    (Group::Recover | Group::Fresh, _) => ("결과 보기", Go::Result(host, r.id)),
                    (Group::Running, _) => ("보기", Go::Peek(host, s.session.id, None)),
                    (Group::Waiting, _) => match r.parent_id {
                        Some(p) => ("보기", Go::Collab(host, p)),
                        None => ("보기", Go::Peek(host, s.session.id, None)),
                    },
                };
                Item {
                    group,
                    alive,
                    kind: Kind::Collab,
                    title: r.title.clone(),
                    state,
                    when: when_request(r, attention.as_ref(), live.as_ref().and_then(|l| l.last_output_at), now),
                    where_,
                    why,
                    act,
                    go,
                    project: pl.project_id,
                    device: device.to_string(),
                    at: r.created_at,
                }
            }
            None => {
                // A live CLI waiting for input is still a run: listed under 실행 중 with its state.
                let idle = alive.then(|| (Group::Running, data::status(&s.session, live.as_ref()).label));
                let Some((group, state)) = sg.map(|(g, t)| (g, t.to_string())).or(idle) else {
                    continue;
                };
                let why = match parent_title(s.session.parent_id) {
                    Some(p) => format!("{p}에서 위임 · {}", s.session.tool.label()),
                    None => format!("내가 시작한 세션 · {}", s.session.tool.label()),
                };
                let (act, go) = if s.session.interrupted && live.is_none() {
                    ("재개 선택", Go::Resume)
                } else {
                    (if group == Group::Attention { "보기" } else { "열기" }, Go::Open(host, s.session.id))
                };
                Item {
                    group,
                    alive,
                    kind: Kind::Session,
                    title: s.session.title.clone(),
                    state,
                    when: when_session(s, attention.as_ref(), live.as_ref().and_then(|l| l.last_output_at), now),
                    where_,
                    why,
                    act,
                    go,
                    project: pl.project_id,
                    device: device.to_string(),
                    at: s.session.created_at,
                }
            }
        };
        out.push(item);
    }
    if host == HostKey::Local {
        let jobs = app.hub.query::<JobsList>(host, &JobQuery { active_only: true, ..Default::default() }).data;
        for j in jobs.iter().flat_map(|l| l.iter()) {
            let (group, state) = match j.status {
                JobStatus::Running => (Group::Running, "자동 · 실행 중".to_string()),
                JobStatus::Held => (
                    Group::Waiting,
                    format!("보류 · {}", j.held_reason.clone().unwrap_or_else(|| "이유 확인 불가".into())),
                ),
                JobStatus::Queued => (Group::Waiting, "자동 · 대기".to_string()),
                _ => continue,
            };
            let when = match j.started_at {
                Some(t) => format!("{} 시작", data::ago(t)),
                None => format!("{} 접수", data::ago(j.created_at)),
            };
            out.push(Item {
                group,
                alive: j.status == JobStatus::Running,
                kind: Kind::Auto,
                title: j.kind.label().to_string(),
                state,
                when,
                where_: j.project_name.clone().unwrap_or_else(|| "프로젝트 확인 불가".into()),
                why: format!("자동 작업 · {}", j.reason),
                act: "설정",
                go: Go::Automation,
                project: Some(j.project_id),
                device: device.to_string(),
                at: j.created_at,
            });
        }
    }
    false
}

fn when_request(r: &Request, a: Option<&Attention>, last_out: Option<Ts>, now: Ts) -> String {
    if let Some(a) = a {
        return data::ago_from(a.since, now);
    }
    match r.status {
        ReqStatus::Running => {
            last_out.map(|t| format!("{} 업데이트", data::ago_from(t, now))).unwrap_or_else(|| "진행 중".into())
        }
        ReqStatus::Queued => format!("접수 {}", data::ago_from(r.created_at, now)),
        ReqStatus::Delivered | ReqStatus::Uncertain => {
            format!("{} · {}", r.receipt(), data::ago_from(r.delivered_at.unwrap_or(r.created_at), now))
        }
        _ => data::ago_from(r.finished_at.unwrap_or(r.updated_at), now),
    }
}

fn when_session(s: &SessionInfo, a: Option<&Attention>, last_out: Option<Ts>, now: Ts) -> String {
    if let Some(a) = a {
        return data::ago_from(a.since, now);
    }
    match last_out {
        Some(t) => format!("{} 업데이트", data::ago_from(t, now)),
        None => data::ago_from(s.session.last_active_at.unwrap_or(s.session.created_at), now),
    }
}

fn section(ui: &mut egui::Ui, title: &str, color: egui::Color32, n: Option<usize>) {
    let text = match n {
        Some(n) => format!("{title} {n}"),
        None => title.to_string(),
    };
    egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 0, bottom: 3 }).show(ui, |ui| {
        ui.label(RichText::new(text).font(medium(12.0)).color(color));
    });
}

fn row(ui: &mut egui::Ui, it: &Item, show_device: bool) -> Option<Go> {
    let tone = group_tone(it.group);
    let glyph = match it.group {
        Group::Attention => icons::CIRCLE_ALERT,
        Group::Recover => icons::TRIANGLE_ALERT,
        Group::Fresh => icons::CIRCLE_CHECK,
        Group::Running => icons::CIRCLE_DOT,
        Group::Waiting => icons::CIRCLE_PAUSE,
    };
    let mut out = None;
    let frame = egui::Frame::new()
        .fill(c::ROW)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 9.0;
                ui.label(icon(glyph, 14.0, tone.fg())).on_hover_text(it.group.label());
                let act_w = widgets::text_width(ui, it.act, font(12.0)) + 18.0;
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - act_w - 9.0).max(60.0));
                    ui.spacing_mut().item_spacing.y = 1.0;
                    theme::truncated(ui, &it.title, medium(13.0), c::TEXT_STRONG);
                    ui.label(RichText::new(format!("{} · {}", it.state, it.when)).font(font(12.0)).color(c::TEXT));
                    let where_ =
                        if show_device { format!("{} · {}", it.device, it.where_) } else { it.where_.clone() };
                    theme::truncated(ui, &where_, font(11.5), c::TEXT_3);
                    ui.add(egui::Label::new(RichText::new(&it.why).font(font(11.5)).color(c::TEXT_3)).wrap());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    let b = egui::Button::new(RichText::new(it.act).font(font(12.0)).color(c::TEXT_STRONG))
                        .fill(c::CONTROL_HOVER)
                        .min_size(egui::vec2(0.0, 26.0))
                        .corner_radius(CornerRadius::same(6));
                    if ui.add(b).clicked() {
                        out = Some(it.go.clone());
                    }
                });
            });
        });
    let click = ui.interact(
        frame.response.rect,
        ui.scope_id().with(("activity-row", &it.title, it.at.timestamp_millis())),
        Sense::click(),
    );
    if out.is_none() && click.clicked() {
        out = Some(it.go.clone());
    }
    out
}

fn unreachable_rows(ui: &mut egui::Ui, list: &[Unreachable], go: &mut Option<Go>) {
    for (off, title) in [(false, "상태 확인 불가"), (true, "연결 끊음")] {
        let rows: Vec<&Unreachable> = list.iter().filter(|u| u.off == off).collect();
        if !rows.is_empty() {
            device_rows(ui, title, &rows, go);
        }
    }
}

fn device_rows(ui: &mut egui::Ui, title: &str, list: &[&Unreachable], go: &mut Option<Go>) {
    section(ui, title, c::GREY, Some(list.len()));
    for u in list {
        let glyph = if u.off { icons::UNPLUG } else { icons::WIFI_OFF };
        egui::Frame::new()
            .fill(c::ROW)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    ui.label(icon(glyph, 14.0, c::GREY));
                    ui.vertical(|ui| {
                        ui.set_width((ui.available_width() - 80.0).max(60.0));
                        ui.label(RichText::new(&u.device).font(medium(13.0)).color(c::TEXT_STRONG));
                        ui.add(egui::Label::new(RichText::new(&u.reason).font(font(11.5)).color(c::TEXT_3)).wrap());
                    });
                    if ui
                        .add(egui::Button::new(RichText::new("기기 확인").font(font(12.0))).fill(c::CONTROL_HOVER))
                        .clicked()
                    {
                        *go = Some(Go::Devices);
                    }
                });
            });
    }
    ui.add_space(11.0);
}

fn empty(ui: &mut egui::Ui) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(icon(icons::INBOX, 22.0, c::TEXT_3));
        ui.label(RichText::new("실행 중인 작업 없음").font(medium(13.0)).color(c::TEXT));
        ui.add(
            egui::Label::new(
                RichText::new(
                    "확인할 일이 생기거나 실행이 시작되면 여기에 모여요. 대화와 기록은 계속 저장되고 있어요.",
                )
                .font(font(12.0))
                .color(c::TEXT_3),
            )
            .wrap(),
        );
    });
}

#[derive(Default)]
struct FilterChange {
    project: Option<Option<ProjectId>>,
    kind: Option<Option<Kind>>,
    group: Option<Option<Group>>,
    by_device: Option<bool>,
}

impl FilterChange {
    fn apply(self, app: &mut App) {
        let st = &mut app.feat.activity;
        if let Some(p) = self.project {
            st.project = p;
        }
        if let Some(k) = self.kind {
            st.kind = k;
        }
        if let Some(g) = self.group {
            st.group = g;
        }
        if let Some(d) = self.by_device {
            st.by_device = d;
        }
    }
}

fn pill(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let b = egui::Button::new(collab::with_icon(icons::CHEVRON_DOWN, text, font(12.0), c::TEXT, true))
        .fill(c::CONTROL)
        .min_size(egui::vec2(0.0, 26.0))
        .corner_radius(CornerRadius::same(6));
    ui.add(b)
}

fn filter_row(app: &mut App, ui: &mut egui::Ui, multi: bool, ch: &mut FilterChange) {
    let projects = data::projects(app).data;
    let st = &app.feat.activity;
    let pname = st
        .project
        .and_then(|p| projects.as_ref().and_then(|l| data::project_of(l, p)).map(|p| p.name.clone()))
        .unwrap_or_else(|| "프로젝트 전체".into());
    let kname = st.kind.map(|k| k.label()).unwrap_or("종류 전체");
    let gname = st.group.map(|g| g.label()).unwrap_or("상태 전체");
    let by_device = st.by_device;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        if multi {
            let items = [theme::Seg::text("상태별"), theme::Seg::text("기기별")];
            if let Some(i) = theme::segmented(ui, &items, usize::from(by_device), 22.0) {
                ch.by_device = Some(i == 1);
            }
        }
        let r = pill(ui, &pname);
        egui::Popup::menu(&r).show(|ui| {
            ui.set_min_width(200.0);
            if widgets::menu_item(ui, icons::LAYERS, "프로젝트 전체").clicked() {
                ch.project = Some(None);
            }
            for p in projects.iter().flat_map(|l| data::visible_projects(l)) {
                if widgets::menu_item(ui, icons::FOLDER, &p.name).clicked() {
                    ch.project = Some(Some(p.id));
                }
            }
        });
        let r = pill(ui, kname);
        egui::Popup::menu(&r).show(|ui| {
            ui.set_min_width(160.0);
            if widgets::menu_item(ui, icons::LAYERS, "종류 전체").clicked() {
                ch.kind = Some(None);
            }
            for (k, g) in
                [(Kind::Session, icons::TERMINAL), (Kind::Collab, icons::USERS), (Kind::Auto, icons::SPARKLES)]
            {
                if widgets::menu_item(ui, g, k.label()).clicked() {
                    ch.kind = Some(Some(k));
                }
            }
        });
        let r = pill(ui, gname);
        egui::Popup::menu(&r).show(|ui| {
            ui.set_min_width(180.0);
            if widgets::menu_item(ui, icons::LAYERS, "상태 전체").clicked() {
                ch.group = Some(None);
            }
            for g in Group::ORDER {
                if widgets::menu_item(ui, group_tone(g).icon(), g.label()).clicked() {
                    ch.group = Some(Some(g));
                }
            }
        });
    });
}

fn footer(app: &mut App, ui: &mut egui::Ui, go: &mut Option<Go>) {
    let ov = collab::overview(app, HostKey::Local).data;
    let text = match &ov {
        Some(o) => format!("{} · 내장 subagent는 집계 제외", runwick_core::api::activity::slots_line(o)),
        None => "실행 수 확인 중…".into(),
    };
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, c::LINE));
    egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let link_w = widgets::text_width(ui, "자원 상세", font(12.0)) + 10.0;
            ui.allocate_ui(egui::vec2((ui.available_width() - link_w).max(40.0), 18.0), |ui| {
                theme::truncated(ui, &text, font(12.0), c::TEXT_3);
            });
            let l = ui.add(
                egui::Label::new(RichText::new("자원 상세").font(font(12.0)).color(c::TEXT_2).underline())
                    .sense(Sense::click()),
            );
            if l.clicked() {
                *go = Some(Go::Automation);
            }
        });
    });
}

fn follow(app: &mut App, g: Go) {
    match g {
        Go::Peek(host, worker, asked) => collab::open_peek(app, host, worker, asked),
        Go::Result(host, request) => app.ui.right = Some(RightPanel::Result { host, request }),
        Go::Collab(host, session) => app.ui.right = Some(RightPanel::Collab { host, session }),
        Go::Open(host, session) => {
            let info =
                data::all_sessions(app, host).data.and_then(|l| l.iter().find(|s| s.session.id == session).cloned());
            if let Some(s) = info {
                let pl = place(app, host, &s.session);
                match (pl.project_id, pl.ws) {
                    (Some(p), Some(ws)) => actions::open_session(app, p, ws, session, s.session.hidden),
                    _ => app.toast_error("세션의 공간을 찾을 수 없어요"),
                }
            }
        }
        Go::Automation => actions::open_settings(app, Some(SettingsSection::Automation)),
        Go::Resume => app.ui.modal = Some(Modal::Resume),
        Go::Devices => actions::open_settings(app, Some(SettingsSection::Devices)),
    }
}
