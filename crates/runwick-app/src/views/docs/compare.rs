//! When the disk changed under unsaved edits: three columns (기준 버전 / 내 편집 / 디스크)
//! with what each side changed, then a choice. Nothing is written until the person picks, and
//! every save re-checks the latest disk version first. 합쳐서 편집 opens an editable merge.

use super::header::{Confirm, hhmm};
use super::model::conflict_markers;
use super::{Act, Entry, Place, View, editor};
use crate::theme::{self, Tone, c, font, medium, mono, semibold};
use egui::{Align, CornerRadius, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2};
use runwick_core::api::files::{display_path, file_name};
use runwick_core::josa;
use similar::{ChangeTag, TextDiff};
use std::hash::{Hash, Hasher};

const FOOTER_H: f32 = 56.0;
const LINE_H: f32 = 22.0;
const COL_MIN: f32 = 240.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Same,
    Added,
    Removed,
}

struct Column {
    name: &'static str,
    sub: String,
    where_: String,
    lines: Vec<(String, Mark)>,
    tone: Tone,
}

pub(crate) struct Cache {
    key: u64,
    cols: [Column; 3],
}

fn key_of(e: &Entry) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    e.doc.base.hash(&mut h);
    e.doc.text.hash(&mut h);
    e.doc.external.as_ref().map(|x| (&x.hash, &x.text)).hash(&mut h);
    h.finish()
}

fn diff_lines(old: &str, new: &str) -> Vec<(String, Mark)> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|ch| {
            let text = ch.value().trim_end_matches(['\n', '\r']).to_string();
            let mark = match ch.tag() {
                ChangeTag::Equal => Mark::Same,
                ChangeTag::Insert => Mark::Added,
                ChangeTag::Delete => Mark::Removed,
            };
            (text, mark)
        })
        .collect()
}

fn build(e: &Entry, place: &Place) -> Option<Cache> {
    let disk = e.doc.external.as_ref()?;
    let disk_text = disk.text.as_ref()?;
    let base_lines = e.doc.base.lines().map(|l| (l.to_string(), Mark::Same)).collect();
    let here = if place.remote() { "이 컴퓨터".to_string() } else { String::new() };
    let there = place.device.clone().unwrap_or_default();
    let changed_at = disk.mtime.map(hhmm).unwrap_or_else(|| hhmm(disk.seen));
    Some(Cache {
        key: key_of(e),
        cols: [
            Column {
                name: "기준 버전",
                sub: format!("내가 편집을 시작한 시점 · {}", hhmm(e.doc.base_at)),
                where_: there.clone(),
                lines: base_lines,
                tone: Tone::Neutral,
            },
            Column {
                name: "내 편집",
                sub: "미저장 · 앱을 닫아도 복원됩니다".into(),
                where_: here,
                lines: diff_lines(&e.doc.base, &e.doc.text),
                tone: Tone::Direct,
            },
            Column {
                name: if place.remote() { "지금 기기에 있는 내용" } else { "디스크 내용" },
                sub: format!("{changed_at}에 바뀜"),
                where_: there,
                lines: diff_lines(&e.doc.base, disk_text),
                tone: Tone::Running,
            },
        ],
    })
}

fn column(ui: &mut egui::Ui, col: &Column, width: f32) {
    // Columns may sit in a row: lay out each one top to bottom.
    ui.allocate_ui_with_layout(Vec2::new(width, 0.0), Layout::top_down(Align::Min), |ui| column_body(ui, col, width));
}

fn column_body(ui: &mut egui::Ui, col: &Column, width: f32) {
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(10)).show(ui, |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.set_width(width - 28.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new(col.name).font(medium(13.0)).color(c::TEXT_STRONG));
                if !col.where_.is_empty() {
                    egui::Frame::new()
                        .fill(c::CONTROL)
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(Margin::symmetric(5, 1))
                        .show(ui, |ui| ui.label(RichText::new(&col.where_).font(font(11.0)).color(c::TEXT)));
                }
            });
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(&col.sub).font(font(12.0)).color(c::TEXT_3)).wrap());
        });
        let (line_rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
        ui.painter().hline(line_rect.x_range(), line_rect.center().y, Stroke::new(1.0, c::LINE));
        ui.add_space(10.0);
        let wrap_w = width - 28.0;
        for (text, mark) in &col.lines {
            let (fg, bg) = match mark {
                Mark::Same => (c::TEXT_2, None),
                Mark::Added if col.tone == Tone::Direct => {
                    (egui::Color32::from_rgb(0xD9, 0xCC, 0xFF), Some(c::VIOLET_BG))
                }
                Mark::Added => (egui::Color32::from_rgb(0x7E, 0xE0, 0xA8), Some(c::MINT_BG)),
                Mark::Removed => (c::TEXT_OFF, Some(c::ROSE_BG)),
            };
            let shown = if *mark == Mark::Removed {
                format!("− {text}")
            } else if text.is_empty() {
                " ".into()
            } else {
                text.clone()
            };
            let mut job = egui::text::LayoutJob::single_section(
                shown,
                egui::TextFormat {
                    font_id: mono(12.5),
                    color: fg,
                    strikethrough: if *mark == Mark::Removed { Stroke::new(1.0, c::TEXT_OFF) } else { Stroke::NONE },
                    ..Default::default()
                },
            );
            job.wrap.max_width = wrap_w;
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let h = galley.size().y.max(LINE_H);
            let (r, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
            if ui.is_rect_visible(r) {
                if let Some(bg) = bg {
                    ui.painter().rect_filled(r, CornerRadius::ZERO, bg);
                }
                ui.painter().galley(Pos2::new(r.left() + 14.0, r.center().y - galley.size().y / 2.0), galley, fg);
            }
        }
        ui.add_space(10.0);
    });
}

fn footer(ui: &mut egui::Ui, rect: Rect, note: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.painter().rect_filled(rect, CornerRadius::ZERO, c::NAV);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, c::LINE));
    let inner = rect.shrink2(Vec2::new(20.0, 12.0));
    ui.scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        add(ui);
        let room = ui.available_width();
        if room > 140.0 {
            crate::theme::truncated(ui, note, font(12.5), c::TEXT_3);
        }
    });
}

fn heading(ui: &mut egui::Ui, title: &str, detail: &str) {
    egui::Frame::new().inner_margin(Margin { left: 20, right: 20, top: 18, bottom: 14 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add(egui::Label::new(RichText::new(title).font(semibold(18.0)).color(c::TEXT_STRONG)).wrap());
        ui.add(egui::Label::new(RichText::new(detail).font(font(13.0)).color(c::TEXT_2)).wrap());
    });
}

pub fn show_compare(ui: &mut egui::Ui, e: &mut Entry, place: &Place, acts: &mut Vec<Act>) {
    if e.ui.compare.as_ref().is_none_or(|c| c.key != key_of(e)) {
        e.ui.compare = build(e, place);
    }
    let Some(cache) = e.ui.compare.as_ref() else {
        // Nothing to compare any more (the disk went back, or is not text): back to the document.
        e.ui.view = View::Doc;
        return;
    };
    let full = ui.max_rect();
    let foot = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - FOOTER_H), full.max);
    let body = Rect::from_min_max(full.min, Pos2::new(full.right(), foot.top()));
    let name = display_path(file_name(&e.doc.path)).to_string();
    ui.scope_builder(UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)), |ui| {
        let title = match &place.device {
            Some(dev) => format!("저장하기 전에 {dev}의 {} 바뀌었어요", josa::i_ga(&name)),
            None => format!("저장하기 전에 {} 바뀌었어요", josa::i_ga(&name)),
        };
        heading(
            ui,
            &title,
            "어떤 쪽을 남길지 정할 때까지 아무것도 덮어쓰지 않습니다. 내 편집은 이 컴퓨터에 그대로 있어요.",
        );
        egui::ScrollArea::vertical()
            .id_salt(("doc-compare", e.doc.ws, e.doc.path.as_str()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new().inner_margin(Margin { left: 20, right: 20, top: 0, bottom: 16 }).show(ui, |ui| {
                    let avail = ui.available_width();
                    if avail >= COL_MIN * 3.0 + 16.0 {
                        let w = (avail - 16.0) / 3.0;
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            for col in &cache.cols {
                                column(ui, col, w);
                            }
                        });
                    } else {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        for col in &cache.cols {
                            column(ui, col, avail);
                        }
                    }
                });
            });
    });
    footer(
        ui,
        foot,
        "선택한 내용을 저장하기 직전에 최신 파일 버전을 한 번 더 확인합니다.",
        |ui| {
            if theme::primary_button(ui, "합쳐서 편집")
                .on_hover_text("겹치지 않는 변경은 자동으로 합치고, 겹치는 곳은 표시해요")
                .clicked()
                && let Some((merged, _)) = e.doc.merge()
            {
                e.ui.merge = Some(merged);
                e.ui.view = View::Merge;
                e.ui.focus_editor = true;
            }
            if theme::secondary_button(ui, "내 편집으로 저장")
                .on_hover_text("디스크의 변경 대신 내 편집을 저장해요")
                .clicked()
            {
                acts.push(Act::SaveOver);
            }
            if theme::secondary_button(ui, "디스크 내용 사용").clicked() {
                e.ui.confirm = Some(Confirm::TakeDisk);
            }
            if theme::secondary_button(ui, "계속 편집").clicked() {
                e.ui.view = View::Doc;
            }
        },
    );
}

pub fn show_merge(ui: &mut egui::Ui, e: &mut Entry, place: &Place, acts: &mut Vec<Act>) {
    if e.ui.merge.is_none() || e.doc.external.is_none() {
        e.ui.view = View::Doc;
        return;
    }
    let full = ui.max_rect();
    let foot = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - FOOTER_H), full.max);
    let body = Rect::from_min_max(full.min, Pos2::new(full.right(), foot.top()));
    let markers = e.ui.merge.as_deref().map(conflict_markers).unwrap_or(0);
    let saving = e.doc.saving();
    ui.scope_builder(UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)), |ui| {
        let detail = if markers > 0 {
            format!(
                "겹치는 곳 {markers}곳은 <<<<<<< 내 편집 과 >>>>>>> 디스크 표시 사이에서 남길 내용을 고르고 표시를 지워 주세요. 표시가 모두 없어지면 저장할 수 있어요."
            )
        } else {
            "겹치지 않는 변경은 자동으로 합쳤어요. 내용을 확인한 뒤 저장하세요.".to_string()
        };
        heading(ui, "두 변경을 합쳐 편집해요", &detail);
        let look = editor::Look { kind: e.doc.kind, lang: super::model::Kind::language(&e.doc.path), proportional: false, editing: true };
        let id = egui::Id::unique(("doc-merge", e.doc.ws, e.doc.path.as_str()));
        if let Some(text) = e.ui.merge.as_mut() {
            let mut no_find = editor::Find::default();
            editor::text_area(ui, id, text, !saving, &look, &mut no_find, &mut e.ui.focus_editor);
        }
    });
    let note = match &place.device {
        Some(dev) => format!("저장하기 직전에 {dev}의 최신 버전을 한 번 더 확인해요."),
        None => "저장하기 직전에 디스크 버전을 한 번 더 확인해요.".to_string(),
    };
    footer(ui, foot, &note, |ui| {
        let ready = markers == 0 && !saving;
        let label = if saving { "저장 중…" } else { "합친 내용 저장" };
        let resp = ui.add_enabled_ui(ready, |ui| theme::primary_button(ui, label)).inner;
        let resp = if markers > 0 {
            resp.on_disabled_hover_text("충돌 표시(<<<<<<<, >>>>>>>)를 정리하면 저장할 수 있어요")
        } else {
            resp
        };
        if resp.clicked() {
            acts.push(Act::SaveMerge);
        }
        if theme::secondary_button(ui, "비교로 돌아가기").clicked() {
            e.ui.view = View::Compare;
        }
    });
}
