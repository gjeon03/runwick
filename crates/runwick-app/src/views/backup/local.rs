//! Device backups (Backup.dc.html): where each computer keeps its data, making a backup on
//! each chosen computer's own disk while CLIs keep running, and this computer's backup list
//! with verification and restore.

use super::{include_exclude, section_title, size, status_line};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::actions;
use crate::theme::{self, Tone, c, font, icon, medium, mono, semibold, ty};
use crate::views::history::fmt;
use egui::{CornerRadius, RichText, Vec2};
use runwick_core::api::backup::*;
use runwick_core::api::devices::DevicesList;
use std::collections::{BTreeMap, BTreeSet};

enum Run {
    Making,
    Done(Box<BackupItem>),
    Failed(String),
}

#[derive(Default)]
pub struct State {
    /// Computers left out of the next backup (connected ones are chosen by default).
    off: BTreeSet<HostKey>,
    /// Folder for this computer's backups (the default folder when None).
    dir: Option<String>,
    runs: BTreeMap<HostKey, Run>,
}

/// A computer that can hold a backup.
struct Target {
    key: HostKey,
    name: String,
    connected: bool,
}

fn targets(app: &mut App) -> Vec<Target> {
    let mut out = vec![Target {
        key: HostKey::Local,
        name: app.hub.local_info().map(|i| i.device_name).unwrap_or_else(|| "이 컴퓨터".into()),
        connected: app.hub.state(HostKey::Local).is_connected(),
    }];
    if let Some(devices) = app.hub.query::<DevicesList>(HostKey::Local, &()).data {
        for d in devices.iter() {
            let key = HostKey::Device(d.id);
            out.push(Target { key, name: d.name.clone(), connected: app.hub.state(key).is_connected() });
        }
    }
    out
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let list = targets(app);
    stores(app, ui, &list);
    ui.add_space(18.0);
    make(app, ui, &list);
    ui.add_space(18.0);
    backups(app, ui);
}

fn stores(app: &mut App, ui: &mut egui::Ui, list: &[Target]) {
    section_title(ui, "기기별 저장 위치", None);
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, t) in list.iter().enumerate() {
            let info = if t.connected { app.hub.query::<BackupInfo>(t.key, &()).data } else { None };
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 9)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.allocate_ui(Vec2::new(150.0, 34.0), |ui| {
                        ui.horizontal_centered(|ui| {
                            theme::status_dot(ui, if t.connected { Tone::Running } else { Tone::Check }, false);
                            theme::truncated(ui, &t.name, medium(ty::BODY), c::TEXT_STRONG);
                        });
                    });
                    ui.vertical(|ui| {
                        ui.set_width((ui.available_width() - 90.0).max(120.0));
                        match &info {
                            Some(s) => {
                                theme::truncated(ui, &s.home, mono(12.0), c::TEXT);
                                let parts = [
                                    ("기록", s.db_bytes),
                                    ("위키", s.wiki_bytes),
                                    ("동기화 사본", s.sync_bytes),
                                    ("백업", s.backups_bytes),
                                    ("터미널 화면", s.screens_bytes),
                                ];
                                let text = parts
                                    .iter()
                                    .filter(|(_, b)| *b > 0)
                                    .map(|(n, b)| format!("{n} {}", size(*b)))
                                    .collect::<Vec<_>>()
                                    .join(" · ");
                                ui.label(
                                    RichText::new(if text.is_empty() { "아직 기록 없음".into() } else { text })
                                        .font(font(12.0))
                                        .color(c::TEXT_3),
                                );
                            }
                            None if t.connected => {
                                ui.label(RichText::new("확인하는 중…").font(font(12.0)).color(c::TEXT_3));
                            }
                            None => {
                                ui.label(
                                    RichText::new("지금 확인할 수 없어요 · 연결되어 있지 않아요")
                                        .font(font(12.0))
                                        .color(c::TEXT_3),
                                );
                            }
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (text, color) = match &info {
                            Some(s) => (
                                size(s.db_bytes + s.wiki_bytes + s.sync_bytes + s.backups_bytes + s.screens_bytes),
                                c::TEXT,
                            ),
                            None => ("확인 불가".into(), c::TEXT_3),
                        };
                        ui.label(RichText::new(text).font(mono(12.5)).color(color));
                    });
                });
            });
            if i + 1 < list.len() {
                crate::views::settings::general::divider(ui);
            }
        }
    });
}

fn make(app: &mut App, ui: &mut egui::Ui, list: &[Target]) {
    section_title(
        ui,
        "백업 만들기",
        Some("고른 기기마다 백업을 따로 만들고, 그 기기 디스크에 저장해요. 만드는 동안 CLI는 계속 실행돼요."),
    );
    let running = app.feat.backup.local.runs.values().any(|r| matches!(r, Run::Making));
    for t in list {
        let chosen = t.connected && !app.feat.backup.local.off.contains(&t.key);
        let info = if t.connected { app.hub.query::<BackupInfo>(t.key, &()).data } else { None };
        egui::Frame::new()
            .fill(c::PANEL)
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(14, 12))
            .stroke(egui::Stroke::new(1.0, if chosen && !running { c::LINE_STRONG } else { c::PANEL }))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    let mut on = chosen;
                    let r = ui.add_enabled(t.connected && !running, egui::Checkbox::without_text(&mut on));
                    let r = r.on_hover_text(format!("{} 백업", t.name));
                    if r.changed() {
                        if on {
                            app.feat.backup.local.off.remove(&t.key);
                        } else {
                            app.feat.backup.local.off.insert(t.key);
                        }
                    }
                    ui.vertical(|ui| {
                        ui.set_width((ui.available_width() - 80.0).max(160.0));
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&t.name).font(semibold(ty::BODY)).color(c::TEXT_STRONG));
                            let (st, color) = match (t.key, t.connected) {
                                (HostKey::Local, _) => ("로컬", c::TEXT_3),
                                (_, true) => ("연결됨", c::MINT),
                                (_, false) => ("연결 안 됨", c::YELLOW),
                            };
                            ui.label(RichText::new(st).font(font(12.0)).color(color));
                        });
                        ui.label(RichText::new(format!("{}에서 실행한 기록 · 핀 · 위키 · 설정 참조", t.name)).font(font(12.5)).color(c::TEXT_2));
                        let place = match (t.key, &info) {
                            (HostKey::Local, _) => app.feat.backup.local.dir.clone().map(|d| crate::shell::data::tilde(&d)).or(info.as_ref().map(|i| i.backups_dir.clone())),
                            (_, Some(i)) => Some(format!("{} (이 컴퓨터 디스크가 아니에요)", i.backups_dir)),
                            (_, None) => None,
                        };
                        if let Some(place) = place {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(format!("저장: {} · {place}", t.name)).font(font(12.0)).color(c::TEXT_3));
                                if t.key == HostKey::Local && !running && fmt::quiet_link(ui, "위치 바꾸기").clicked()
                                    && let Some(d) = rfd::FileDialog::new().set_title("백업을 저장할 폴더").pick_folder()
                                {
                                    app.feat.backup.local.dir = Some(d.to_string_lossy().to_string());
                                }
                            });
                        }
                        if !t.connected {
                            ui.label(
                                RichText::new("지금은 연결되어 있지 않아 만들 수 없어요. 그 기기의 기록은 그 기기에서만 백업해요.")
                                    .font(font(12.0))
                                    .color(c::YELLOW),
                            );
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        let est = info.as_ref().map(|i| size(i.db_bytes + i.wiki_bytes)).unwrap_or_else(|| "—".into());
                        ui.label(RichText::new(est).font(mono(12.5)).color(c::TEXT)).on_hover_text("예상 용량(압축 전)");
                    });
                });
                progress(app, ui, t);
            });
        ui.add_space(2.0);
    }
    include_exclude(ui, &INCLUDED.join(" · "), &EXCLUDED.join(" · "));
    let chosen: Vec<&Target> =
        list.iter().filter(|t| t.connected && !app.feat.backup.local.off.contains(&t.key)).collect();
    let finished = !app.feat.backup.local.runs.is_empty() && !running;
    ui.horizontal(|ui| {
        let summary = if running {
            "만드는 중이에요. 다른 작업은 그대로 해도 돼요.".to_string()
        } else if finished {
            let ok = app.feat.backup.local.runs.values().filter(|r| matches!(r, Run::Done(_))).count();
            format!("백업 {ok}개를 만들고 확인했어요. 각 기기 디스크에 있어요.")
        } else if chosen.is_empty() {
            "백업할 기기를 골라 주세요.".to_string()
        } else {
            format!(
                "백업 {}개 · {} · 각자 자기 디스크에 저장",
                chosen.len(),
                chosen.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
            )
        };
        ui.add(egui::Label::new(RichText::new(summary).font(font(12.5)).color(c::TEXT_3)).wrap());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if finished {
                if theme::secondary_button(ui, "다시 고르기").clicked() {
                    app.feat.backup.local.runs.clear();
                }
            } else {
                let label = if running {
                    "만드는 중…".to_string()
                } else {
                    format!("백업 {}개 만들기", chosen.len())
                };
                let r = ui.add_enabled_ui(!running && !chosen.is_empty(), |ui| theme::primary_button(ui, &label)).inner;
                if r.clicked() {
                    let keys: Vec<HostKey> = chosen.iter().map(|t| t.key).collect();
                    start(app, keys);
                }
            }
        });
    });
}

fn start(app: &mut App, keys: Vec<HostKey>) {
    app.feat.backup.local.runs.clear();
    for key in keys {
        let dir = if key == HostKey::Local { app.feat.backup.local.dir.clone() } else { None };
        app.feat.backup.local.runs.insert(key, Run::Making);
        app.hub.call::<BackupCreate>(key, CreateBackup { dir }, move |app, res| {
            let run = match res {
                Ok(item) => Run::Done(Box::new(item)),
                Err(e) => Run::Failed(e.message),
            };
            app.feat.backup.local.runs.insert(key, run);
        });
    }
}

fn progress(app: &mut App, ui: &mut egui::Ui, t: &Target) {
    let Some(run) = app.feat.backup.local.runs.get(&t.key) else {
        return;
    };
    let (glyph, color, title, detail) = match run {
        Run::Making => (
            icons::LOADER_CIRCLE,
            c::TEXT_2,
            format!("{}에서 만드는 중 · 스냅샷 → 묶기 → 해시 확인", t.name),
            "일관된 시점으로 묶고 있어요".to_string(),
        ),
        Run::Done(item) => {
            (icons::CIRCLE_CHECK, c::MINT, format!("{}에 백업했어요 · 해시 확인함", t.name), item.path.clone())
        }
        Run::Failed(e) => (icons::CIRCLE_ALERT, c::ROSE, format!("{}에서 만들지 못했어요", t.name), e.clone()),
    };
    let making = matches!(run, Run::Making);
    let done_path = match run {
        Run::Done(item) => Some(item.path.clone()),
        _ => None,
    };
    ui.add_space(6.0);
    egui::Frame::new()
        .fill(c::ROW)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if making {
                    ui.add(egui::Spinner::new().size(13.0).color(color));
                } else {
                    ui.label(icon(glyph, 14.0, color));
                }
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 100.0).max(120.0));
                    ui.label(RichText::new(title).font(font(12.5)).color(c::TEXT));
                    theme::truncated(ui, crate::shell::data::tilde(&detail), mono(11.5), c::TEXT_3);
                });
                if let Some(p) = done_path {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if t.key == HostKey::Local {
                            if fmt::small_button(ui, "폴더 열기").clicked() {
                                let dir = std::path::Path::new(&p)
                                    .parent()
                                    .map(|d| d.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                actions::reveal(app, &dir);
                            }
                        } else if fmt::small_button(ui, "경로 복사").clicked() {
                            let ctx = ui.ctx().clone();
                            actions::copy_path(app, &ctx, &p);
                        }
                    });
                }
            });
        });
}

fn verify_chip(ui: &mut egui::Ui, v: &Verify) {
    match v {
        Verify::Ok { at } => {
            theme::chip(ui, Tone::Running, "확인됨").on_hover_text(format!("해시 확인 {}", fmt::when_full(*at)));
        }
        Verify::Failed { problems, .. } => {
            theme::chip(ui, Tone::Recover, "손상됨").on_hover_text(problems.join("\n"));
        }
        Verify::NotChecked => {
            theme::chip(ui, Tone::Waiting, "확인 안 함");
        }
    }
}

fn backups(app: &mut App, ui: &mut egui::Ui) {
    section_title(
        ui,
        "이 컴퓨터의 백업",
        Some("백업은 만든 기기에서 복구해요. 복구하기 전에 해시를 다시 확인하고 따로 풀어서 보여 드려요."),
    );
    let q = app.hub.query::<BackupListGet>(HostKey::Local, &());
    let Some(list) = q.data else {
        match q.error {
            Some(e) => status_line(ui, Tone::Recover, "확인 불가", &e.message),
            None => status_line(ui, Tone::Waiting, "확인 중", "백업 폴더를 읽고 있어요"),
        }
        return;
    };
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if list.items.is_empty() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 12)).show(ui, |ui| {
                ui.label(
                    RichText::new(format!("아직 백업이 없어요 · {}", list.dirs.join(", ")))
                        .font(font(12.5))
                        .color(c::TEXT_3),
                );
            });
            return;
        }
        let n = list.items.len();
        for (i, it) in list.items.iter().enumerate() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(icon(icons::ARCHIVE, 14.0, c::TEXT_3));
                    ui.vertical(|ui| {
                        ui.set_width((ui.available_width() - 170.0).max(160.0));
                        ui.horizontal_wrapped(|ui| {
                            let when =
                                it.created_at.map(fmt::when_full).unwrap_or_else(|| "만든 시각 확인 불가".into());
                            ui.label(RichText::new(when).font(medium(ty::BODY)).color(c::TEXT_STRONG));
                            if let Some(d) = &it.device {
                                fmt::device_badge(ui, d, false);
                            }
                            if it.copy {
                                fmt::device_badge(ui, "다른 기기 사본", true);
                            }
                            if it.before_restore {
                                fmt::device_badge(ui, "복구 전 자동 보관", false);
                            }
                            verify_chip(ui, &it.verify);
                        });
                        theme::truncated(ui, crate::shell::data::tilde(&it.path), mono(11.5), c::TEXT_3);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = fmt::small_button(ui, "복구…");
                        crate::terminal::verify::target(&format!("backup:restore:{i}"), r.rect);
                        if r.clicked() {
                            super::restore::open(app, it.path.clone());
                        }
                        if fmt::small_button(ui, "확인").on_hover_text("해시와 내용을 다시 확인해요").clicked()
                        {
                            verify(app, it.path.clone());
                        }
                        ui.label(RichText::new(size(it.bytes)).font(mono(12.0)).color(c::TEXT));
                    });
                });
            });
            if i + 1 < n {
                crate::views::settings::general::divider(ui);
            }
        }
    });
}

fn verify(app: &mut App, path: String) {
    app.hub.call::<BackupVerify>(HostKey::Local, BackupPath { path }, |app, res| match res {
        Ok(item) => match item.verify {
            Verify::Ok { .. } => {
                app.toast(crate::state::ToastKind::Success, "백업을 확인했어요 · 모든 파일의 해시가 맞아요")
            }
            Verify::Failed { problems, .. } => app.toast_error(&format!("백업이 손상됐어요: {}", problems.join(" · "))),
            Verify::NotChecked => {}
        },
        Err(e) => app.toast_error(&e.message),
    });
}
