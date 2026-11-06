//! "가져오기와 복구": the entry rows, and restoring a backup — verify and unpack into a
//! separate folder, preview the records next to the current ones, then activate only when no
//! run is alive and no document has unsaved edits. The current data is kept as a dated backup.

use super::{counts_table, section_title, status_line};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{dialog, dialog_title};
use crate::theme::{self, Tone, c, font, icon, medium, mono, semibold, ty};
use crate::views::history::fmt;
use egui::{CornerRadius, RichText, Sense, Vec2};
use runwick_core::api::backup::*;
use runwick_core::api::sync::{SyncRun, SyncStatusGet};

const HOST: HostKey = HostKey::Local;

enum Step {
    Checking,
    Preview(Box<RestorePreview>),
    /// Keeps the preview to show again if activation is refused or fails.
    Activating(Box<RestorePreview>),
    Done(RestoreDone),
}

pub struct Dialog {
    path: String,
    step: Step,
    error: Option<String>,
    /// The person confirmed a backup this computer's signature cannot vouch for.
    accept_unsigned: bool,
}

pub fn open(app: &mut App, path: String) {
    app.feat.backup.restore =
        Some(Dialog { path: path.clone(), step: Step::Checking, error: None, accept_unsigned: false });
    app.hub.call::<BackupRestorePreview>(HOST, BackupPath { path }, |app, res| {
        let Some(d) = app.feat.backup.restore.as_mut() else {
            if let Ok(p) = res {
                app.hub.send::<BackupRestoreDiscard>(HOST, PreviewRef { id: p.id });
            }
            return;
        };
        match res {
            Ok(p) => d.step = Step::Preview(Box::new(p)),
            Err(e) => d.error = Some(e.message),
        }
    });
}

/// One row of the "가져오기와 복구" list.
fn row(ui: &mut egui::Ui, glyph: &str, title: &str, desc: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
    let t = title.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &t));
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(10), c::ROW);
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(14.0, 6.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.label(icon(glyph, 15.0, c::TEXT_2));
    child.vertical(|ui| {
        ui.label(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
        ui.label(RichText::new(desc).font(font(12.0)).color(c::TEXT_3));
    });
    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(icon(icons::CHEVRON_RIGHT, 12.0, c::TEXT_3));
    });
    theme::focus_ring(ui, &resp, CornerRadius::same(10));
    resp
}

pub fn entry(app: &mut App, ui: &mut egui::Ui) {
    section_title(ui, "가져오기와 복구", None);
    let connected = app.hub.query::<SyncStatusGet>(HOST, &()).data.is_some_and(|s| s.repo.is_some());
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        let desc = if connected {
            "연결된 저장소에서 다른 컴퓨터의 변경을 지금 받아요."
        } else {
            "GitHub 동기화 저장소에서 가져와요. 먼저 따로 받아 확인한 뒤에 가져와요."
        };
        if row(ui, icons::CLOUD_DOWNLOAD, "다른 컴퓨터의 기록 가져오기", desc).clicked() {
            if connected {
                app.hub.call::<SyncRun>(HOST, (), |app, res| match res {
                    Ok(r) => app.toast(crate::state::ToastKind::Success, super::sync::summary(&r)),
                    Err(e) => app.toast_error(&e.message),
                });
            } else {
                super::open_import(app);
            }
        }
        crate::views::settings::general::divider(ui);
        if row(
            ui,
            icons::ARCHIVE_RESTORE,
            "백업에서 복구",
            "백업을 만든 기기에서 복구해요. 따로 풀어 확인한 뒤에 바꿔요.",
        )
        .clicked()
        {
            let start = app.hub.query::<BackupInfo>(HOST, &()).data.map(|i| i.backups_dir.clone());
            let mut d = rfd::FileDialog::new().set_title("복구할 Runwick 백업").add_filter("Runwick 백업", &["gz"]);
            if let Some(s) = start {
                d = d.set_directory(expand(&s));
            }
            if let Some(f) = d.pick_file() {
                open(app, f.to_string_lossy().to_string());
            }
        }
    });
}

fn expand(p: &str) -> std::path::PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => runwick_core::paths::user_home().join(rest),
        None => std::path::PathBuf::from(p),
    }
}

enum Act {
    Activate(uuid::Uuid),
    Close,
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.feat.backup.restore.take() else {
        return;
    };
    let unsaved = super::unsaved_documents(app);
    let synced = app.hub.query::<SyncStatusGet>(HOST, &()).data.is_some_and(|s| s.repo.is_some());
    let mut acts = Vec::new();
    let (_, close) = dialog(ctx, "backup-restore", 600.0, |ui| {
        dialog_title(
            ui,
            "백업에서 복구",
            Some("백업을 따로 풀어서 확인했어요. 바꾸기 전까지 지금 데이터는 그대로예요."),
        );
        let name =
            std::path::Path::new(&d.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        theme::truncated(ui, name, mono(12.0), c::TEXT_2);
        match &d.step {
            Step::Checking if d.error.is_none() => {
                working(ui, "백업을 확인하고 있어요", "해시와 서명 확인 → 따로 풀기 → 개수 세기")
            }
            Step::Checking => {}
            Step::Preview(p) => preview(ui, p, unsaved, synced, &mut d.accept_unsigned, &mut acts),
            Step::Activating(_) => working(ui, "복구하고 있어요", "지금 데이터 보관 → 기록 바꾸기 → 색인 다시 만들기"),
            Step::Done(r) => {
                status_line(
                    ui,
                    Tone::Running,
                    "복구했어요",
                    "Runwick이 복구한 기록으로 다시 시작해요. 잠시 후 다시 연결돼요.",
                );
                ui.label(
                    RichText::new("이전 데이터는 날짜가 붙은 백업으로 남겨 두었어요").font(font(12.5)).color(c::TEXT_2),
                );
                theme::truncated(ui, crate::shell::data::tilde(&r.kept), mono(11.5), c::TEXT_3);
            }
        }
        if let Some(e) = &d.error {
            egui::Frame::new()
                .fill(c::ROSE_BG)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(icon(icons::CIRCLE_ALERT, 13.0, c::ROSE));
                        ui.label(RichText::new(e).font(font(12.5)).color(c::TEXT));
                    });
                });
        }
        if !matches!(d.step, Step::Preview(_) | Step::Activating(_)) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::secondary_button(ui, "닫기").clicked() {
                    acts.push(Act::Close);
                }
            });
        }
    });
    if close && !matches!(d.step, Step::Activating(_)) {
        acts.push(Act::Close);
    }
    for a in acts {
        match a {
            Act::Close => {
                if let Step::Preview(p) = &d.step {
                    app.hub.send::<BackupRestoreDiscard>(HOST, PreviewRef { id: p.id });
                }
                return;
            }
            Act::Activate(id) => {
                if let Step::Preview(p) = std::mem::replace(&mut d.step, Step::Checking) {
                    d.step = Step::Activating(p);
                }
                d.error = None;
                app.hub.call::<BackupRestoreActivate>(
                    HOST,
                    ActivateRestore { id, unsaved_documents: unsaved, accept_unsigned: d.accept_unsigned },
                    |app, res| {
                        let Some(d) = app.feat.backup.restore.as_mut() else {
                            return;
                        };
                        match res {
                            Ok(done) => d.step = Step::Done(done),
                            Err(e) => {
                                // The current data is still in use; show the preview again.
                                d.error = Some(e.message);
                                if let Step::Activating(p) = std::mem::replace(&mut d.step, Step::Checking) {
                                    d.step = Step::Preview(p);
                                }
                            }
                        }
                    },
                );
            }
        }
    }
    app.feat.backup.restore = Some(d);
}

fn working(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(16.0).color(c::TEXT_2));
        ui.vertical(|ui| {
            ui.label(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
            ui.label(RichText::new(detail).font(font(12.5)).color(c::TEXT_3));
        });
    });
    ui.add_space(6.0);
}

fn preview(
    ui: &mut egui::Ui,
    p: &RestorePreview,
    unsaved: u32,
    synced: bool,
    accept_unsigned: &mut bool,
    acts: &mut Vec<Act>,
) {
    ui.horizontal_wrapped(|ui| {
        theme::chip(ui, Tone::Running, "해시 확인함");
        if p.unsigned.is_none() && !p.item.copy {
            theme::chip(ui, Tone::Running, "이 컴퓨터의 서명 확인함");
        }
        let when = p.item.created_at.map(fmt::when_full).unwrap_or_else(|| "만든 시각 확인 불가".into());
        ui.label(RichText::new(when).font(semibold(ty::BODY)).color(c::TEXT_STRONG));
        if let Some(dev) = &p.item.device {
            fmt::device_badge(ui, dev, p.item.copy);
        }
    });
    counts_table(ui, "restore-counts", &[("백업", &p.in_backup), ("지금", &p.now)]);
    for k in &p.kept {
        ui.horizontal_wrapped(|ui| {
            ui.label(icon(icons::LOCK, 11.0, c::TEXT_3));
            ui.label(RichText::new(k).font(font(12.5)).color(c::TEXT_2));
        });
    }
    if !p.projects.is_empty() {
        ui.add(
            egui::Label::new(
                RichText::new(format!("백업의 프로젝트: {}", p.projects.join(", "))).font(font(12.5)).color(c::TEXT_2),
            )
            .wrap(),
        );
    }
    if let Some(why) = &p.unsigned {
        egui::Frame::new()
            .fill(c::YELLOW_BG)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(icon(icons::SHIELD, 13.0, c::YELLOW));
                    ui.label(
                        RichText::new("서명을 확인할 수 없는 백업이에요")
                            .font(medium(12.5))
                            .color(c::TEXT_STRONG),
                    );
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{why}. 이 컴퓨터에서 직접 만든 백업이 맞을 때만 복구하세요. 누군가 바꿔 둔 파일이면 기록이 그 내용으로 바뀌어요."
                        ))
                        .font(font(12.5))
                        .color(c::TEXT_2),
                    )
                    .wrap(),
                );
                ui.checkbox(accept_unsigned, "직접 만든 백업이 맞아요");
            });
    }
    let mut blockers = p.blockers.clone();
    if unsaved > 0 && !blockers.iter().any(|b| b.contains("저장하지 않은 문서")) {
        blockers.push(format!("저장하지 않은 문서가 {unsaved}개 있어요. 저장하거나 닫은 뒤에 바꿀 수 있어요"));
    }
    if blockers.is_empty() {
        egui::Frame::new().fill(c::NAV).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new("바꾸기 전에 지금 데이터를 날짜가 붙은 백업으로 먼저 보관해요. 복구한 세션은 기록으로만 보이고 자동으로 다시 실행하지 않아요. 바꾸다 실패하면 지금 데이터를 그대로 써요.")
                        .font(font(12.5))
                        .color(c::TEXT_2),
                )
                .wrap(),
            );
            if synced {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new("GitHub 동기화가 연결되어 있어요. 다음 동기화에서는 복구한 기록이 우선해요. 다른 컴퓨터에서 더 나중에 바뀐 같은 기록은 '겹친 변경'에 남기고, 복구 뒤에 다른 컴퓨터에서 새로 생긴 기록은 함께 받아요.")
                            .font(font(12.5))
                            .color(c::TEXT_2),
                    )
                    .wrap(),
                );
            }
        });
    } else {
        ui.label(RichText::new("지금은 둘러보기만 할 수 있어요").font(medium(12.5)).color(c::YELLOW));
        for b in &blockers {
            status_line(ui, Tone::Check, "보류", b);
        }
    }
    ui.add_space(4.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui
            .add_enabled_ui(blockers.is_empty() && (p.unsigned.is_none() || *accept_unsigned), |ui| {
                theme::primary_button(ui, "이 백업으로 바꾸기")
            })
            .inner
            .clicked()
        {
            acts.push(Act::Activate(p.id));
        }
        if theme::secondary_button(ui, "닫기").clicked() {
            acts.push(Act::Close);
        }
    });
}
