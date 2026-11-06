//! Start screens: the CLI chooser of an empty workspace (S01) and the first start without any
//! project (S00). Tool states come from `tools.list`; unknown stays "확인 불가".

use super::{actions, data, widgets};
use crate::App;
use crate::client::HostKey;
use crate::state::{SettingsSection, SidebarMode, TabId, TabKind, WsKey};
use crate::theme::{self, Tone, c, font, icon_font, medium, mono, semibold, ty};
use egui::{Align2, CornerRadius, Pos2, Rect, RichText, Sense, Vec2};
use runwick_core::api::projects::WorkspaceKind;
use runwick_core::api::sessions::Tool;

/// `TabKind::Start` inside a workspace.
pub fn show_start_tab(app: &mut App, ui: &mut egui::Ui, ws: WsKey, _tab: TabId) {
    show_start(app, ui, ws);
}

fn tone_color(t: Tone) -> egui::Color32 {
    match t {
        Tone::Neutral => c::TEXT_2,
        Tone::Waiting => c::TEXT_3,
        t => t.fg(),
    }
}

/// S01: "어떤 CLI로 시작할까요?"
pub fn show_start(app: &mut App, ui: &mut egui::Ui, ws: WsKey) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, c::WINDOW);
    let Some(w) = data::workspaces(app).data.and_then(|l| data::find_ws(&l, &ws).cloned()) else { return };
    let project = w.project_id;
    let lines = data::tool_lines(app, ws.host, true);
    let width = (rect.width() - 64.0).min(640.0);
    let grid_h = 128.0;
    let content_h = 70.0 + 24.0 + grid_h + 24.0 + 20.0;
    let top = rect.top() + ((rect.height() - content_h) / 2.0).max(24.0);
    let col = Rect::from_min_size(Pos2::new(rect.center().x - width / 2.0, top), Vec2::new(width, content_h));
    let mut start = None;
    let mut help = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(col), |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new("어떤 CLI로 시작할까요?").font(semibold(24.0)).color(c::TEXT_STRONG));
        let space = if w.kind == WorkspaceKind::Default {
            "기본 공간이".to_string()
        } else {
            format!("{} 공간이", w.name)
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(RichText::new(format!("{space} 준비됐어요. 세션은 ")).font(font(ty::BODY)).color(c::TEXT_2));
            ui.label(RichText::new(data::tilde(&w.path)).font(mono(12.0)).color(c::TEXT));
            ui.label(RichText::new(" 폴더에서 열립니다.").font(font(ty::BODY)).color(c::TEXT_2));
        });
        ui.add_space(18.0);
        let gap = 8.0;
        let cols = if width >= 3.0 * 180.0 + 2.0 * gap { 3 } else { 1 };
        let card_w = if cols == 3 { (width - 2.0 * gap) / 3.0 } else { width };
        let card_h = if cols == 3 { 128.0 } else { 64.0 };
        let (grid, _) = ui.allocate_exact_size(
            Vec2::new(width, if cols == 3 { card_h } else { 3.0 * card_h + 2.0 * gap }),
            Sense::hover(),
        );
        for (i, (tool, line, info)) in lines.iter().enumerate() {
            let tool = *tool;
            let (cx, cy) = if cols == 3 { (i as f32 * (card_w + gap), 0.0) } else { (0.0, i as f32 * (card_h + gap)) };
            let r = Rect::from_min_size(grid.min + Vec2::new(cx, cy), Vec2::new(card_w, card_h));
            let name = match (tool, info) {
                (Tool::Shell, Some(t)) if t.installed => "셸".to_string(),
                _ => tool.label().to_string(),
            };
            let busy = app.shell.center.starting == Some(tool);
            let any_busy = app.shell.center.starting.is_some();
            match tool_card(ui, r, tool, &name, line, busy, any_busy, cols == 3) {
                CardHit::Start => start = Some(tool),
                CardHit::Help => help = Some(tool),
                CardHit::None => {}
            }
        }
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.label(RichText::new("먼저 둘러보려면").font(font(12.5)).color(c::TEXT_3));
            if link(ui, "파일 보기").clicked() {
                app.ui.sidebar_mode = SidebarMode::Files;
                app.ui.sidebar_collapsed = false;
            }
            let readme = std::path::Path::new(&w.path).join("README.md");
            if ws.host == HostKey::Local && readme.is_file() && link(ui, "README.md 열기").clicked() {
                app.ui.open_tab(ws, TabKind::Doc { ws, path: "README.md".into() });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("새 세션 ⌘T").font(font(12.5)).color(c::TEXT_3));
            });
        });
    });
    if let Some(tool) = start {
        actions::start_session(app, ws, project, tool, None);
    }
    if help.is_some() {
        actions::open_settings(app, Some(SettingsSection::Tools));
    }
}

fn link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let resp =
        ui.add(egui::Label::new(RichText::new(text).font(font(12.5)).color(c::TEXT).underline()).sense(Sense::click()));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    theme::focus_ring(ui, &resp, CornerRadius::same(3));
    resp
}

enum CardHit {
    None,
    Start,
    Help,
}

#[allow(clippy::too_many_arguments)]
fn tool_card(
    ui: &mut egui::Ui,
    r: Rect,
    tool: Tool,
    name: &str,
    line: &data::ToolLine,
    busy: bool,
    any_busy: bool,
    tall: bool,
) -> CardHit {
    let enabled = line.startable && !any_busy;
    let resp =
        ui.interact(r, ui.scope_id().with(("tool-card", tool)), if enabled { Sense::click() } else { Sense::hover() });
    let label = format!("{name} 시작 — {}", line.text);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label));
    let bg = if enabled && resp.hovered() { c::CONTROL } else { c::ROW };
    ui.painter().rect_filled(r, CornerRadius::same(10), bg);
    let badge = if tall {
        Rect::from_min_size(r.min + Vec2::splat(14.0), Vec2::splat(30.0))
    } else {
        Rect::from_center_size(Pos2::new(r.left() + 29.0, r.center().y), Vec2::splat(30.0))
    };
    theme::paint_tool_badge(ui.painter(), badge, data::tool_mark(tool), c::CONTROL_HOVER);
    let (name_pos, state_pos) = if tall {
        (Pos2::new(r.left() + 14.0, r.top() + 56.0), Pos2::new(r.left() + 14.0, r.top() + 77.0))
    } else {
        (Pos2::new(badge.right() + 12.0, r.center().y - 18.0), Pos2::new(badge.right() + 12.0, r.center().y + 2.0))
    };
    let name_fg = if line.startable { c::TEXT_STRONG } else { c::TEXT_2 };
    ui.painter().text(name_pos, Align2::LEFT_TOP, name, medium(14.0), name_fg);
    let state = if busy { "시작하는 중…".to_string() } else { line.text.clone() };
    let state_rect = Rect::from_min_max(state_pos, Pos2::new(r.right() - 12.0, state_pos.y + 18.0));
    widgets::paint_clipped(ui, state_rect, &state, font(12.0), tone_color(line.tone), false);
    let mut hit = CardHit::None;
    if let Some(h) = line.help {
        let hr = Rect::from_min_size(Pos2::new(r.right() - 84.0, r.top() + 14.0), Vec2::new(70.0, 26.0));
        let hresp = ui.interact(hr, ui.scope_id().with(("tool-help", tool)), Sense::click());
        ui.painter().rect_filled(
            hr,
            CornerRadius::same(6),
            if hresp.hovered() { c::CONTROL_HOVER } else { c::CONTROL },
        );
        ui.painter().text(hr.center(), Align2::CENTER_CENTER, h, font(12.0), c::TEXT);
        if hresp.on_hover_text("설정 › CLI 도구에서 확인해요").clicked() {
            hit = CardHit::Help;
        }
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(10));
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    if matches!(hit, CardHit::None) && resp.clicked() {
        hit = CardHit::Start;
    }
    hit
}

/// S00: first start without any project.
pub fn show_welcome(app: &mut App, ui: &mut egui::Ui) {
    egui::CentralPanel::default().frame(egui::Frame::new().fill(c::WINDOW)).show(ui, |ui| {
        let view = ui.max_rect();
        let wide = view.width() >= 380.0 * 2.0 + 56.0 + 64.0;
        let width = (view.width() - 64.0).min(960.0);
        let col_w = if wide { (width - 56.0) / 2.0 } else { width };
        let needed = if wide { 330.0 } else { 660.0 };
        egui::ScrollArea::vertical().id_salt("welcome").auto_shrink([false, false]).show(ui, |ui| {
            let h = view.height().max(needed + 48.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(view.width(), h), Sense::hover());
            // Centered a little above the middle, like the design (bottom padding 72).
            let top = rect.top() + ((rect.height() - 72.0 - needed) / 2.0).max(24.0);
            let left = Rect::from_min_size(Pos2::new(rect.center().x - width / 2.0, top), Vec2::new(col_w, 330.0));
            let right = if wide {
                Rect::from_min_size(Pos2::new(left.right() + 56.0, top + 20.0), Vec2::new(col_w, 300.0))
            } else {
                Rect::from_min_size(Pos2::new(left.left(), left.bottom() + 32.0), Vec2::new(col_w, 300.0))
            };
            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| welcome_intro(app, ui));
            ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| welcome_tools(app, ui));
        });
    });
}

/// The Runwick mark: an ember flame over a grey wick (design `Main`, 24-unit view box).
fn paint_logo(ui: &mut egui::Ui, side: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let k = side / 24.0;
    let p = |x: f32, y: f32| rect.min + Vec2::new(x * k, y * k);
    let cubic = |pts: &mut Vec<Pos2>, a: (f32, f32), b: (f32, f32), c2: (f32, f32), d: (f32, f32)| {
        for i in 1..=12 {
            let t = i as f32 / 12.0;
            let u = 1.0 - t;
            let x = u * u * u * a.0 + 3.0 * u * u * t * b.0 + 3.0 * u * t * t * c2.0 + t * t * t * d.0;
            let y = u * u * u * a.1 + 3.0 * u * u * t * b.1 + 3.0 * u * t * t * c2.1 + t * t * t * d.1;
            pts.push(p(x, y));
        }
    };
    let mut pts = vec![p(12.0, 2.0)];
    cubic(&mut pts, (12.0, 2.0), (13.0, 5.6), (17.0, 7.6), (17.0, 12.0));
    // Lower half: arc of radius 5 around (12, 12) from the right side through the bottom.
    for i in 1..=16 {
        let a = std::f32::consts::PI * i as f32 / 16.0;
        pts.push(p(12.0 + 5.0 * a.cos(), 12.0 + 5.0 * a.sin()));
    }
    cubic(&mut pts, (7.0, 12.0), (7.0, 9.8), (8.2, 8.6), (9.0, 7.6));
    cubic(&mut pts, (9.0, 7.6), (9.3, 9.0), (10.1, 9.7), (10.8, 9.7));
    cubic(&mut pts, (10.8, 9.7), (10.6, 7.2), (10.6, 4.6), (12.0, 2.0));
    pts.pop(); // back at the start point
    // The outline is star-shaped around this point, so a triangle fan fills it exactly; a thin
    // outline in the same color smooths the edge.
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(p(12.4, 12.6), c::EMBER);
    for q in &pts {
        mesh.colored_vertex(*q, c::EMBER);
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    ui.painter().add(egui::Shape::closed_line(pts, egui::Stroke::new(0.75, c::EMBER)));
    ui.painter().line_segment([p(12.0, 18.0), p(12.0, 22.0)], egui::Stroke::new(1.5 * k, c::TEXT_3));
}

fn welcome_intro(app: &mut App, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    paint_logo(ui, 40.0);
    ui.add_space(20.0);
    ui.label(
        RichText::new("폴더를 열면\n바로 시작합니다")
            .font(semibold(36.0))
            .color(c::TEXT_STRONG)
            .line_height(Some(43.0)),
    );
    ui.add_space(20.0);
    ui.add(
        egui::Label::new(
            RichText::new(
                "쓰던 CLI를 그대로 열고, 필요하면 여러 AI로 넓혀 갑니다. 대화와 결과는 Runwick이 이어 둡니다.",
            )
            .font(font(15.0))
            .color(c::TEXT_2),
        )
        .wrap(),
    );
    ui.add_space(24.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let open = egui::Button::new(RichText::new("폴더 열기").font(semibold(14.0)).color(c::ON_EMBER))
            .right_text(RichText::new("⌘O").font(medium(12.0)).color(egui::Color32::from_rgb(0x4A, 0x28, 0x10)))
            .fill(c::EMBER)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(0.0, 40.0));
        let r = ui.add(open);
        theme::focus_ring(ui, &r, CornerRadius::same(8));
        if r.clicked() {
            actions::open_folder_dialog(app);
        }
        let import =
            egui::Button::new(RichText::new("다른 컴퓨터의 기록 가져오기").font(font(14.0)).color(c::TEXT_STRONG))
                .fill(c::ROW)
                .corner_radius(CornerRadius::same(8))
                .min_size(Vec2::new(0.0, 40.0));
        let r = ui.add(import).on_hover_text("GitHub 백업·동기화 저장소에서 기록을 가져와요");
        theme::focus_ring(ui, &r, CornerRadius::same(8));
        if r.clicked() {
            actions::open_settings(app, Some(SettingsSection::Backup));
            crate::views::backup::open_import(app);
        }
    });
    ui.add_space(20.0);
    ui.label(
        RichText::new("이 창으로 폴더를 끌어다 놓아도 됩니다. Git이 아닌 폴더도 열 수 있어요.")
            .font(font(12.5))
            .color(c::TEXT_3),
    );
}

fn welcome_tools(app: &mut App, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 14.0;
    ui.label(RichText::new("이 컴퓨터에서 쓸 수 있는 도구").font(medium(12.5)).color(c::TEXT_3));
    let lines = data::tool_lines(app, HostKey::Local, false);
    let mut help = false;
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, (tool, line, info)) in lines.iter().enumerate() {
            let (tool, info) = (*tool, info.as_ref());
            let w = ui.available_width();
            let (r, _) = ui.allocate_exact_size(Vec2::new(w, 56.0), Sense::hover());
            if i < 2 {
                ui.painter().hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, c::LINE));
            }
            let badge = Rect::from_center_size(Pos2::new(r.left() + 29.0, r.center().y), Vec2::splat(30.0));
            theme::paint_tool_badge(ui.painter(), badge, data::tool_mark(tool), c::CONTROL);
            let name = match (tool, info) {
                (Tool::Shell, Some(t)) if t.installed => format!("셸 ({})", data::shell_name(t)),
                _ => tool.label().to_string(),
            };
            ui.painter().text(
                Pos2::new(badge.right() + 12.0, r.center().y - 9.0),
                Align2::LEFT_CENTER,
                &name,
                medium(ty::BODY),
                c::TEXT_STRONG,
            );
            let glyph = line.glyph();
            let col = tone_color(line.tone);
            let sy = r.center().y + 10.0;
            ui.painter().text(Pos2::new(badge.right() + 18.5, sy), Align2::CENTER_CENTER, glyph, icon_font(13.0), col);
            ui.painter().text(Pos2::new(badge.right() + 30.0, sy), Align2::LEFT_CENTER, &line.text, font(12.0), col);
            if let Some(h) = line.help {
                let tw = widgets::text_width(ui, h, font(12.0)) + 20.0;
                let hr =
                    Rect::from_center_size(Pos2::new(r.right() - 14.0 - tw / 2.0, r.center().y), Vec2::new(tw, 28.0));
                let resp = ui.interact(hr, ui.scope_id().with(("welcome-help", tool)), Sense::click());
                ui.painter().rect_filled(
                    hr,
                    CornerRadius::same(6),
                    if resp.hovered() { c::CONTROL_HOVER } else { c::CONTROL },
                );
                ui.painter().text(hr.center(), Align2::CENTER_CENTER, h, font(12.0), c::TEXT_STRONG);
                if resp.clicked() {
                    help = true;
                }
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let f = font(12.5);
        ui.label(RichText::new("도구가 준비되지 않아도 파일과 기록은 열 수 있어요. AI는 필요할 때 허용한 범위 안에서 다른 CLI에 일을 맡기며, 범위는 ").font(f.clone()).color(c::TEXT_3));
        if ui.add(egui::Label::new(RichText::new("설정").font(f.clone()).color(c::TEXT_2).underline()).sense(Sense::click())).clicked() {
            actions::open_settings(app, Some(SettingsSection::Collab));
        }
        ui.label(RichText::new("에서 바꿀 수 있습니다.").font(f).color(c::TEXT_3));
    });
    if help {
        actions::open_settings(app, Some(SettingsSection::Tools));
    }
}
