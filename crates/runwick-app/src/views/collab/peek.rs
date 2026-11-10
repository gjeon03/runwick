//! S05 작업자 미리보기 (WorkerPeek.dc.html) over the center: the worker's real terminal, read-only
//! until `직접 대화` (then automatic instructions wait, violet), `탭에 유지` (opens it in its own
//! workspace) and close (the run continues). Opened from an attention entry that was already
//! answered in the terminal, it says so, shows the current screen and sends nothing (S02 Stale).

use super::{attention_key, look_live, overview, place};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, data, widgets};
use crate::state::{Page, RightPanel};
use crate::theme::{c, font, icon, icon_font, medium, mono, semibold, size};
use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
use runwick_core::api::collab::{CollabDirect, CollabList, Direct, ReqStatus, RequestQuery};
use runwick_core::api::sessions::{Attention, Phase, SessionId};

#[derive(Debug, Clone)]
pub struct Peek {
    pub host: HostKey,
    pub worker: SessionId,
    /// The prompt the person clicked on (from the activity list), to tell whether it is gone.
    stale: Option<(String, i64)>,
    focused: bool,
}

/// Open the peek for a worker. `asked` is the prompt shown where the person clicked, if any.
pub fn open(app: &mut App, host: HostKey, worker: SessionId, asked: Option<Attention>) {
    app.feat.collab.peek = Some(Peek { host, worker, stale: asked.as_ref().map(attention_key), focused: false });
}

fn close(app: &mut App) {
    app.feat.collab.peek = None;
}

enum Act {
    Direct,
    Release,
    Keep,
    Close,
    Result(uuid::Uuid),
    BackToActivity,
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(p) = app.feat.collab.peek.clone() else {
        return;
    };
    let Some(center) = app.shell.center.rect.filter(|_| app.ui.page == Page::Work) else {
        return;
    };
    let q = data::all_sessions(app, p.host);
    let Some(info) = q.data.as_ref().and_then(|l| l.iter().find(|s| s.session.id == p.worker)).cloned() else {
        if q.data.is_some() {
            close(app);
        }
        return;
    };
    let live = data::live_of(app, p.host, &info);
    let direct = overview(app, p.host).data.is_some_and(|o| o.direct_workers.contains(&p.worker));
    let reqs = app
        .hub
        .query::<CollabList>(p.host, &RequestQuery { worker_id: Some(p.worker), limit: 20, ..Default::default() })
        .data;
    let latest = reqs.as_ref().and_then(|l| l.first().cloned());
    let waiting = reqs.iter().flat_map(|l| l.iter()).filter(|r| r.status == ReqStatus::Queued).count();
    let place = place(app, p.host, &info.session);
    let (state, tone) = match &latest {
        Some(r) => {
            let lk = look_live(r, direct, live.as_ref());
            (lk.label, lk.tone)
        }
        None => {
            let st = data::status(&info.session, live.as_ref());
            (st.label, st.tone)
        }
    };
    let alive = live.as_ref().is_some_and(|l| matches!(l.phase, Phase::Running | Phase::Starting));
    let stale = p
        .stale
        .as_ref()
        .is_some_and(|k| live.as_ref().and_then(|l| l.attention.as_ref()).map(attention_key).as_ref() != Some(k));
    let title = latest.as_ref().map(|r| r.title.clone()).unwrap_or_else(|| info.session.title.clone());

    let dim = Rect::from_min_max(
        Pos2::new(center.left(), center.top() + size::TABS),
        Pos2::new(center.right(), center.bottom() - size::SESSION_BAR),
    );
    let panel = Rect::from_min_max(dim.min + Vec2::new(24.0, 24.0), dim.max - Vec2::new(24.0, 24.0));
    if panel.width() < 200.0 || panel.height() < 160.0 {
        return;
    }
    let mut acts: Vec<Act> = Vec::new();
    let peek_id = Id::unique(("collab-peek", p.worker));
    egui::Area::new(Id::unique("collab-peek-layer")).order(egui::Order::Middle).fixed_pos(dim.min).show(ctx, |ui| {
        ui.set_min_size(dim.size());
        ui.set_max_size(dim.size());
        // Click on the dim closes (only while previewing); clicks on the panel stay inside.
        let bg = ui.interact(dim, peek_id.with("dim"), Sense::click());
        let inside = ui.interact(panel, peek_id, Sense::click());
        inside.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Window, true, "작업자 터미널 미리보기"));
        ui.painter().rect_filled(dim, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(8, 9, 11, 158));
        if bg.clicked() && !direct {
            acts.push(Act::Close);
        }
        let shadow = egui::epaint::Shadow { offset: [0, 24], blur: 64, spread: 0, color: Color32::from_black_alpha(140) };
        ui.painter().add(shadow.as_shape(panel, CornerRadius::same(12)));
        ui.painter().rect_filled(panel, CornerRadius::same(12), c::TERMINAL);
        let ring = if direct { c::VIOLET } else { c::LINE_STRONG };

        // Header.
        let head = Rect::from_min_size(panel.min, Vec2::new(panel.width(), 54.0));
        ui.painter().rect_filled(
            head,
            CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 },
            c::PANEL,
        );
        let buttons_w = 300.0;
        let text_rect = Rect::from_min_max(head.min + Vec2::new(14.0, 9.0), Pos2::new(head.right() - buttons_w, head.bottom() - 8.0));
        let state_w = widgets::text_width(ui, &state, medium(12.0));
        let title_rect = Rect::from_min_size(text_rect.min, Vec2::new((text_rect.width() - state_w - 10.0).max(40.0), 18.0));
        let cut = widgets::paint_clipped(ui, title_rect, &title, semibold(13.0), c::TEXT_STRONG, true);
        let title_w = widgets::text_width(ui, &title, semibold(13.0)).min(title_rect.width());
        ui.painter().text(
            Pos2::new(title_rect.left() + title_w + 8.0, title_rect.center().y),
            Align2::LEFT_CENTER,
            &state,
            medium(12.0),
            tone.fg(),
        );
        if cut {
            ui.interact(title_rect, peek_id.with("title"), Sense::hover()).on_hover_text(&title);
        }
        let where_ = format!("{} · {}", info.session.tool.label(), place.line());
        let where_rect = Rect::from_min_size(Pos2::new(text_rect.left(), title_rect.bottom() + 2.0), Vec2::new(text_rect.width(), 16.0));
        if widgets::paint_clipped(ui, where_rect, &where_, mono(11.0), c::TEXT_3, true) {
            ui.interact(where_rect, peek_id.with("where"), Sense::hover()).on_hover_text(&where_);
        }
        let btn_rect = Rect::from_min_max(Pos2::new(head.right() - buttons_w, head.top()), head.max - Vec2::new(10.0, 0.0));
        ui.scope_builder(UiBuilder::new().max_rect(btn_rect).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let tip = "닫기 — 실행은 유지됩니다 (Esc)";
            if widgets::icon_btn(ui, icons::X, tip, 30.0, 30.0, 13.0).clicked() {
                acts.push(Act::Close);
            }
            let keep = egui::Button::new(RichText::new("탭에 유지").font(font(12.5)).color(c::TEXT_STRONG))
                .fill(c::CONTROL)
                .min_size(Vec2::new(0.0, 30.0))
                .corner_radius(CornerRadius::same(7));
            if ui.add(keep).on_hover_text("작업자가 속한 공간으로 전환해 탭으로 열어요").clicked() {
                acts.push(Act::Keep);
            }
            let (label, act, hint) = if direct {
                ("다시 맡기기", Act::Release, "자동 지시를 다시 전달해요. 내가 한 대화는 이 시도에 연결돼요")
            } else {
                ("직접 대화", Act::Direct, "입력권을 가져와 이 작업자와 직접 대화해요. 그동안 자동 지시는 대기해요")
            };
            let primary = egui::Button::new(RichText::new(label).font(medium(12.5)).color(c::ON_EMBER))
                .fill(c::EMBER)
                .min_size(Vec2::new(0.0, 30.0))
                .corner_radius(CornerRadius::same(7));
            let enabled = alive || direct;
            let resp = ui.add_enabled(enabled, primary);
            let resp = if enabled { resp.on_hover_text(hint) } else { resp.on_disabled_hover_text("실행이 끝나 직접 대화할 수 없어요") };
            if resp.clicked() {
                acts.push(act);
            }
        });

        // Footer.
        let foot = Rect::from_min_max(Pos2::new(panel.left(), panel.bottom() - 36.0), panel.max);
        let (fbg, ffg, glyph, text) = if direct {
            let w = if waiting > 0 { format!("자동 지시 {waiting}건은 다시 맡길 때까지 대기합니다.") } else { "자동 지시는 다시 맡길 때까지 대기합니다.".into() };
            (c::VIOLET_BG, Color32::from_rgb(0xD9, 0xCC, 0xFF), icons::KEYBOARD, format!("입력이 이 작업자에게 전달됩니다. {w}"))
        } else {
            (c::PANEL, c::TEXT_2, icons::EYE, "미리보기 중입니다. 지금 누르는 키는 작업자에게 전달되지 않아요. 답하려면 직접 대화를 누르세요.".to_string())
        };
        ui.painter().rect_filled(foot, CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 }, fbg);
        ui.painter().text(Pos2::new(foot.left() + 21.0, foot.center().y), Align2::CENTER_CENTER, glyph, icon_font(13.0), ffg);
        widgets::paint_clipped(
            ui,
            Rect::from_min_max(Pos2::new(foot.left() + 34.0, foot.top()), foot.max - Vec2::new(12.0, 0.0)),
            &text,
            font(12.5),
            ffg,
            true,
        );

        // Stale prompt notice.
        let mut body = Rect::from_min_max(Pos2::new(panel.left(), head.bottom()), Pos2::new(panel.right(), foot.top()));
        if stale {
            let bar = Rect::from_min_size(body.min, Vec2::new(body.width(), 58.0));
            ui.painter().rect_filled(bar, CornerRadius::ZERO, c::ROW);
            ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, c::LINE));
            ui.scope_builder(UiBuilder::new().max_rect(bar.shrink2(Vec2::new(12.0, 8.0))), |ui| {
                ui.horizontal(|ui| {
                    ui.label(icon(icons::CLOCK, 15.0, c::TEXT_2));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("이 승인 요청은 이미 끝났어요").font(medium(13.0)).color(c::TEXT_STRONG));
                        ui.label(
                            RichText::new("터미널에서 직접 응답되어 더 답할 수 없습니다. 아무것도 전송하지 않았고, 아래는 현재 화면입니다.")
                                .font(font(12.0))
                                .color(c::TEXT_2),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new("활동으로 돌아가기").font(font(12.5))).clicked() {
                            acts.push(Act::BackToActivity);
                        }
                        if let Some(r) = latest.as_ref().filter(|r| !r.status.is_open()) {
                            let b = egui::Button::new(RichText::new("결과 보기").font(medium(12.5)).color(c::ON_EMBER)).fill(c::EMBER);
                            if ui.add(b).clicked() {
                                acts.push(Act::Result(r.id));
                            }
                        }
                    });
                });
            });
            body.min.y = bar.bottom();
        }

        // The worker's terminal.
        let mut child = ui.new_child(UiBuilder::new().max_rect(body.shrink2(Vec2::new(1.0, 0.0))).id_salt(("peek-term", p.worker)));
        child.set_clip_rect(body);
        if direct {
            crate::terminal::show_tab(app, &mut child, p.host, p.worker, 0);
        } else {
            crate::terminal::show_readonly(app, &mut child, p.host, p.worker);
        }
        ui.painter().rect_stroke(panel, CornerRadius::same(12), Stroke::new(1.0, ring), egui::StrokeKind::Outside);
    });

    // While previewing, keys must not reach the terminal behind the peek.
    if !direct && !p.focused {
        ctx.memory_mut(|m| m.request_focus(peek_id));
        if let Some(pk) = app.feat.collab.peek.as_mut() {
            pk.focused = true;
        }
    }
    if !direct && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        acts.push(Act::Close);
    }
    for a in acts {
        match a {
            Act::Close => close(app),
            Act::Direct => {
                super::direct(app, p.host, p.worker);
                close(app);
            }
            Act::Release => {
                app.hub.send::<CollabDirect>(p.host, Direct { worker_id: p.worker, on: false });
                close(app);
            }
            Act::Keep => match (place.project_id, place.ws) {
                (Some(project), Some(ws)) => {
                    actions::open_session(app, project, ws, p.worker, info.session.hidden);
                    close(app);
                }
                _ => app.toast_error("작업자의 공간을 찾을 수 없어요"),
            },
            Act::Result(id) => {
                super::open_result(app, p.host, id);
                close(app);
            }
            Act::BackToActivity => {
                app.ui.show_panel_root(RightPanel::Activity, None);
                close(app);
            }
        }
    }
}
