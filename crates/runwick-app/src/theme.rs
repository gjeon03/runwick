//! Graphite Wick: graphite workshop with one ember. Tokens follow docs/design/README.md §2.
//! Use these helpers instead of ad-hoc colors so every screen stays consistent.

use crate::icons;
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Pos2, Response, RichText, Sense,
    Stroke, Ui, Vec2,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Color tokens.
pub mod c {
    use egui::Color32;
    const fn rgb(hex: u32) -> Color32 {
        Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }
    // Grounds
    pub const TERMINAL: Color32 = rgb(0x0B0C0E);
    pub const WINDOW: Color32 = rgb(0x0F1012);
    pub const DOC: Color32 = rgb(0x101114);
    pub const NAV: Color32 = rgb(0x15161A);
    pub const PANEL: Color32 = rgb(0x17181C);
    pub const PANEL_2: Color32 = rgb(0x1B1C20);
    pub const ROW: Color32 = rgb(0x1F2025);
    pub const CONTROL: Color32 = rgb(0x26272D);
    pub const CONTROL_HOVER: Color32 = rgb(0x33343B);
    pub const LINE: Color32 = rgb(0x25262B);
    pub const LINE_STRONG: Color32 = rgb(0x33343B);
    // Text
    pub const TEXT_STRONG: Color32 = rgb(0xEDEDF0);
    pub const TEXT: Color32 = rgb(0xD6D7DC);
    pub const TEXT_2: Color32 = rgb(0xB0B1B8);
    pub const TEXT_3: Color32 = rgb(0x8E8F98);
    pub const TEXT_OFF: Color32 = rgb(0x6F7078);
    // States
    pub const EMBER: Color32 = rgb(0xFF8B3E);
    pub const EMBER_BG: Color32 = rgb(0x2B1C12);
    pub const ON_EMBER: Color32 = rgb(0x1B0F06);
    pub const ROSE: Color32 = rgb(0xFF7B86);
    pub const ROSE_BG: Color32 = rgb(0x2A1719);
    pub const SKY: Color32 = rgb(0x74B7FF);
    pub const SKY_BG: Color32 = rgb(0x132232);
    pub const MINT: Color32 = rgb(0x5AD6A4);
    pub const MINT_BG: Color32 = rgb(0x10281C);
    pub const VIOLET: Color32 = rgb(0xBBA2FF);
    pub const VIOLET_BG: Color32 = rgb(0x211B33);
    pub const YELLOW: Color32 = rgb(0xF2C66D);
    pub const YELLOW_BG: Color32 = rgb(0x2A2412);
    pub const GREY: Color32 = rgb(0x9B9CA5);
    pub const GREY_BG: Color32 = rgb(0x1F2025);
}

/// Layout sizes (logical points).
pub mod size {
    pub const SIDEBAR: f32 = 264.0;
    pub const SIDEBAR_COLLAPSED: f32 = 48.0;
    pub const SIDEBAR_MIN: f32 = 220.0;
    pub const SIDEBAR_MAX: f32 = 420.0;
    pub const RIGHT_PANEL_MIN: f32 = 320.0;
    pub const RIGHT_PANEL_MAX: f32 = 640.0;
    /// Below this window width the sidebar collapses to an icon rail.
    pub const NARROW_WINDOW: f32 = 860.0;
    pub const PANEL_HEAD: f32 = 44.0;
    pub const RIGHT_PANEL: f32 = 380.0;
    pub const TITLEBAR: f32 = 44.0;
    pub const TABS: f32 = 38.0;
    pub const SESSION_BAR: f32 = 34.0;
    pub const ROW: f32 = 28.0;
    /// Below this central width the right panel becomes an overlay.
    pub const CENTER_MIN: f32 = 520.0;
    pub const RADIUS: u8 = 7;
    pub const RADIUS_SM: u8 = 5;
    pub const RADIUS_LG: u8 = 12;
}

/// Type sizes.
pub mod ty {
    pub const TITLE: f32 = 22.0;
    pub const PANEL_TITLE: f32 = 15.0;
    pub const BODY: f32 = 13.0;
    pub const SMALL: f32 = 12.0;
    pub const MONO: f32 = 12.5;
}

/// What a status means to the person. Always shown with an icon and words, never color alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Answer/approval needed: something only the person can do.
    Attention,
    /// Failed, needs recovery, or a stop confirmation.
    Recover,
    /// New result, AI summaries and suggestions.
    Info,
    /// Observed running, connected, passed.
    Running,
    /// I hold the input; drafts only on this computer.
    Direct,
    /// Needs a check: version mismatch, partial records, stale verification.
    Check,
    /// Waiting, held, disconnected (always with a reason).
    Waiting,
    /// Neutral label.
    Neutral,
}

impl Tone {
    pub fn fg(self) -> Color32 {
        match self {
            Tone::Attention => c::EMBER,
            Tone::Recover => c::ROSE,
            Tone::Info => c::SKY,
            Tone::Running => c::MINT,
            Tone::Direct => c::VIOLET,
            Tone::Check => c::YELLOW,
            Tone::Waiting => c::GREY,
            Tone::Neutral => c::TEXT_2,
        }
    }
    pub fn bg(self) -> Color32 {
        match self {
            Tone::Attention => c::EMBER_BG,
            Tone::Recover => c::ROSE_BG,
            Tone::Info => c::SKY_BG,
            Tone::Running => c::MINT_BG,
            Tone::Direct => c::VIOLET_BG,
            Tone::Check => c::YELLOW_BG,
            Tone::Waiting | Tone::Neutral => c::GREY_BG,
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Tone::Attention => icons::HAND,
            Tone::Recover => icons::CIRCLE_ALERT,
            Tone::Info => icons::SPARKLES,
            Tone::Running => icons::CIRCLE_DOT,
            Tone::Direct => icons::KEYBOARD,
            Tone::Check => icons::TRIANGLE_ALERT,
            Tone::Waiting => icons::CIRCLE_PAUSE,
            Tone::Neutral => icons::CIRCLE,
        }
    }
}

static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

pub fn set_reduce_motion(v: bool) {
    REDUCE_MOTION.store(v, Ordering::Relaxed);
}

pub fn reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------------------------
// Fonts

pub const FAMILY_MEDIUM: &str = "medium";
pub const FAMILY_SEMIBOLD: &str = "semibold";
pub const FAMILY_ICONS: &str = "icons";
pub const FAMILY_MONO_BOLD: &str = "mono-bold";

pub fn medium_family() -> FontFamily {
    FontFamily::Name(FAMILY_MEDIUM.into())
}
pub fn semibold_family() -> FontFamily {
    FontFamily::Name(FAMILY_SEMIBOLD.into())
}
pub fn icon_family() -> FontFamily {
    FontFamily::Name(FAMILY_ICONS.into())
}
pub fn mono_bold_family() -> FontFamily {
    FontFamily::Name(FAMILY_MONO_BOLD.into())
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn medium(size: f32) -> FontId {
    FontId::new(size, medium_family())
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, semibold_family())
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, icon_family())
}

/// Korean (and other CJK) fallback from the system, memory-mapped so only used glyphs load.
/// Returns faces for regular, medium and semibold text (the same face when a font has one weight).
/// Symbols CLIs draw in terminals that neither JetBrains Mono nor the Korean font has
/// (Claude Code: ⏺ ✻ ⎿; spinners: ✳ ✢ ⠋), memory-mapped from the system. Missing files are
/// skipped.
fn system_symbol_fonts() -> Vec<FontData> {
    let candidates: &[&str] = &[
        // Monospace dingbats and arrows (✻ ✳ ✶ ✢ ❯ ✓ ✗ ★ ☐).
        "/System/Library/Fonts/Menlo.ttc",
        // Technical symbols and braille (⎿ ⠋).
        "/System/Library/Fonts/Apple Symbols.ttf",
        // Media symbols (⏺ ⏵ ⏸).
        "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    ];
    candidates
        .iter()
        .filter_map(|path| {
            let file = std::fs::File::open(path).ok()?;
            // SAFETY: system font files are not modified while the app runs.
            let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
            Some(FontData::from_blob(Arc::new(map), 0))
        })
        .collect()
}

fn system_cjk_fonts() -> Option<[FontData; 3]> {
    // (path, regular, medium, semibold) face indices.
    let candidates: &[(&str, u32, u32, u32)] = &[
        ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0, 2, 4),
        ("/System/Library/Fonts/Supplemental/AppleGothic.ttf", 0, 0, 0),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 1, 1, 1),
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 1, 1, 1),
        ("/usr/share/fonts/truetype/nanum/NanumGothic.ttf", 0, 0, 0),
    ];
    for (path, regular, medium, semibold) in candidates {
        let Ok(file) = std::fs::File::open(path) else { continue };
        // SAFETY: system font files are not modified while the app runs.
        let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
        let blob: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(map);
        let face = |i: u32| FontData::from_blob(blob.clone(), i);
        return Some([face(*regular), face(*medium), face(*semibold)]);
    }
    None
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let add = |defs: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        defs.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut defs, "Geist", include_bytes!("../../../assets/fonts/Geist-Regular.ttf"));
    add(&mut defs, "Geist-Medium", include_bytes!("../../../assets/fonts/Geist-Medium.ttf"));
    add(&mut defs, "Geist-SemiBold", include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf"));
    add(&mut defs, "JetBrainsMono", include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"));
    add(&mut defs, "JetBrainsMono-Bold", include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf"));
    defs.font_data.insert(
        "lucide".to_owned(),
        Arc::new(
            FontData::from_static(include_bytes!("../../../assets/fonts/lucide.ttf"))
                .tweak(egui::FontTweak { y_offset_factor: 0.08, ..Default::default() }),
        ),
    );
    let cjk_faces = system_cjk_fonts().map(|[regular, medium, semibold]| {
        defs.font_data.insert("cjk".to_owned(), Arc::new(regular));
        defs.font_data.insert("cjk-medium".to_owned(), Arc::new(medium));
        defs.font_data.insert("cjk-semibold".to_owned(), Arc::new(semibold));
    });
    let cjk = cjk_faces.map(|_| "cjk".to_owned());
    let cjk_weight = |name: &str| cjk_faces.map(|_| name.to_owned());

    let defaults_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let defaults_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let chain = |first: &str, cjk: Option<String>| -> Vec<String> {
        let mut v = vec![first.to_owned()];
        v.extend(cjk);
        v.extend(defaults_prop.iter().cloned());
        v
    };
    let symbols: Vec<String> = system_symbol_fonts()
        .into_iter()
        .enumerate()
        .map(|(i, data)| {
            let name = format!("symbols-{i}");
            defs.font_data.insert(name.clone(), Arc::new(data));
            name
        })
        .collect();
    let mono_chain = |first: &str| -> Vec<String> {
        let mut v = vec![first.to_owned()];
        v.extend(cjk.clone());
        v.extend(symbols.iter().cloned());
        v.extend(defaults_mono.iter().cloned());
        v
    };
    defs.families.insert(FontFamily::Proportional, chain("Geist", cjk.clone()));
    defs.families.insert(medium_family(), chain("Geist-Medium", cjk_weight("cjk-medium")));
    defs.families.insert(semibold_family(), chain("Geist-SemiBold", cjk_weight("cjk-semibold")));
    defs.families.insert(FontFamily::Monospace, mono_chain("JetBrainsMono"));
    defs.families.insert(mono_bold_family(), mono_chain("JetBrainsMono-Bold"));
    defs.families.insert(icon_family(), vec!["lucide".to_owned()]);
    ctx.set_fonts(defs);
}

pub fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        use egui::TextStyle;
        style.text_styles = [
            (TextStyle::Small, font(ty::SMALL)),
            (TextStyle::Body, font(ty::BODY)),
            (TextStyle::Button, font(ty::BODY)),
            (TextStyle::Heading, semibold(ty::TITLE)),
            (TextStyle::Monospace, mono(ty::MONO)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(8.0, 6.0);
        s.button_padding = Vec2::new(10.0, 5.0);
        s.interact_size = Vec2::new(24.0, 26.0);
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(14);
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = c::WINDOW;
        v.window_fill = c::PANEL;
        v.window_stroke = Stroke::new(1.0, c::LINE_STRONG);
        v.window_corner_radius = CornerRadius::same(size::RADIUS_LG);
        v.menu_corner_radius = CornerRadius::same(size::RADIUS);
        v.extreme_bg_color = c::TERMINAL;
        v.faint_bg_color = c::PANEL;
        v.code_bg_color = c::ROW;
        v.text_edit_bg_color = Some(c::ROW);
        v.hyperlink_color = c::SKY;
        v.warn_fg_color = c::YELLOW;
        v.error_fg_color = c::ROSE;
        v.selection.bg_fill = Color32::from_rgba_unmultiplied(116, 183, 255, 70);
        v.selection.stroke = Stroke::new(1.0, c::TEXT_STRONG);
        v.override_text_color = None;
        let w = &mut v.widgets;
        let base = CornerRadius::same(size::RADIUS_SM + 1);
        w.noninteractive.bg_fill = c::PANEL;
        w.noninteractive.weak_bg_fill = c::PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, c::LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, c::TEXT);
        w.noninteractive.corner_radius = base;
        w.inactive.bg_fill = c::CONTROL;
        w.inactive.weak_bg_fill = c::CONTROL;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, c::TEXT);
        w.inactive.corner_radius = base;
        w.hovered.bg_fill = c::CONTROL_HOVER;
        w.hovered.weak_bg_fill = c::CONTROL_HOVER;
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.0, c::TEXT_STRONG);
        w.hovered.corner_radius = base;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = c::LINE_STRONG;
        w.active.weak_bg_fill = c::LINE_STRONG;
        w.active.bg_stroke = Stroke::NONE;
        w.active.fg_stroke = Stroke::new(1.0, c::TEXT_STRONG);
        w.active.corner_radius = base;
        w.active.expansion = 0.0;
        w.open = w.hovered;
    });
}

// ---------------------------------------------------------------------------------------------
// Small building blocks

pub fn icon(glyph: &str, size: f32, color: Color32) -> RichText {
    RichText::new(glyph).font(icon_font(size)).color(color)
}

/// Primary action: the only filled ember button in an area.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let b = egui::Button::new(RichText::new(text).font(medium(ty::BODY)).color(c::ON_EMBER))
        .fill(c::EMBER)
        .corner_radius(CornerRadius::same(size::RADIUS_SM + 1))
        .min_size(Vec2::new(0.0, 28.0));
    ui.add(b)
}

/// Secondary action: grey ground.
pub fn secondary_button(ui: &mut Ui, text: &str) -> Response {
    let b = egui::Button::new(RichText::new(text).font(medium(ty::BODY)).color(c::TEXT))
        .fill(c::CONTROL)
        .corner_radius(CornerRadius::same(size::RADIUS_SM + 1))
        .min_size(Vec2::new(0.0, 28.0));
    ui.add(b)
}

/// Destructive confirmation (stop, remove).
pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    let b = egui::Button::new(RichText::new(text).font(medium(ty::BODY)).color(c::ROSE))
        .fill(c::ROSE_BG)
        .corner_radius(CornerRadius::same(size::RADIUS_SM + 1))
        .min_size(Vec2::new(0.0, 28.0));
    ui.add(b)
}

/// Text-only button for low-emphasis actions.
pub fn link_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_2)).frame(false))
}

/// Icon-only button. Always give a tooltip: it is also the accessible name.
pub fn icon_button(ui: &mut Ui, glyph: &str, tooltip: &str) -> Response {
    let size = Vec2::splat(26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_text(tooltip);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tooltip));
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered() || resp.has_focus();
        if hovered {
            ui.painter().rect_filled(rect, CornerRadius::same(size::RADIUS_SM), c::CONTROL);
        }
        let color = if hovered { c::TEXT_STRONG } else { c::TEXT_3 };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, glyph, icon_font(14.0), color);
    }
    resp
}

/// Small rounded label with icon, colored by tone. Text is always present.
pub fn chip(ui: &mut Ui, tone: Tone, text: &str) -> Response {
    chip_with_icon(ui, tone, Some(tone.icon()), text)
}

pub fn chip_with_icon(ui: &mut Ui, tone: Tone, glyph: Option<&str>, text: &str) -> Response {
    let fg = tone.fg();
    egui::Frame::new()
        .fill(tone.bg())
        .corner_radius(CornerRadius::same(size::RADIUS_SM))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.horizontal(|ui| {
                if let Some(g) = glyph {
                    ui.label(icon(g, 11.0, fg));
                }
                ui.label(RichText::new(text).font(medium(ty::SMALL)).color(fg));
            });
        })
        .response
}

/// Filled dot; `pulse` makes running state breathe like a wick (off with reduced motion).
pub fn status_dot(ui: &mut Ui, tone: Tone, pulse: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    let mut color = tone.fg();
    if pulse && !reduce_motion() {
        let t = ui.input(|i| i.time) as f32;
        let a = 0.55 + 0.45 * (t * 2.2).sin().abs();
        color = color.gamma_multiply(a);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
    ui.painter().circle_filled(rect.center(), 3.5, color);
    resp
}

/// Keyboard shortcut hint like ⌘K.
pub fn kbd(ui: &mut Ui, text: &str) -> Response {
    egui::Frame::new()
        .fill(c::CONTROL)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(5, 1))
        .show(ui, |ui| ui.label(RichText::new(text).font(mono(11.0)).color(c::TEXT_3)))
        .response
}

/// Muted section label used in sidebars and panels.
pub fn section_label(ui: &mut Ui, text: &str) -> Response {
    ui.label(RichText::new(text).font(medium(11.5)).color(c::TEXT_3))
}

/// Card-like grouped area on a panel.
pub fn card_frame() -> egui::Frame {
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(size::RADIUS + 1)).inner_margin(Margin::same(12))
}

/// One-line text with ellipsis and the full text on hover (long Korean names, paths).
pub fn truncated(ui: &mut Ui, text: impl Into<String>, font: FontId, color: Color32) -> Response {
    let text = text.into();
    let job = egui::text::LayoutJob::simple_singleline(text.clone(), font, color);
    let resp = ui.add(egui::Label::new(job).truncate());
    if resp.hovered() { resp.clone().on_hover_text(text) } else { resp }
}

/// One option of a [`segmented`] control: text, icon, or both. `name` is the accessible name.
pub struct Seg<'a> {
    pub text: Option<&'a str>,
    pub glyph: Option<&'a str>,
    pub name: &'a str,
}

impl<'a> Seg<'a> {
    pub fn text(text: &'a str) -> Self {
        Self { text: Some(text), glyph: None, name: text }
    }
    pub fn icon(glyph: &'a str, name: &'a str) -> Self {
        Self { text: None, glyph: Some(glyph), name }
    }
}

/// Segmented switch such as `전체 / ☆` or `세션 / 파일`. Returns the option clicked this frame.
pub fn segmented(ui: &mut Ui, items: &[Seg<'_>], selected: usize, height: f32) -> Option<usize> {
    let mut clicked = None;
    egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(size::RADIUS)).inner_margin(Margin::same(2)).show(
        ui,
        |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (i, it) in items.iter().enumerate() {
                    let on = i == selected;
                    let fg = if on { c::TEXT_STRONG } else { c::TEXT_3 };
                    let galley = it.text.map(|t| ui.painter().layout_no_wrap(t.to_owned(), font(ty::SMALL), fg));
                    let w = match &galley {
                        Some(g) => g.size().x + 16.0,
                        None => 28.0,
                    };
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, height), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, it.name));
                    let hovered = resp.hovered() && !on;
                    if on {
                        ui.painter().rect_filled(rect, CornerRadius::same(size::RADIUS_SM), c::CONTROL_HOVER);
                    } else if hovered {
                        ui.painter().rect_filled(rect, CornerRadius::same(size::RADIUS_SM), c::CONTROL);
                    }
                    let fg = if hovered { c::TEXT } else { fg };
                    match galley {
                        Some(g) => {
                            let pos = rect.center() - g.size() / 2.0;
                            ui.painter().galley(pos, g, fg);
                        }
                        // The favorites filter shows a filled star while it is on.
                        None if on && it.glyph == Some(icons::STAR) => {
                            paint_star(ui.painter(), rect.center(), 6.5, fg);
                        }
                        None => {
                            let g = it.glyph.unwrap_or(icons::CIRCLE);
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, g, icon_font(13.0), fg);
                        }
                    }
                    focus_ring(ui, &resp, CornerRadius::same(size::RADIUS_SM));
                    let resp = if it.text.is_none() { resp.on_hover_text(it.name) } else { resp };
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                }
            });
        },
    );
    clicked
}

/// Paint a filled five-point star (favorite projects, the active favorites filter).
pub fn paint_star(painter: &egui::Painter, center: Pos2, r: f32, fill: Color32) {
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        let rr = if i % 2 == 0 { r } else { r * 0.45 };
        pts.push(Pos2::new(center.x + rr * a.cos(), center.y + rr * a.sin()));
    }
    // Convex fan from the center keeps the star shape without a concave polygon fill.
    for (i, p) in pts.iter().enumerate() {
        let tri = vec![center, *p, pts[(i + 1) % 10]];
        painter.add(egui::Shape::convex_polygon(tri, fill, Stroke::NONE));
    }
}

/// Search box with a leading magnifier, like the sidebar and palette fields.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, id: egui::Id, height: f32) -> Response {
    egui::Frame::new()
        .fill(c::ROW)
        .corner_radius(CornerRadius::same(size::RADIUS))
        .inner_margin(Margin::symmetric(9, 0))
        .show(ui, |ui| {
            ui.set_height(height);
            ui.spacing_mut().item_spacing.x = 7.0;
            ui.horizontal_centered(|ui| {
                ui.label(icon(icons::SEARCH, 13.0, c::TEXT_3));
                ui.add(
                    egui::TextEdit::singleline(text)
                        .id(id)
                        .frame(egui::Frame::NONE)
                        .margin(Margin::ZERO)
                        .font(font(12.5))
                        .text_color(c::TEXT_STRONG)
                        .hint_text(RichText::new(hint).font(font(12.5)).color(c::TEXT_3))
                        .align(egui::Align2::LEFT_CENTER)
                        .min_size(Vec2::new(0.0, height))
                        .desired_width(f32::INFINITY),
                )
            })
            .inner
        })
        .inner
}

/// Short tool mark (`Cl`, `Cx`, `$`) on a dark square, as in tabs and session rows.
pub fn tool_badge(ui: &mut Ui, mark: &str, side: f32, bg: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    paint_tool_badge(ui.painter(), rect, mark, bg);
    resp
}

pub fn paint_tool_badge(painter: &egui::Painter, rect: egui::Rect, mark: &str, bg: Color32) {
    let size = if rect.height() >= 28.0 {
        11.5
    } else if rect.height() >= 22.0 {
        10.0
    } else {
        9.5
    };
    painter.rect_filled(rect, CornerRadius::same(if rect.height() >= 28.0 { 7 } else { 4 }), bg);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        mark,
        FontId::new(size, mono_bold_family()),
        c::TEXT_STRONG,
    );
}

/// Visible keyboard focus: a light outline distinct from selection and running state.
pub fn focus_ring(ui: &Ui, resp: &Response, radius: CornerRadius) {
    if resp.has_focus() && ui.input(|i| !i.pointer.any_down()) {
        ui.painter().rect_stroke(
            resp.rect.expand(1.0),
            radius,
            Stroke::new(1.5, c::TEXT_STRONG),
            egui::StrokeKind::Outside,
        );
    }
}
