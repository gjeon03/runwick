//! Session workers, with navigation to a result and secondary controls in the menu.

use super::{overview, requests_of};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{data, dialog, dialog_title, widgets};
use crate::theme::{self, Tone, c, font, medium};
use egui::{Margin, RichText};
use runwick_core::api::collab::*;
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::{SessionId, SessionInfo, SessionsStop, Stop};

pub fn show(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId) {
    let q = data::all_sessions(app, host);
    let Some(list) = q.data else {
        widgets::empty_note(ui, &q.error.map(|e| e.message).unwrap_or_else(|| "작업자를 불러오는 중…".into()));
        return;
    };
    let workers: Vec<_> = list.iter().filter(|s| s.session.parent_id == Some(session) && !s.session.archived).collect();
    let reqs = requests_of(app, host, session).data;
    let ov = overview(app, host).data;
    let held = ov.as_ref().is_some_and(|o| o.held_parents.contains(&session));
    egui::Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("작업자 {}", workers.len())).font(medium(13.0)).color(c::TEXT));
            let more = widgets::icon_btn(ui, icons::ELLIPSIS, "작업자 더 보기", 28.0, 28.0, 14.0);
            egui::Popup::menu(&more).show(|ui| {
                let text = if held { "배정 재개" } else { "새 배정 보류" };
                if widgets::menu_item(ui, if held { icons::PLAY } else { icons::PAUSE }, text).clicked() {
                    app.hub.send::<CollabHold>(host, Hold { parent_id: session, held: !held });
                }
                if widgets::menu_item_danger(ui, icons::CIRCLE_STOP, "작업자 모두 멈추기…").clicked() {
                    app.feat.collab.stop_all = Some((host, session));
                }
            });
        });
        if held {
            ui.label(RichText::new("새 배정을 보류하고 있어요").font(font(12.0)).color(c::TEXT_3));
        }
    });
    if workers.is_empty() {
        egui::Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
            widgets::empty_note(
                ui,
                "이 세션이 맡긴 일이 없어요. CLI에 '다른 AI에게 맡겨줘'라고 말하면 여기에 나타나요.",
            )
        });
        return;
    }
    egui::ScrollArea::vertical().id_salt(("workers", host, session)).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
            for worker in workers {
                let id = worker.session.id;
                let latest = reqs.as_ref().and_then(|l| l.iter().find(|r| r.worker_id == id));
                let live = data::live_of(app, host, worker);
                let (status, tone) = worker_status(latest.map(|r| r.status), live.as_ref());
                let answer = latest
                    .and_then(|r| app.hub.query::<CollabGet>(host, &ById { id: r.id }).data)
                    .and_then(|v| v.answer.as_deref().map(|s| super::result::first_line(s, 120)));
                let title = latest.map(|r| r.title.as_str()).unwrap_or(&worker.session.title);
                let role = worker.session.role.as_deref().unwrap_or(worker.session.tool.label());
                let row =
                    egui::Frame::new().fill(c::ROW).corner_radius(8).inner_margin(Margin::same(10)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            theme::tool_badge(ui, data::tool_mark(worker.session.tool), 24.0, c::CONTROL);
                            theme::truncated(ui, title, medium(13.0), c::TEXT_STRONG);
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(status).font(font(12.0)).color(tone.fg()));
                            theme::truncated(ui, role, font(12.0), c::TEXT_3);
                        });
                        theme::truncated(ui, answer.as_deref().unwrap_or("아직 답변이 없어요"), font(12.0), c::TEXT_2);
                    });
                let response = ui.interact(row.response.rect, ui.scope_id().with(("worker", id)), egui::Sense::click());
                response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{title} 상세 보기")));
                theme::focus_ring(ui, &response, 8.into());
                if response.clicked() {
                    super::open_worker(app, host, id);
                }
                if ov.as_ref().is_some_and(|o| o.direct_workers.contains(&id)) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("직접 대화 중이에요").font(font(11.5)).color(c::TEXT_3));
                        ui.menu_button("⋯", |ui| {
                            if ui.button("다시 맡기기").clicked() {
                                app.hub.send::<CollabDirect>(host, Direct { worker_id: id, on: false });
                                ui.close();
                            }
                        });
                    });
                }
                ui.add_space(6.0);
            }
        });
    });
}

fn worker_status(
    status: Option<ReqStatus>,
    live: Option<&runwick_core::api::sessions::SessionLive>,
) -> (&'static str, Tone) {
    if live.and_then(super::attention_words).is_some() {
        return ("확인 필요", Tone::Attention);
    }
    match status {
        Some(ReqStatus::NeedsInput | ReqStatus::Uncertain) => ("확인 필요", Tone::Attention),
        Some(ReqStatus::Failed) => ("실패", Tone::Recover),
        Some(ReqStatus::Done | ReqStatus::Cancelled) => ("끝남", Tone::Info),
        Some(ReqStatus::Running) => ("작업 중", Tone::Running),
        _ if live.is_some_and(|l| l.busy) => ("작업 중", Tone::Running),
        _ => ("대기 중", Tone::Waiting),
    }
}

/// "협업 중단": stop the workers this session started and cancel what is still queued. The
/// session itself and runs started separately keep going.
pub fn show_stop_all(app: &mut App, ctx: &egui::Context) {
    let Some((host, parent)) = app.feat.collab.stop_all else {
        return;
    };
    let list = data::all_sessions(app, host).data;
    let reqs = requests_of(app, host, parent).data;
    let running: Vec<SessionInfo> = list
        .iter()
        .flat_map(|l| l.iter())
        .filter(|s| s.session.parent_id == Some(parent))
        .filter(|s| data::status(&s.session, data::live_of(app, host, s).as_ref()).alive)
        .cloned()
        .collect();
    let queued: Vec<RequestId> =
        reqs.iter().flat_map(|l| l.iter()).filter(|r| r.status == ReqStatus::Queued).map(|r| r.id).collect();
    let mut go = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "collab-stop-all", 460.0, |ui| {
        dialog_title(
            ui,
            "협업을 중단할까요?",
            Some(
                "이 세션이 맡긴 작업자를 멈추고 아직 전달하지 않은 요청을 취소해요. 이 세션과 따로 시작한 실행은 그대로예요. 결과와 기록은 남아요.",
            ),
        );
        if running.is_empty() && queued.is_empty() {
            ui.label(RichText::new("멈출 작업자도, 대기 중인 요청도 없어요").font(font(12.5)).color(c::TEXT_3));
        }
        for s in &running {
            ui.horizontal(|ui| {
                theme::status_dot(ui, Tone::Running, true);
                ui.label(RichText::new(&s.session.title).font(medium(12.5)).color(c::TEXT));
                ui.label(
                    RichText::new(format!("{} · 실행 종료", s.session.tool.label())).font(font(12.0)).color(c::TEXT_3),
                );
            });
        }
        if !queued.is_empty() {
            ui.label(
                RichText::new(format!("대기 중인 요청 {}개 취소", queued.len())).font(font(12.5)).color(c::TEXT_2),
            );
        }
        ui.label(
            RichText::new("실제로 멈췄는지는 각 작업자 상태에서 확인할 수 있어요.").font(font(12.0)).color(c::TEXT_3),
        );
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::danger_button(ui, "협업 중단").clicked() {
                go = true;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                cancel = true;
            }
        });
    });
    if go {
        app.hub.send::<CollabHold>(host, Hold { parent_id: parent, held: true });
        for id in queued {
            app.hub.send::<CollabCancel>(host, ById { id });
        }
        for s in running {
            app.hub.send::<SessionsStop>(host, Stop { id: s.session.id, force: false });
        }
        app.feat.collab.stop_all = None;
    } else if close || cancel {
        app.feat.collab.stop_all = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_rows_use_the_five_product_states() {
        for (status, label) in [
            (ReqStatus::Queued, "대기 중"),
            (ReqStatus::Delivered, "대기 중"),
            (ReqStatus::Running, "작업 중"),
            (ReqStatus::NeedsInput, "확인 필요"),
            (ReqStatus::Uncertain, "확인 필요"),
            (ReqStatus::Done, "끝남"),
            (ReqStatus::Cancelled, "끝남"),
            (ReqStatus::Failed, "실패"),
        ] {
            assert_eq!(worker_status(Some(status), None).0, label);
        }
    }
}
