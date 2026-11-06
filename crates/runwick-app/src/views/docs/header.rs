//! Document header (file name, save state in words, 편집/미리보기, 저장 ⌘S, the path with its
//! workspace and device) and the banners for what needs the person: a disk change, a deleted
//! file, an unreachable device, a slow or failed save.

use super::model::{Body, Kind, SaveState};
use super::{Act, Entry, Place};
use crate::icons;
use crate::shell::widgets;
use crate::theme::{self, Seg, Tone, c, font, icon_font, medium, mono, semibold};
use egui::{
    Align, Align2, Color32, CornerRadius, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, UiBuilder, Vec2,
};
use runwick_core::Ts;
use runwick_core::api::files::{OpenWith, display_path, file_name};

const HEIGHT: f32 = 54.0;
/// A save slower than this gets an explanation instead of a silent spinner.
const SLOW_SAVE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirm {
    TakeDisk,
    Discard,
    /// The tab is being closed with unsaved edits: 저장 / 편집 유지 / 버리기.
    Close {
        ws: crate::state::WsKey,
        tab: crate::state::TabId,
    },
}

pub fn hhmm(ts: Ts) -> String {
    ts.with_timezone(&chrono::Local).format("%H:%M").to_string()
}

/// The save state in words, with an icon and a color.
fn status(e: &Entry, place: &Place) -> Option<(Color32, &'static str, String)> {
    let d = &e.doc;
    let s = match (&d.body, &d.save) {
        (Body::Loading, _) => (c::TEXT_3, icons::LOADER_CIRCLE, "불러오는 중…".to_string()),
        (_, SaveState::Saving { .. }) => (c::TEXT_2, icons::LOADER_CIRCLE, "저장 중…".to_string()),
        (_, SaveState::Queued) => (Tone::Direct.fg(), icons::KEYBOARD, "저장 대기 · 연결되면 확인 후 저장".to_string()),
        _ if d.missing && (d.dirty() || d.save == SaveState::Conflict) => {
            (Tone::Check.fg(), icons::TRIANGLE_ALERT, "저장 안 됨 · 디스크에서 없어진 파일".to_string())
        }
        _ if d.external.is_some() || d.save == SaveState::Conflict => {
            (Tone::Attention.fg(), icons::CIRCLE_ALERT, "저장 안 됨 · 디스크 버전이 달라요".to_string())
        }
        (_, SaveState::Failed { .. }) => (Tone::Recover.fg(), icons::CIRCLE_ALERT, "저장 안 됨".to_string()),
        _ if d.dirty() && place.remote() && !place.connected => {
            (Tone::Direct.fg(), Tone::Direct.icon(), "미저장 · 이 컴퓨터의 초안".to_string())
        }
        _ if d.dirty() && place.remote() => {
            (c::TEXT_STRONG, icons::CIRCLE_DOT, "미저장 · 이 컴퓨터에 보관 중".to_string())
        }
        _ if d.dirty() && d.restored => (Tone::Direct.fg(), icons::CIRCLE_DOT, "미저장 · 지난 편집 복원".to_string()),
        _ if d.dirty() => (c::TEXT_2, icons::CIRCLE_DOT, "미저장".to_string()),
        (_, SaveState::Saved { at }) => {
            let at = if place.remote() { format!("{} {}", place.where_(), hhmm(*at)) } else { hhmm(*at) };
            (Tone::Running.fg(), icons::CHECK, format!("저장됨 · {at}"))
        }
        _ if d.reloaded_at.is_some() => (
            Tone::Info.fg(),
            icons::REFRESH_CW,
            format!("디스크에서 바뀜 · {}", d.reloaded_at.map(hhmm).unwrap_or_default()),
        ),
        _ if d.lossy => (Tone::Check.fg(), icons::TRIANGLE_ALERT, "UTF-8이 아닌 글자가 있어 읽기 전용".to_string()),
        _ if d.readonly_file => (c::TEXT_3, icons::LOCK, "읽기 전용 파일".to_string()),
        (Body::Text, _) if d.kind == Kind::Code && !e.ui.edit_code => (c::TEXT_3, icons::EYE, "보기 전용".to_string()),
        _ => return None,
    };
    Some(s)
}

/// Save button: ember while there is something to save and no banner holds the ember.
fn save_button(ui: &mut egui::Ui, ember: bool, enabled: bool) -> egui::Response {
    let label = "저장";
    let w = widgets::text_width(ui, label, medium(12.0)) + widgets::text_width(ui, "⌘S", font(11.5)) + 26.0;
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(w, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, "저장 (⌘S)"));
    let (bg, fg, sub) = match (ember && enabled, enabled) {
        (true, _) => (c::EMBER, c::ON_EMBER, c::ON_EMBER.gamma_multiply(0.7)),
        (false, true) => (if resp.hovered() { c::CONTROL_HOVER } else { c::CONTROL }, c::TEXT_STRONG, c::TEXT_3),
        (false, false) => (c::ROW, c::TEXT_3, c::TEXT_OFF),
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    let x = rect.left() + 10.0;
    let lw = widgets::text_width(ui, label, medium(12.0));
    ui.painter().text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, label, medium(12.0), fg);
    ui.painter().text(Pos2::new(x + lw + 6.0, rect.center().y), Align2::LEFT_CENTER, "⌘S", font(11.5), sub);
    theme::focus_ring(ui, &resp, CornerRadius::same(6));
    let tip = if enabled {
        "저장 (⌘S) — 디스크 버전을 확인한 뒤 저장해요"
    } else {
        "저장할 편집이 없어요"
    };
    resp.on_hover_text(tip)
}

struct Banner {
    tone: Tone,
    glyph: &'static str,
    title: String,
    detail: String,
    action: Option<(&'static str, Act, bool)>,
    pulse: bool,
}

fn banners(e: &Entry, place: &Place) -> Vec<Banner> {
    let d = &e.doc;
    let mut v = Vec::new();
    if place.remote() && !place.connected {
        let detail = if d.save == SaveState::Queued {
            "편집은 이 컴퓨터의 초안으로 보관 중이에요. 다시 연결되면 디스크 버전이 그대로인지 확인한 뒤 저장해요."
        } else if d.dirty() {
            "편집은 이 컴퓨터의 초안으로 보관 중이에요. 다시 연결되면 저장할 수 있어요."
        } else {
            "보이는 내용은 마지막으로 확인한 버전이에요. 다시 연결되면 최신 내용을 확인해요."
        };
        v.push(Banner {
            tone: Tone::Direct,
            glyph: icons::WIFI_OFF,
            title: format!("{}에 연결되어 있지 않아요", place.where_()),
            detail: detail.into(),
            action: None,
            pulse: false,
        });
    }
    if let Some(ext) = &d.external {
        let seen = hhmm(ext.seen);
        match (&ext.hash, &ext.text) {
            (Some(_), Some(_)) => v.push(Banner {
                tone: Tone::Attention,
                glyph: icons::CIRCLE_ALERT,
                title: "다른 프로그램이 이 파일을 바꿨어요 · 내 편집은 그대로예요".into(),
                detail: format!("CLI나 다른 앱이 디스크의 파일을 바꿨어요({seen}에 확인). 자동으로 덮어쓰지 않았어요."),
                action: Some(("비교하기", Act::Compare, true)),
                pulse: false,
            }),
            (Some(_), None) => v.push(Banner {
                tone: Tone::Attention,
                glyph: icons::CIRCLE_ALERT,
                title: "디스크의 파일이 텍스트로 읽을 수 없게 바뀌었어요".into(),
                detail: format!("{seen}에 확인. 내 편집은 그대로예요. 내 편집으로 저장하면 디스크의 파일을 바꿔요."),
                action: Some(("내 편집으로 저장", Act::SaveOver, false)),
                pulse: false,
            }),
            (None, _) => {}
        }
    }
    if d.missing && d.is_text() {
        v.push(Banner {
            tone: Tone::Check,
            glyph: icons::TRIANGLE_ALERT,
            title: if d.dirty() {
                "디스크에서 이 파일이 없어졌어요 · 내 편집은 그대로예요".into()
            } else {
                "디스크에서 이 파일이 없어졌어요".into()
            },
            detail: "지워졌거나 이름이 바뀌었을 수 있어요. 저장하면 이 내용으로 다시 만들어요.".into(),
            action: Some(("다시 만들기", Act::Save, false)),
            pulse: false,
        });
    }
    if let SaveState::Saving { since } = d.save
        && since.elapsed() >= SLOW_SAVE
    {
        v.push(Banner {
            tone: Tone::Waiting,
            glyph: icons::HOURGLASS,
            title: format!("{}에 저장하는 중이에요", place.where_()),
            detail: format!(
                "응답이 늦어요 · {}초째. 저장이 확인되기 전에는 저장됨으로 표시하지 않아요. 편집은 그대로 있어요.",
                since.elapsed().as_secs()
            ),
            action: None,
            pulse: true,
        });
    }
    if let SaveState::Failed { message } = &d.save {
        v.push(Banner {
            tone: Tone::Recover,
            glyph: icons::CIRCLE_ALERT,
            title: "저장하지 못했어요 · 편집은 그대로 있어요".into(),
            detail: message.clone(),
            action: Some(("다시 저장", Act::Save, false)),
            pulse: false,
        });
    }
    if d.restored && d.dirty() && d.verified && d.external.is_none() && !d.missing {
        v.push(Banner {
            tone: Tone::Direct,
            glyph: icons::HISTORY,
            title: "저장하지 않은 지난 편집을 복원했어요".into(),
            detail: "앱을 닫기 전의 편집이에요. 디스크 버전이 그대로인 것을 확인했어요. 저장하거나 계속 편집하세요."
                .into(),
            action: None,
            pulse: false,
        });
    }
    v
}

fn draw_banner(ui: &mut egui::Ui, b: Banner, acts: &mut Vec<Act>) {
    let w = ui.available_width();
    let fg = b.tone.fg();
    let resp = egui::Frame::new()
        .fill(b.tone.bg())
        .inner_margin(Margin { left: 14, right: 10, top: 9, bottom: 9 })
        .show(ui, |ui| {
            ui.set_width(w - 24.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 9.0;
                let mut color = fg;
                if b.pulse && !theme::reduce_motion() {
                    let t = ui.input(|i| i.time) as f32;
                    color = color.gamma_multiply(0.55 + 0.45 * (t * 2.2).sin().abs());
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
                } else if b.pulse {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                }
                ui.label(theme::icon(b.glyph, 14.0, color));
                let action_w =
                    b.action.as_ref().map(|(t, _, _)| widgets::text_width(ui, t, medium(13.0)) + 30.0).unwrap_or(0.0);
                let text_w = (ui.available_width() - action_w).max(120.0);
                ui.allocate_ui(Vec2::new(text_w, 0.0), |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.vertical(|ui| {
                        let title_color = if b.tone == Tone::Attention {
                            Color32::from_rgb(0xFF, 0xD9, 0xBF)
                        } else {
                            c::TEXT_STRONG
                        };
                        ui.add(
                            egui::Label::new(RichText::new(&b.title).font(semibold(12.5)).color(title_color)).wrap(),
                        );
                        if !b.detail.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&b.detail).font(font(12.0)).color(c::TEXT_2)).wrap());
                        }
                    });
                });
                if let Some((text, act, ember)) = b.action {
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        let clicked =
                            if ember { theme::primary_button(ui, text) } else { theme::secondary_button(ui, text) };
                        if clicked.clicked() {
                            acts.push(act);
                        }
                    });
                }
            });
        });
    let r = resp.response.rect;
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, c::LINE));
}

pub fn show(ui: &mut egui::Ui, e: &mut Entry, place: &Place, acts: &mut Vec<Act>) {
    let list = banners(e, place);
    let banner_ember = list.iter().any(|b| b.action.as_ref().is_some_and(|a| a.2));
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, HEIGHT), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, c::LINE));
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + 14.0, rect.top() + 8.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 8.0),
    );
    let controls = ui
        .scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            controls(ui, e, place, !banner_ember, acts);
            ui.min_rect()
        })
        .inner;
    let left =
        Rect::from_min_max(inner.min, Pos2::new((controls.left() - 12.0).max(inner.left() + 40.0), inner.bottom()));
    title_block(ui, left, e, place);
    for b in list {
        draw_banner(ui, b, acts);
    }
}

fn title_block(ui: &mut egui::Ui, left: Rect, e: &Entry, place: &Place) {
    let name = display_path(file_name(&e.doc.path)).to_string();
    let top_y = left.top() + 9.0;
    let name_w = widgets::text_width(ui, &name, medium(13.0));
    let st = status(e, place);
    let st_w = st.as_ref().map(|(_, _, t)| widgets::text_width(ui, t, font(12.0)) + 22.0).unwrap_or(0.0);
    let name_room = (left.width() - st_w).max(left.width() * 0.45).min(name_w);
    let name_rect = Rect::from_min_size(Pos2::new(left.left(), top_y - 9.0), Vec2::new(name_room, 18.0));
    widgets::paint_clipped(ui, name_rect, &name, medium(13.0), c::TEXT_STRONG, true);
    if let Some((color, glyph, text)) = st {
        let x = name_rect.right() + 8.0;
        let room = left.right() - x - 16.0;
        if room > 24.0 {
            ui.painter().text(Pos2::new(x + 6.0, top_y), Align2::CENTER_CENTER, glyph, icon_font(11.0), color);
            let r = Rect::from_min_size(Pos2::new(x + 15.0, top_y - 9.0), Vec2::new(room, 18.0));
            widgets::paint_clipped(ui, r, &text, font(12.0), color, true);
            ui.interact(r, ui.scope_id().with("doc-status"), Sense::hover()).on_hover_text(text);
        }
    }
    // Second line: device · workspace · relative path.
    let y = left.top() + 28.0;
    let mut x = left.left();
    if let Some(dev) = &place.device {
        x += widgets::device_badge(ui, Pos2::new(x, y), dev, !place.connected) + 7.0;
    }
    let rel = display_path(&e.doc.path).to_string();
    let line = format!("{} · {rel}", place.ws_name);
    let r = Rect::from_min_max(Pos2::new(x, y - 8.0), Pos2::new(left.right(), y + 8.0));
    widgets::paint_clipped(ui, r, &line, mono(11.0), c::TEXT_3, true);
    let full = match (place.absolute(&e.doc.path), &place.device) {
        (Some(abs), Some(dev)) => format!("{dev}: {abs}"),
        (Some(abs), None) => crate::shell::data::tilde(&abs),
        (None, _) => rel,
    };
    ui.interact(r, ui.scope_id().with("doc-path"), Sense::hover()).on_hover_text(full);
}

fn controls(ui: &mut egui::Ui, e: &mut Entry, place: &Place, ember_free: bool, acts: &mut Vec<Act>) {
    let d = &e.doc;
    let can_save = d.is_text() && !d.lossy && !d.saving() && (d.dirty() || d.missing || d.save == SaveState::Queued);
    let show_save = d.is_text() && (can_save || super::editor::editable(e));
    if show_save && save_button(ui, ember_free && can_save, can_save).clicked() {
        acts.push(Act::Save);
    }
    let more = widgets::icon_btn(ui, icons::ELLIPSIS, "더 보기", 26.0, 26.0, 14.0);
    more_menu(&more, e, place, acts);
    let copy = widgets::icon_btn(ui, icons::COPY, "경로 복사", 26.0, 26.0, 13.0);
    copy_menu(&copy, e, place, acts);
    if !e.doc.is_text() {
        return;
    }
    match e.doc.kind {
        Kind::Markdown => {
            let sel = usize::from(e.ui.preview);
            if let Some(i) = theme::segmented(ui, &[Seg::text("미리보기"), Seg::text("편집")], 1 - sel, 24.0) {
                e.ui.preview = i == 0;
                if !e.ui.preview {
                    e.ui.focus_editor = true;
                }
            }
        }
        Kind::Code => {
            let sel = usize::from(e.ui.edit_code);
            let editable = !e.doc.lossy && !e.doc.readonly_file && e.doc.text.len() <= super::editor::EDIT_MAX;
            if editable && let Some(i) = theme::segmented(ui, &[Seg::text("편집"), Seg::text("보기")], 1 - sel, 24.0)
            {
                e.ui.edit_code = i == 0;
                e.ui.focus_editor = e.ui.edit_code;
            }
        }
        Kind::Text => {}
    }
}

fn copy_menu(resp: &egui::Response, e: &Entry, place: &Place, acts: &mut Vec<Act>) {
    egui::Popup::menu(resp).show(|ui| {
        ui.set_min_width(260.0);
        let rel = e.doc.path.clone();
        if copy_option(ui, "작업 공간 기준 경로", &display_path(&rel), "같은 공간의 CLI에 붙여 넣기 좋아요.")
        {
            acts.push(Act::Copy(rel.clone()));
        }
        if let Some(abs) = place.absolute(&rel) {
            let note = match &place.device {
                Some(dev) => format!("{dev} 안에서만 맞는 경로예요."),
                None => "이 컴퓨터의 실제 위치예요.".to_string(),
            };
            if copy_option(ui, "실제 경로", &abs, &note) {
                acts.push(Act::Copy(abs));
            }
        }
    });
}

fn copy_option(ui: &mut egui::Ui, title: &str, value: &str, note: &str) -> bool {
    let w = ui.available_width().max(260.0);
    let resp = egui::Frame::new()
        .fill(c::ROW)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(w - 20.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(medium(12.5)).color(c::TEXT_STRONG));
            theme::truncated(ui, value, mono(11.5), c::TEXT);
            ui.label(RichText::new(note).font(font(12.0)).color(c::TEXT_3));
        })
        .response;
    let resp = ui.interact(resp.rect, ui.scope_id().with(("copy-opt", title)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MenuItem, true, title));
    if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect,
            CornerRadius::same(8),
            Stroke::new(1.0, c::LINE_STRONG),
            egui::StrokeKind::Inside,
        );
    }
    if resp.clicked() {
        ui.close();
        return true;
    }
    false
}

fn more_menu(resp: &egui::Response, e: &mut Entry, place: &Place, acts: &mut Vec<Act>) {
    egui::Popup::menu(resp).show(|ui| {
        ui.set_min_width(220.0);
        if e.doc.is_text() && widgets::menu_item(ui, icons::SEARCH, "문서에서 찾기  ⌘F").clicked() {
            e.ui.preview = false;
            e.ui.find.open();
        }
        if e.doc.is_text() && e.doc.kind != Kind::Code {
            let label = if e.ui.proportional { "고정폭 글꼴로 보기" } else { "본문 글꼴로 보기" };
            if widgets::menu_item(ui, icons::TYPE, label).clicked() {
                e.ui.proportional = !e.ui.proportional;
            }
        }
        if !place.remote() {
            widgets::menu_sep(ui);
            if widgets::menu_item(ui, icons::FOLDER_OPEN, "Finder에서 보기").clicked() {
                acts.push(Act::Reveal);
            }
            if widgets::menu_item(ui, icons::EXTERNAL_LINK, "외부 에디터로 열기").clicked() {
                acts.push(Act::OpenExternal(OpenWith::Editor));
            }
        }
        if e.doc.dirty() {
            widgets::menu_sep(ui);
            if widgets::menu_item_danger(ui, icons::UNDO_2, "편집 버리기…").clicked() {
                e.ui.confirm = Some(Confirm::Discard);
            }
        }
    });
}

/// Confirmation before throwing away local edits.
pub fn confirm(ctx: &egui::Context, e: &mut Entry, acts: &mut Vec<Act>) {
    let Some(kind) = e.ui.confirm else { return };
    if let Confirm::Close { ws, tab } = kind {
        return confirm_close(ctx, e, ws, tab, acts);
    }
    let (title, desc, button) = match kind {
        Confirm::TakeDisk => (
            "디스크 내용을 받을까요?",
            "내 편집을 버리고 지금 디스크에 있는 내용으로 바꿔요. 버린 편집은 되돌릴 수 없어요.",
            "편집 버리고 받기",
        ),
        _ => (
            "저장하지 않은 편집을 버릴까요?",
            "마지막으로 불러온 디스크 버전으로 돌아가요. 버린 편집은 되돌릴 수 없어요.",
            "편집 버리기",
        ),
    };
    let mut choice = None;
    let (_, close) = crate::shell::dialog(ctx, "doc-confirm", 420.0, |ui| {
        crate::shell::dialog_title(ui, title, Some(desc));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::danger_button(ui, button).clicked() {
                choice = Some(true);
            }
            if theme::secondary_button(ui, "취소").clicked() {
                choice = Some(false);
            }
        });
    });
    if close || choice.is_some() {
        e.ui.confirm = None;
    }
    if choice == Some(true) {
        acts.push(if kind == Confirm::TakeDisk { Act::TakeDisk } else { Act::Discard });
    }
}

/// Closing a tab never drops edits silently: save, keep editing (the tab stays), or discard.
fn confirm_close(
    ctx: &egui::Context,
    e: &mut Entry,
    ws: crate::state::WsKey,
    tab: crate::state::TabId,
    acts: &mut Vec<Act>,
) {
    let name = display_path(file_name(&e.doc.path)).to_string();
    let conflict = e.doc.external.is_some();
    let desc = if conflict {
        "디스크의 파일도 바뀌어서 바로 저장할 수 없어요. 저장을 고르면 비교 화면으로 가요. 편집 유지를 고르면 탭을 닫지 않아요."
    } else {
        "닫기 전에 저장할까요? 편집 유지를 고르면 탭을 닫지 않아요. 버린 편집은 되돌릴 수 없어요."
    };
    let mut choice = None;
    let (_, close) = crate::shell::dialog(ctx, "doc-close", 460.0, |ui| {
        crate::shell::dialog_title(ui, &format!("{name}에 저장하지 않은 편집이 있어요"), Some(desc));
        ui.horizontal(|ui| {
            if theme::danger_button(ui, "버리기").clicked() {
                choice = Some(Act::DiscardAndClose(ws, tab));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::primary_button(ui, "저장").clicked() {
                    choice = Some(Act::SaveAndClose(ws, tab));
                }
                if theme::secondary_button(ui, "편집 유지").clicked() {
                    choice = Some(Act::Edited);
                }
            });
        });
    });
    if close || choice.is_some() {
        e.ui.confirm = None;
    }
    if let Some(act) = choice {
        acts.push(act);
    }
}
