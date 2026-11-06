//! Settings › 일반: interface and terminal text size, reduced motion, Option as Meta, the
//! external editor, stop confirmation, what closing the window does (and the menu bar icon on
//! macOS), and the shortcuts. Stored on the host
//! (`prefs.*`) and applied at once ([`on_frame`] keeps every window in step); other modules
//! read the current values through [`terminal_font_size`], [`option_as_meta`] and
//! [`confirm_stop`].

use crate::App;
use crate::client::HostKey;
use crate::theme::{self, c, font, medium, mono, semibold, ty};
use egui::{CornerRadius, RichText, Sense, Stroke, Vec2};
use runwick_core::api::prefs::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[derive(Default)]
pub struct State {
    /// Values shown while a save is on its way (so controls do not jump back).
    draft: Option<Prefs>,
    /// External editor command being typed (`None` = showing the saved one).
    editor_text: Option<String>,
    editor_saving: bool,
    editor_error: Option<String>,
    /// Menu bar choice shown while its save is on its way.
    menubar_draft: Option<bool>,
}

static TERMINAL_FONT: AtomicU32 = AtomicU32::new(0);
static OPTION_AS_META: AtomicBool = AtomicBool::new(true);
static CONFIRM_STOP: AtomicBool = AtomicBool::new(true);

/// Terminal text size in points.
pub fn terminal_font_size() -> f32 {
    match TERMINAL_FONT.load(Ordering::Relaxed) {
        0 => Prefs::default().terminal_font_size,
        bits => f32::from_bits(bits),
    }
}

/// The Option key sends Meta (Esc prefix) in terminals.
pub fn option_as_meta() -> bool {
    OPTION_AS_META.load(Ordering::Relaxed)
}

/// Ask before stopping a run.
pub fn confirm_stop() -> bool {
    CONFIRM_STOP.load(Ordering::Relaxed)
}

/// Make the preferences take effect in this window.
pub fn apply(ctx: &egui::Context, p: &Prefs) {
    if (ctx.zoom_factor() - p.ui_scale).abs() > 0.001 {
        ctx.set_zoom_factor(p.ui_scale);
    }
    theme::set_reduce_motion(p.reduce_motion);
    TERMINAL_FONT.store(p.terminal_font_size.to_bits(), Ordering::Relaxed);
    OPTION_AS_META.store(p.option_as_meta, Ordering::Relaxed);
    CONFIRM_STOP.store(p.confirm_stop, Ordering::Relaxed);
}

/// Apply the saved preferences (call once per frame). Reads the cached `prefs.get`, which is
/// fetched again whenever settings change on the host, so this costs a cache lookup.
pub fn on_frame(app: &mut App, ctx: &egui::Context) {
    let p = match app.feat.settings.general.draft {
        Some(d) => d,
        None => match app.hub.query::<PrefsGet>(HostKey::Local, &()).data {
            Some(p) => *p,
            None => return,
        },
    };
    apply(ctx, &p);
}

fn save(app: &mut App, ctx: &egui::Context, p: Prefs) {
    let p = p.sanitized();
    apply(ctx, &p);
    app.feat.settings.general.draft = Some(p);
    app.hub.call::<PrefsSet>(HostKey::Local, p, |app, res| {
        app.feat.settings.general.draft = None;
        if let Err(e) = res {
            app.toast_error(&format!("설정을 저장하지 못했어요: {}", e.message));
        }
    });
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, "일반", "글자 크기와 움직임, 터미널 키, 창을 닫을 때의 동작이에요. 이 컴퓨터에 저장돼요.");
    let q = app.hub.query::<PrefsGet>(HostKey::Local, &());
    let saved = match (&app.feat.settings.general.draft, &q.data) {
        (Some(d), _) => *d,
        (None, Some(p)) => **p,
        (None, None) => {
            let text = match &q.error {
                Some(e) => format!("설정을 불러오지 못했어요: {}", e.message),
                None => "설정을 불러오는 중…".into(),
            };
            ui.label(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3));
            return;
        }
    };
    let ctx = ui.ctx().clone();
    apply(&ctx, &saved);
    let mut p = saved;

    group(ui, "화면", |ui| {
        row(ui, "화면 크기", "모든 글자와 여백을 함께 키우거나 줄여요.", |ui| {
            let labels: Vec<String> = UI_SCALES.iter().map(|s| format!("{}%", (s * 100.0).round())).collect();
            let items: Vec<theme::Seg> = labels.iter().map(|l| theme::Seg::text(l)).collect();
            let sel = UI_SCALES.iter().position(|s| (s - p.ui_scale).abs() < 0.01).unwrap_or(1);
            // The row's controls are laid out right to left; keep the sizes in reading order.
            let w: f32 =
                labels.iter().map(|l| crate::shell::widgets::text_width(ui, l, font(ty::SMALL)) + 18.0).sum::<f32>()
                    + 4.0;
            let picked = ui
                .allocate_ui_with_layout(Vec2::new(w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    theme::segmented(ui, &items, sel, 26.0)
                })
                .inner;
            if let Some(i) = picked {
                p.ui_scale = UI_SCALES[i];
            }
        });
        divider(ui);
        row(
            ui,
            "터미널 글자 크기",
            "터미널 글자만 바꿔요. 크기가 바뀌면 줄 수와 칸 수가 다시 계산돼요.",
            |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if theme::icon_button(ui, crate::icons::PLUS, "터미널 글자 크게").clicked() {
                    p.terminal_font_size += 0.5;
                }
                ui.label(
                    RichText::new(format!("{:.1}pt", p.terminal_font_size)).font(mono(12.5)).color(c::TEXT_STRONG),
                );
                if theme::icon_button(ui, crate::icons::MINUS, "터미널 글자 작게").clicked() {
                    p.terminal_font_size -= 0.5;
                }
            },
        );
        divider(ui);
        row(ui, "움직임 줄이기", "실행 중 표시가 깜박이지 않고 그대로 보여요.", |ui| {
            toggle(ui, &mut p.reduce_motion, "움직임 줄이기");
        });
    });
    group(ui, "터미널", |ui| {
        row(
            ui,
            "Option 키를 Meta로 사용",
            "켜면 Option+B, Option+F 같은 단축키가 셸과 CLI로 가요. 끄면 Option으로 특수 문자를 입력해요.",
            |ui| {
                toggle(ui, &mut p.option_as_meta, "Option 키를 Meta로 사용");
            },
        );
    });
    group(ui, "파일", |ui| external_editor(app, ui));
    group(ui, "실행과 종료", |ui| {
        row(ui, "실행 종료 전에 확인", "실행 중인 CLI를 멈추기 전에 한 번 더 물어요.", |ui| {
            toggle(ui, &mut p.confirm_stop, "실행 종료 전에 확인");
        });
        divider(ui);
        ui.vertical(|ui| {
            ui.label(RichText::new("창을 닫으면").font(medium(ty::BODY)).color(c::TEXT_STRONG));
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "창을 닫아도 실행 중인 CLI와 기록 수집은 계속돼요. 다시 열면 같은 화면으로 이어집니다. \
                         실행까지 모두 끝내려면 명령 팔레트(⌘K)의 ‘작업까지 종료’를 쓰세요.",
                    )
                    .font(font(12.5))
                    .color(c::TEXT_2),
                )
                .wrap(),
            );
        });
        if cfg!(target_os = "macos") {
            divider(ui);
            menubar_row(app, ui);
        }
    });
    if p != saved {
        save(app, &ctx, p);
    }
    shortcuts(ui);
}

/// Whether the menu bar icon stays after the window closes (host kv, missing = on; macOS).
fn menubar_row(app: &mut App, ui: &mut egui::Ui) {
    use crate::presence::{MENUBAR_ENABLED_KEY, menubar_enabled};
    use runwick_core::api::host::{KvGet, KvKey, KvPut, KvSet};
    let key = KvKey { key: MENUBAR_ENABLED_KEY.into() };
    let q = app.hub.query::<KvGet>(HostKey::Local, &key);
    let saved = q.data.as_deref().map(|v| menubar_enabled(v.as_ref()));
    let shown = app.feat.settings.general.menubar_draft.or(saved);
    let mut changed = None;
    row(
        ui,
        "창을 닫아도 메뉴 막대에 표시",
        "실행 중인 작업이 있을 때 확인할 일과 실행 수를 보여 줘요",
        |ui| match shown {
            Some(mut on) => {
                if toggle(ui, &mut on, "창을 닫아도 메뉴 막대에 표시") {
                    changed = Some(on);
                }
            }
            None => {
                let text = if q.error.is_some() { "확인 불가" } else { "확인하는 중…" };
                ui.label(RichText::new(text).font(font(12.0)).color(c::TEXT_3));
            }
        },
    );
    if let Some(on) = changed {
        app.feat.settings.general.menubar_draft = Some(on);
        let p = KvSet { key: MENUBAR_ENABLED_KEY.into(), value: serde_json::Value::Bool(on) };
        app.hub.call::<KvPut>(HostKey::Local, p, |app, res| {
            app.feat.settings.general.menubar_draft = None;
            if let Err(e) = res {
                app.toast_error(&format!("설정을 저장하지 못했어요: {}", e.message));
            }
        });
    }
}

/// "외부 에디터": the command the files panel's "외부 에디터로 열기" runs (host `files.editor`).
fn external_editor(app: &mut App, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.label(RichText::new("외부 에디터").font(medium(ty::BODY)).color(c::TEXT_STRONG));
    ui.add(
        egui::Label::new(
            RichText::new(
                "파일의 ‘외부 에디터로 열기’에 쓸 명령이에요 (예: code, zed, subl -w처럼 창이 있는 에디터). 비워 두면 시스템 텍스트 편집기로 열어요.",
            )
            .font(font(12.0))
            .color(c::TEXT_3),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    let q = app.hub.query::<PrefsEditor>(HostKey::Local, &());
    let Some(saved) = q.data.map(|d| (*d).clone()) else {
        let text = match &q.error {
            Some(e) => format!("에디터 설정을 불러오지 못했어요: {}", e.message),
            None => "에디터 설정을 불러오는 중…".into(),
        };
        ui.label(RichText::new(text).font(font(12.0)).color(c::TEXT_3));
        return;
    };
    let st = &mut app.feat.settings.general;
    let mut text = st.editor_text.clone().unwrap_or_else(|| saved.command.clone());
    let saving = st.editor_saving;
    let mut save = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let field = ui.add_enabled(
            !saving,
            egui::TextEdit::singleline(&mut text)
                .font(mono(12.5))
                .hint_text("비워 두면 시스템 텍스트 편집기")
                .desired_width((ui.available_width() - 150.0).max(160.0))
                .margin(egui::Margin::symmetric(10, 6)),
        );
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let changed = text.trim() != saved.command;
        let label = if saving { "확인하는 중…" } else { "저장" };
        let b = ui.add_enabled_ui(changed && !saving, |ui| theme::secondary_button(ui, label)).inner;
        save = (b.clicked() || enter) && changed && !saving;
        if changed && !saving && theme::link_button(ui, "되돌리기").clicked() {
            text = saved.command.clone();
        }
    });
    let st = &mut app.feat.settings.general;
    let edited = (text != saved.command).then(|| text.clone());
    if edited != st.editor_text {
        // A new attempt: the previous error no longer describes what is typed.
        st.editor_error = None;
        st.editor_text = edited;
    }
    if let Some(e) = &st.editor_error {
        ui.label(RichText::new(e).font(font(12.0)).color(c::ROSE));
    } else if let Some(p) = &saved.problem {
        ui.label(RichText::new(p).font(font(12.0)).color(c::ROSE));
    } else if st.editor_text.is_none() {
        let note = if saved.command.is_empty() {
            "지금은 시스템 텍스트 편집기로 열어요".to_string()
        } else {
            "로그인 셸의 PATH에서 찾은 전체 경로로 저장했어요".to_string()
        };
        ui.label(RichText::new(note).font(font(12.0)).color(c::TEXT_3));
    }
    if save {
        st.editor_saving = true;
        st.editor_error = None;
        app.hub.call::<PrefsSetEditor>(HostKey::Local, SetEditor { command: text }, |app, res| {
            let st = &mut app.feat.settings.general;
            st.editor_saving = false;
            match res {
                Ok(_) => st.editor_text = None,
                Err(e) => st.editor_error = Some(e.message),
            }
        });
    }
}

const SHORTCUTS: &[(&str, &str)] = &[
    ("⌘K", "명령 팔레트"),
    ("⌘T", "새 CLI 세션"),
    ("⌘O", "폴더 열기"),
    ("⌘⇧F", "대화 검색"),
    ("⌘,", "설정"),
    ("⌘W", "탭 닫기 (실행은 계속)"),
    ("⌘1 – ⌘9", "탭 이동 (⌘9는 마지막 탭)"),
    ("⌘F", "터미널 화면에서 찾기 (터미널에 초점이 있을 때)"),
    ("Esc", "열린 창 닫기 (터미널에서는 CLI로 전달)"),
];

fn shortcuts(ui: &mut egui::Ui) {
    group(ui, "단축키", |ui| {
        egui::Grid::new("shortcuts").num_columns(2).spacing(Vec2::new(16.0, 8.0)).show(ui, |ui| {
            for (k, what) in SHORTCUTS {
                theme::kbd(ui, k);
                ui.label(RichText::new(*what).font(font(ty::BODY)).color(c::TEXT));
                ui.end_row();
            }
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Pieces shared by the settings sections of this track

/// Page title and one-line description.
pub fn header(ui: &mut egui::Ui, title: &str, desc: &str) {
    ui.spacing_mut().item_spacing.y = 4.0;
    ui.label(RichText::new(title).font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
    ui.add(egui::Label::new(RichText::new(desc).font(font(ty::BODY)).color(c::TEXT_2)).wrap());
    ui.add_space(14.0);
}

/// A titled card.
pub fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.label(RichText::new(title).font(medium(12.0)).color(c::TEXT_3));
    egui::Frame::new()
        .fill(c::PANEL)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            add(ui);
        });
    ui.add_space(14.0);
}

/// A setting: title and explanation on the left, the control on the right.
fn row(ui: &mut egui::Ui, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 230.0).max(160.0));
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
            ui.add(egui::Label::new(RichText::new(desc).font(font(12.0)).color(c::TEXT_3)).wrap());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

pub fn divider(ui: &mut egui::Ui) {
    let r = ui.available_rect_before_wrap();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, c::LINE));
    ui.add_space(1.0);
}

/// On/off switch with its state in words for screen readers. Returns whether it changed.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, name: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 20.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, *on, name));
    if resp.clicked() {
        *on = !*on;
    }
    let bg = if *on {
        c::EMBER
    } else if resp.hovered() {
        c::CONTROL_HOVER
    } else {
        c::CONTROL
    };
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
    let x = if *on { rect.right() - 10.0 } else { rect.left() + 10.0 };
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 7.0, if *on { c::ON_EMBER } else { c::TEXT_2 });
    theme::focus_ring(ui, &resp, CornerRadius::same(10));
    let label = if *on { "켜짐" } else { "꺼짐" };
    ui.label(RichText::new(label).font(font(12.0)).color(c::TEXT_3));
    resp.clicked()
}
