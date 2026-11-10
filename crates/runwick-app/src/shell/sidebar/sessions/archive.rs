//! Manual archive, restore and record access through the existing session API.

use crate::App;
use crate::client::{HostKey, Query};
use crate::shell::{modal, widgets};
use crate::state::WsKey;
use crate::theme::{self, c, font, medium};
use runwick_core::api::sessions::{SessionId, SessionInfo, SessionQuery, SessionUpdate, SessionsList, SessionsUpdate};

#[derive(Clone)]
pub(crate) enum ArchiveDialog {
    List(WsKey),
    Confirm { ws: WsKey, ids: Vec<SessionId> },
}

pub(super) fn query(app: &mut App, ws: WsKey) -> Query<Vec<SessionInfo>> {
    app.hub.query::<SessionsList>(
        ws.host,
        &SessionQuery { workspace_id: Some(ws.id), include_hidden: true, include_archived: true, ..Default::default() },
    )
}

pub(crate) fn set_archived(app: &mut App, host: HostKey, id: SessionId, archived: bool) {
    app.hub.call::<SessionsUpdate>(
        host,
        SessionUpdate { id, archived: Some(archived), ..Default::default() },
        move |app, res| {
            match res {
                Ok(s) if !archived => {
                    // A restored session is immediately discoverable without restarting its CLI.
                    app.ui.expanded_ended_sessions.insert(WsKey { host, id: s.workspace_id });
                }
                Ok(_) => {}
                Err(e) => {
                    let action = if archived { "보관하지" } else { "복원하지" };
                    app.toast_error(&format!("세션을 {action} 못했어요: {}", e.message));
                }
            }
        },
    );
}

pub(crate) fn show_archive_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(dialog) = app.shell.sidebar.archive.clone() else { return };
    match dialog {
        ArchiveDialog::Confirm { ws, ids } => confirm(app, ctx, ws, &ids),
        ArchiveDialog::List(ws) => list(app, ctx, ws),
    }
}

fn confirm(app: &mut App, ctx: &egui::Context, ws: WsKey, ids: &[SessionId]) {
    let mut archive = false;
    let mut cancel = false;
    let ((), close) = modal::dialog(ctx, "archive-ended", 420.0, |ui| {
        modal::dialog_title(
            ui,
            &format!("끝난 세션 {}개를 보관할까요?", ids.len()),
            Some("목록에서만 숨겨요. 대화 기록은 계속 검색할 수 있고, 보관 목록에서 복원할 수 있어요."),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            archive = theme::primary_button(ui, "모두 보관").clicked();
            cancel = theme::secondary_button(ui, "취소").clicked();
        });
    });
    if archive {
        for &id in ids {
            // The host rechecks each run, including sessions resumed after this dialog opened.
            set_archived(app, ws.host, id, true);
        }
    }
    if archive || cancel || close {
        app.shell.sidebar.archive = None;
    }
}

fn list(app: &mut App, ctx: &egui::Context, ws: WsKey) {
    let q = query(app, ws);
    let mut restore = None;
    let mut record = None;
    let mut done = false;
    let ((), close) = modal::dialog(ctx, "archived-sessions", 540.0, |ui| {
        modal::dialog_title(ui, "보관된 세션", Some("기록을 보거나 끝난 세션 목록으로 복원할 수 있어요."));
        egui::ScrollArea::vertical()
            .id_salt(("archive-list", ws))
            .max_height((ctx.content_rect().height() - 220.0).max(100.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let Some(error) = &q.error {
                    widgets::empty_note(ui, &format!("보관 목록을 불러오지 못했어요 · {}", error.message));
                }
                if let Some(list) = &q.data {
                    let mut archived: Vec<_> = list.iter().filter(|s| s.session.archived).collect();
                    archived.sort_by_key(|s| (s.session.created_at, s.session.id));
                    if archived.is_empty() {
                        widgets::empty_note(ui, "보관된 세션이 없어요.");
                    }
                    for s in archived {
                        ui.push_id(s.session.id, |ui| {
                            egui::Frame::new().fill(c::ROW).corner_radius(6).inner_margin(10).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let title = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&s.session.title).font(medium(13.0)).color(c::TEXT_STRONG),
                                    )
                                    .truncate(),
                                );
                                title.on_hover_text(&s.session.title);
                                let ended = s
                                    .session
                                    .ended_at
                                    .map(|at| at.with_timezone(&chrono::Local).format("%Y.%m.%d %H:%M").to_string())
                                    .unwrap_or_else(|| "시각 없음".into());
                                ui.label(
                                    egui::RichText::new(format!("{} · 종료 {ended}", s.session.tool.label()))
                                        .font(font(12.0))
                                        .color(c::TEXT_3),
                                );
                                ui.horizontal(|ui| {
                                    if theme::secondary_button(ui, "기록 보기").clicked() {
                                        record = Some(s.session.id);
                                    }
                                    if theme::secondary_button(ui, "복원").clicked() {
                                        restore = Some(s.session.id);
                                    }
                                });
                            });
                        });
                    }
                } else if q.error.is_none() {
                    widgets::empty_note(ui, "보관 목록을 불러오는 중…");
                }
            });
        done = theme::secondary_button(ui, "닫기").clicked();
    });
    if let Some(id) = restore {
        set_archived(app, ws.host, id, false);
    }
    if let Some(id) = record {
        crate::views::history::open_session_records(app, ws.host, id);
    }
    if done || close || record.is_some() {
        app.shell.sidebar.archive = None;
    }
}
