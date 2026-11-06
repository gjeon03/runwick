//! What sync asks the person to look at: changes made on two computers where one side's
//! content was not kept ("겹친 변경", with that content and a way to put it back), and every
//! environment variable value or removal from another computer that differs from this one
//! ("검토 후 적용"; variables that can run programs get a stronger warning).

use super::status_line;
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::actions;
use crate::state::TabKind;
use crate::theme::{self, Tone, c, font, icon, medium, mono, ty};
use crate::views::history::fmt;
use egui::{CornerRadius, RichText};
use runwick_core::api::sync::*;

const HOST: HostKey = HostKey::Local;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    staged(app, ui);
    conflicts(app, ui);
}

fn staged(app: &mut App, ui: &mut egui::Ui) {
    let q = app.hub.query::<SyncStagedVars>(HOST, &());
    let Some(list) = q.data.filter(|l| !l.is_empty()) else {
        if let Some(e) = q.error {
            status_line(ui, Tone::Recover, "확인 불가", &e.message);
        }
        return;
    };
    ui.add_space(6.0);
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(icon(icons::SHIELD, 14.0, c::YELLOW));
            ui.label(RichText::new(format!("검토를 기다리는 환경변수 {}", list.len())).font(medium(ty::BODY)).color(c::TEXT_STRONG));
        });
        ui.add(
            egui::Label::new(
                RichText::new("다른 컴퓨터에서 바꾼 공통 환경변수와 '겹친 변경'에서 올린 값은 이 컴퓨터에서 바로 쓰지 않아요. 내용을 확인하고 정해 주세요. 쓰지 않아도 다른 컴퓨터의 값은 그대로예요. 이 컴퓨터에서 직접 바꾸거나 지우면 그 변경이 우선해요.")
                    .font(font(ty::SMALL))
                    .color(c::TEXT_3),
            )
            .wrap(),
        );
        for v in list.iter() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 190.0).max(160.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&v.key).font(mono(12.5)).color(c::TEXT_STRONG));
                        fmt::device_badge(ui, &v.device, false);
                        if v.rejected {
                            theme::chip(ui, Tone::Waiting, "쓰지 않기로 함");
                        }
                    });
                    if v.dangerous {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(icon(icons::TRIANGLE_ALERT, 12.0, c::ROSE));
                            ui.label(RichText::new("프로그램 실행에 영향을 주는 변수예요. 값을 꼭 확인하세요").font(font(ty::SMALL)).color(c::ROSE));
                        });
                    }
                    if v.remove {
                        ui.label(RichText::new("다른 컴퓨터에서 지웠어요").font(font(12.5)).color(c::TEXT));
                    } else {
                        theme::truncated(ui, &v.value, mono(12.0), c::TEXT);
                    }
                    if !v.description.is_empty() {
                        ui.label(RichText::new(&v.description).font(font(ty::SMALL)).color(c::TEXT_3));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (yes, no) = if v.remove { ("이 컴퓨터에서도 지우기", "그대로 두기") } else { ("이 컴퓨터에서 쓰기", "쓰지 않기") };
                    if !v.rejected && fmt::small_button(ui, no).clicked() {
                        decide(app, &v.key, false, v.remove);
                    }
                    if fmt::small_button(ui, yes).clicked() {
                        decide(app, &v.key, true, v.remove);
                    }
                });
            });
        }
    });
}

fn decide(app: &mut App, key: &str, accept: bool, remove: bool) {
    let key = key.to_string();
    let p = StagedDecision { key: key.clone(), accept };
    app.hub.call::<SyncDecideStaged>(HOST, p, move |app, res| match (res, accept, remove) {
        (Ok(()), true, true) => {
            app.toast_info(&format!("{} 이 컴퓨터에서도 지웠어요", runwick_core::josa::eul_reul(&key)))
        }
        (Ok(()), true, false) => app.toast(
            crate::state::ToastKind::Success,
            format!("{} 이 컴퓨터에서 써요. 새로 시작하는 실행부터 적용돼요", runwick_core::josa::eul_reul(&key)),
        ),
        (Ok(()), false, true) => {
            app.toast_info(&format!("{} 이 컴퓨터에 그대로 둬요", runwick_core::josa::eun_neun(&key)))
        }
        (Ok(()), false, false) => {
            app.toast_info(&format!("{} 이 컴퓨터에서 쓰지 않아요", runwick_core::josa::eun_neun(&key)))
        }
        (Err(e), _, _) => app.toast_error(&e.message),
    });
}

fn conflicts(app: &mut App, ui: &mut egui::Ui) {
    let q = app.hub.query::<SyncConflicts>(HOST, &());
    let Some(list) = q.data.filter(|l| !l.is_empty()) else {
        if let Some(e) = q.error {
            status_line(ui, Tone::Recover, "확인 불가", &e.message);
        }
        return;
    };
    ui.add_space(6.0);
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(icon(icons::GIT_COMPARE, 14.0, c::YELLOW));
            ui.label(RichText::new(format!("겹친 변경 {}", list.len())).font(medium(ty::BODY)).color(c::TEXT_STRONG));
        });
        ui.add(
            egui::Label::new(
                RichText::new("두 컴퓨터에서 같은 기록을 바꿔 한쪽 내용만 남은 경우예요. 남지 않은 내용은 여기 그대로 있고, 되돌리면 새 변경으로 다시 동기화돼요. 되돌리면서 바뀌는 내용도 여기에 남아요. 환경변수는 바로 바꾸지 않고 검토 목록에 올려요.")
                    .font(font(ty::SMALL))
                    .color(c::TEXT_3),
            )
            .wrap(),
        );
        for item in list.iter() {
            ui.add_space(4.0);
            egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    theme::chip(ui, Tone::Check, item.kind.label());
                    theme::truncated(ui, &item.title, medium(12.5), c::TEXT_STRONG);
                    ui.label(RichText::new(fmt::when_full(item.at)).font(font(11.5)).color(c::TEXT_3));
                });
                let who = if item.lost_device == item.kept_device {
                    format!("{}에서 되돌리기 전 내용이에요", item.kept_device)
                } else if item.lost_here {
                    format!("이 컴퓨터의 변경이 {}의 변경으로 바뀌었어요", item.kept_device)
                } else if !item.found_here {
                    format!(
                        "{}에서 알려 온 내용이에요 · {}의 변경 대신 {} 쪽을 뒀어요",
                        item.found_on, item.lost_device, item.kept_device
                    )
                } else {
                    format!("{}의 변경 대신 {} 쪽을 뒀어요", item.lost_device, item.kept_device)
                };
                ui.label(RichText::new(who).font(font(12.5)).color(c::TEXT_2));
                egui::CollapsingHeader::new(RichText::new("남지 않은 내용 보기").font(font(12.0)).color(c::TEXT_2))
                    .id_salt(("sync-conflict", item.id))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(160.0).id_salt(("sync-conflict-text", item.id)).show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(&item.lost_text).font(mono(12.0)).color(c::TEXT)).wrap());
                        });
                    });
                ui.horizontal(|ui| {
                    let label = if item.kind == SyncKind::Env { "검토 목록에 올리기" } else { "이 내용으로 되돌리기" };
                    if item.can_restore && fmt::small_button(ui, label).clicked() {
                        restore(app, item.id, item.kind);
                    }
                    if let (Some(project), Some(slug)) = (item.project_id, item.slug.clone())
                        && fmt::small_button(ui, "문서와 이전 버전 보기").clicked()
                    {
                        actions::open_center_tab(app, TabKind::Wiki { project, page: Some(slug) });
                    }
                    if fmt::quiet_link(ui, "닫기").on_hover_text("모든 컴퓨터의 목록에서 지워요. 기록은 그대로예요").clicked() {
                        app.hub.send::<SyncConflictDismiss>(HOST, ConflictRef { id: item.id });
                    }
                });
            });
        }
    });
}

fn restore(app: &mut App, id: uuid::Uuid, kind: SyncKind) {
    app.hub.call::<SyncConflictRestore>(HOST, ConflictRef { id }, move |app, res| match res {
        Ok(()) if kind == SyncKind::Env => {
            app.toast_info("검토 목록에 올렸어요. 위에서 값을 확인하고 이 컴퓨터에서 쓸지 정해 주세요")
        }
        Ok(()) => app.toast(
            crate::state::ToastKind::Success,
            "남지 않았던 내용으로 되돌렸어요. 바뀌기 전 내용은 '겹친 변경'에 남겨 두었어요",
        ),
        Err(e) => app.toast_error(&e.message),
    });
}
