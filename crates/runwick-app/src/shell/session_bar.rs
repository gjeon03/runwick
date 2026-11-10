//! Session bar under a terminal (34): state, tool, folder/branch, collaboration entry (only when
//! the session has workers), attention, and the session tools (기록, 핀, 새로고침, 더 보기).

use super::{actions, data, widgets};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::state::{Modal, RenameTarget, RightPanel, TabId, WsKey};
use crate::theme::{Tone, c, font, icon_font, medium};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use runwick_core::api::sessions::{SessionId, Tool};

pub fn show(app: &mut App, ui: &mut egui::Ui, ws: WsKey, host: HostKey, session: SessionId, tab: TabId) {
    let bar = ui.max_rect();
    ui.painter().rect_filled(bar, CornerRadius::ZERO, c::NAV);
    ui.painter().hline(bar.x_range(), bar.top() + 0.5, Stroke::new(1.0, c::LINE));
    let list = data::sessions_in(app, ws).data;
    let Some(info) = list.as_ref().and_then(|l| l.iter().find(|s| s.session.id == session)).cloned() else {
        ui.painter().text(
            Pos2::new(bar.left() + 12.0, bar.center().y),
            Align2::LEFT_CENTER,
            "세션 정보를 불러오는 중…",
            font(12.0),
            c::TEXT_3,
        );
        return;
    };
    let live = data::live_of(app, host, &info);
    let st = data::status(&info.session, live.as_ref());
    let workers: Vec<_> = list
        .iter()
        .flat_map(|l| l.iter())
        .filter(|s| s.session.parent_id == Some(session) && !s.session.archived)
        .cloned()
        .collect();
    let worker_attention =
        workers.iter().filter(|w| data::live_of(app, host, w).is_some_and(|l| l.attention.is_some())).count();
    let ws_branch = data::workspaces(app).data.and_then(|l| data::find_ws(&l, &ws).and_then(|w| w.branch.clone()));
    let cy = bar.center().y;

    // Right: tools.
    let pin = crate::views::history::session_pin(app, host, session, info.session.tool);
    let mut x = bar.right() - 6.0;
    let tools: [(&str, &str); 4] = [
        (icons::ELLIPSIS, "더 보기"),
        (icons::REFRESH_CW, "세션 새로고침"),
        (icons::PIN, "핀"),
        (icons::HISTORY, "이 세션 기록"),
    ];
    let mut clicked = None;
    let mut more_resp = None;
    for (i, (g, name)) in tools.iter().enumerate() {
        if i == 2 && !info.session.tool.is_agent() {
            continue;
        }
        let r = Rect::from_min_size(Pos2::new(x - 28.0, cy - 13.0), Vec2::new(28.0, 26.0));
        let resp = ui
            .scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                if i == 2 {
                    let pinned = pin.is_some_and(|(_, pinned)| pinned);
                    let tip = if pinned {
                        "마지막 응답의 핀을 해제해요"
                    } else {
                        "마지막 응답을 핀에 고정해요"
                    };
                    ui.add_enabled_ui(pin.is_some(), |ui| {
                        crate::views::history::pin_toggle_button(ui, pinned, tip, 26.0)
                    })
                    .inner
                    .on_disabled_hover_text("아직 고정할 응답이 없거나 기록을 불러오는 중이에요")
                } else {
                    widgets::icon_btn(ui, g, name, 28.0, 26.0, 14.0)
                }
            })
            .inner;
        // Ember dot on 세션 새로고침 when settings changed since this run started (S12).
        if i == 1 && crate::views::refresh::pending(app, host, session) {
            ui.painter().circle_filled(Pos2::new(r.right() - 7.0, r.top() + 6.0), 3.0, c::EMBER);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "세션 새로고침 · 적용 대기"));
        }
        if resp.clicked() {
            clicked = Some(i);
        }
        if i == 0 {
            more_resp = Some(resp);
        }
        x = r.left() - 1.0;
    }
    x -= 9.0;
    // Collaboration entry (only with workers).
    if !workers.is_empty() {
        let text = format!("협업 {}", workers.len());
        let need = if worker_attention > 0 { Some(format!("확인 필요 {worker_attention}")) } else { None };
        let tw = widgets::text_width(ui, &text, font(12.0));
        let nw = need.as_ref().map(|n| widgets::text_width(ui, n, medium(12.0)) + 6.0).unwrap_or(0.0);
        let w = tw + nw + 18.0 + 19.0;
        let r = Rect::from_min_size(Pos2::new(x - w, cy - 12.0), Vec2::new(w, 24.0));
        let resp = ui.interact(r, ui.scope_id().with(("collab", session)), Sense::click());
        let label = format!("협업 상세 열기: {text} {}", need.clone().unwrap_or_default());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        let highlight = worker_attention > 0;
        let bg = if highlight {
            c::EMBER_BG
        } else if resp.hovered() {
            c::CONTROL_HOVER
        } else {
            c::CONTROL
        };
        ui.painter().rect_filled(r, CornerRadius::same(12), bg);
        if highlight {
            ui.painter().rect_stroke(r, CornerRadius::same(12), Stroke::new(1.0, c::EMBER), egui::StrokeKind::Inside);
        }
        ui.painter().text(
            Pos2::new(r.left() + 15.5, cy),
            Align2::CENTER_CENTER,
            icons::USERS,
            icon_font(13.0),
            c::TEXT_STRONG,
        );
        ui.painter().text(Pos2::new(r.left() + 28.0, cy), Align2::LEFT_CENTER, &text, font(12.0), c::TEXT_STRONG);
        if let Some(n) = &need {
            ui.painter().text(
                Pos2::new(r.left() + 28.0 + tw + 6.0, cy),
                Align2::LEFT_CENTER,
                n,
                medium(12.0),
                c::EMBER,
            );
        }
        crate::theme::focus_ring(ui, &resp, CornerRadius::same(12));
        if resp.on_hover_text(label.clone()).clicked() {
            actions::toggle_right(app, RightPanel::Collab { host, session });
        }
        x = r.left() - 10.0;
    }
    // Attention chip.
    if let Some(a) = live.as_ref().and_then(|l| l.attention.clone()) {
        let label = data::attention_label(a.kind);
        let w = widgets::text_width(ui, label, medium(12.0)) + 30.0;
        let r = Rect::from_min_size(Pos2::new(x - w, cy - 11.0), Vec2::new(w, 22.0));
        ui.painter().rect_filled(r, CornerRadius::same(5), Tone::Attention.bg());
        ui.painter().text(
            Pos2::new(r.left() + 13.0, cy),
            Align2::CENTER_CENTER,
            Tone::Attention.icon(),
            icon_font(11.0),
            c::EMBER,
        );
        ui.painter().text(Pos2::new(r.left() + 23.0, cy), Align2::LEFT_CENTER, label, medium(12.0), c::EMBER);
        let tip = if a.text.is_empty() {
            format!("{label} · 터미널에서 답하세요")
        } else {
            format!("{label}: {}\n터미널에서 답하세요", a.text)
        };
        ui.interact(r, ui.scope_id().with(("attention", session)), Sense::hover()).on_hover_text(tip);
        x = r.left() - 10.0;
    }
    // Left: state, tool, folder · branch.
    let left = bar.left() + 12.0;
    let state_color = match st.tone {
        Tone::Neutral => c::TEXT_2,
        Tone::Waiting => c::TEXT_3,
        t => t.fg(),
    };
    widgets::dot(ui, Pos2::new(left + 3.5, cy), state_color, st.pulse, 3.5);
    let sw = widgets::text_width(ui, &st.label, font(12.0));
    ui.painter().text(Pos2::new(left + 13.0, cy), Align2::LEFT_CENTER, &st.label, font(12.0), state_color);
    let mut lx = left + 13.0 + sw + 8.0;
    let tool_label = info.session.tool.label();
    if lx + 40.0 < x {
        ui.painter().text(Pos2::new(lx, cy), Align2::LEFT_CENTER, tool_label, font(12.0), c::TEXT_3);
        lx += widgets::text_width(ui, tool_label, font(12.0)) + 8.0;
    }
    let mut meta = data::tilde(&info.session.cwd);
    if let Some(b) = &ws_branch {
        meta.push_str(&format!(" · {b}"));
    }
    if let Some(err) = live.as_ref().and_then(|l| l.error.clone()) {
        meta = err;
    }
    if lx + 20.0 < x {
        widgets::paint_clipped(
            ui,
            Rect::from_min_max(Pos2::new(lx, bar.top()), Pos2::new(x, bar.bottom())),
            &meta,
            font(12.0),
            c::TEXT_3,
            true,
        );
    }
    match clicked {
        Some(3) => crate::views::history::toggle_from_session_bar(app),
        Some(2) => {
            if let Some((message, _)) = pin {
                crate::views::history::pin_from_session_bar(app, host, session, message);
            }
        }
        Some(1) => app.ui.modal = Some(Modal::Refresh { host, session }),
        _ => {}
    }
    if let Some(resp) = more_resp {
        let alive = st.alive;
        let resumable = info.session.tool != Tool::Shell && info.session.provider_conv_id.is_some();
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(200.0);
            if widgets::menu_item(ui, icons::PENCIL, "이름 바꾸기").clicked() {
                app.ui.modal = Some(Modal::Rename { target: RenameTarget::Session { host, id: session } });
            }
            if !alive {
                if resumable && widgets::menu_item(ui, icons::ROTATE_CCW, "같은 대화 재개").clicked() {
                    actions::restart_session(app, host, session, true);
                }
                if widgets::menu_item(ui, icons::PLAY, "새로 실행")
                    .on_hover_text("새 프로세스로 시작해요. 이전 대화는 이어지지 않아요")
                    .clicked()
                {
                    actions::restart_session(app, host, session, false);
                }
            }
            widgets::menu_sep(ui);
            if alive && widgets::menu_item_danger(ui, icons::CIRCLE_STOP, "실행 종료…").clicked() {
                app.shell.modal.ask_stop(host, session);
            }
            if widgets::menu_item(ui, icons::X, "탭 닫기 (실행 유지)").clicked() {
                actions::close_tab(app, ws, tab);
            }
        });
    }
}
