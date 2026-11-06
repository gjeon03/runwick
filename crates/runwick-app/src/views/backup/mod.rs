//! Backup and sync screens (S14), shown in Settings › 백업·동기화.
//!
//! - `sync`: GitHub sync status, actions, auto sync, last report, folders to link, history
//! - `connect`: choosing a repository (new, from the account, or a pasted URL), its preview and
//!   the first sync; also the "다른 컴퓨터의 기록 가져오기" entry from the first screen
//! - `local`: where each computer keeps its data and making per-device backups
//! - `restore`: verifying a backup, previewing it in a separate folder and activating it

mod connect;
mod local;
mod restore;
mod review;
mod sync;

use crate::App;
use crate::theme::{self, Tone, c, font, medium, semibold, ty};
use egui::{CornerRadius, RichText, Vec2};
use runwick_core::api::sync::{KindCount, SyncKind};

#[derive(Default)]
pub struct State {
    connect: Option<connect::Wizard>,
    restore: Option<restore::Dialog>,
    sync: sync::State,
    local: local::State,
}

/// "다른 컴퓨터의 기록 가져오기" (first screen and settings): choose the sync repository to
/// restore from.
pub fn open_import(app: &mut App) {
    app.feat.backup.connect = Some(connect::Wizard::new(true));
}

/// The page body (Settings › 백업·동기화).
pub fn show_page(app: &mut App, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 8.0;
    ui.label(RichText::new("백업·동기화").font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
    ui.add(
        egui::Label::new(
            RichText::new(
                "기록은 실행한 기기에 저장돼요. 다른 컴퓨터에서 이어 쓰려면 GitHub 비공개 저장소로 동기화하고, 기기마다 백업을 따로 만들 수 있어요.",
            )
            .font(font(ty::BODY))
            .color(c::TEXT_2),
        )
        .wrap(),
    );
    ui.add_space(14.0);
    sync::show(app, ui);
    ui.add_space(26.0);
    local::show(app, ui);
    ui.add_space(26.0);
    restore::entry(app, ui);
    ui.add_space(12.0);
    let ctx = ui.ctx().clone();
    connect::show(app, &ctx);
    restore::show(app, &ctx);
    sync::dialogs(app, &ctx);
}

// Shared pieces.

fn section_title(ui: &mut egui::Ui, title: &str, desc: Option<&str>) {
    ui.label(RichText::new(title).font(semibold(14.0)).color(c::TEXT_STRONG));
    if let Some(d) = desc {
        ui.add(egui::Label::new(RichText::new(d).font(font(12.5)).color(c::TEXT_2)).wrap());
    }
}

/// "1.9GB", "340MB", "12KB".
fn size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K {
        format!("{:.1}GB", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.0}MB", (b / (K * K)).max(1.0))
    } else if b >= K {
        format!("{:.0}KB", b / K)
    } else {
        format!("{bytes}B")
    }
}

fn count_of(list: &[KindCount], kind: SyncKind) -> Option<u64> {
    list.iter().find(|k| k.kind == kind).map(|k| k.count)
}

/// Counts per kind in columns ("저장소에 있음", "새로 들어옴"...). A kind missing from every
/// column is left out.
fn counts_table(ui: &mut egui::Ui, id: &str, columns: &[(&str, &[KindCount])]) {
    let kinds: Vec<SyncKind> =
        SyncKind::ALL.into_iter().filter(|k| columns.iter().any(|(_, list)| count_of(list, *k).is_some())).collect();
    if kinds.is_empty() {
        ui.label(RichText::new("기록이 없어요").font(font(12.5)).color(c::TEXT_3));
        return;
    }
    egui::Frame::new()
        .fill(c::PANEL)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new(id).num_columns(columns.len() + 1).spacing(Vec2::new(18.0, 6.0)).show(ui, |ui| {
                ui.label(RichText::new("종류").font(medium(12.0)).color(c::TEXT_3));
                for (head, _) in columns {
                    ui.label(RichText::new(*head).font(medium(12.0)).color(c::TEXT_3));
                }
                ui.end_row();
                for k in kinds {
                    ui.label(RichText::new(k.label()).font(font(12.5)).color(c::TEXT));
                    for (_, list) in columns {
                        let text = count_of(list, k).map(group).unwrap_or_else(|| "—".into());
                        ui.label(RichText::new(text).font(theme::mono(12.5)).color(c::TEXT_STRONG));
                    }
                    ui.end_row();
                }
            });
        });
}

/// 1,204
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Two boxes side by side: what is included and what is left out.
fn include_exclude(ui: &mut egui::Ui, put: &str, keep: &str) {
    let w = ((ui.available_width() - 6.0) / 2.0).max(200.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (head, body) in [("넣는 것", put), ("빼는 것", keep)] {
            egui::Frame::new()
                .fill(c::NAV)
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(w - 24.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.label(RichText::new(head).font(medium(12.0)).color(c::TEXT_3));
                        ui.add(egui::Label::new(RichText::new(body).font(font(12.5)).color(c::TEXT)).wrap());
                    });
                });
        }
    });
}

/// A status line: tone chip plus explanation.
fn status_line(ui: &mut egui::Ui, tone: Tone, chip: &str, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        theme::chip(ui, tone, chip);
        if !text.is_empty() {
            ui.label(RichText::new(text).font(font(12.5)).color(c::TEXT_2));
        }
    });
}

/// Paths of open documents with unsaved edits in this window.
fn unsaved_documents(app: &App) -> u32 {
    let docs: Vec<(crate::state::WsKey, String)> = app
        .ui
        .tabsets
        .values()
        .flat_map(|s| s.tabs.iter())
        .filter_map(|t| match &t.kind {
            crate::state::TabKind::Doc { ws, path } => Some((*ws, path.clone())),
            _ => None,
        })
        .collect();
    docs.into_iter().filter(|(ws, path)| crate::views::docs::is_dirty(app, *ws, path)).count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_counts_read_naturally() {
        assert_eq!(size(0), "0B");
        assert_eq!(size(12 * 1024), "12KB");
        assert_eq!(size(340 * 1024 * 1024), "340MB");
        assert_eq!(size(2_040_109_465), "1.9GB");
        assert_eq!(group(1204), "1,204");
        assert_eq!(group(88), "88");
        assert_eq!(group(1_000_000), "1,000,000");
    }
}
