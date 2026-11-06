//! Settings page frame: left navigation (232) and the section body on the document ground.
//! Section bodies belong to their feature modules (`views::settings::show_section`).

use super::widgets;
use crate::App;
use crate::icons;
use crate::state::{Page, SettingsSection};
use crate::theme::{c, font, icon_font};
use egui::{Align2, CornerRadius, Pos2, Sense, Vec2};

pub const SECTIONS: [(SettingsSection, &str); 9] = [
    (SettingsSection::General, "일반"),
    (SettingsSection::Tools, "CLI 도구"),
    (SettingsSection::Env, "공통 환경변수"),
    (SettingsSection::Mcp, "MCP"),
    (SettingsSection::Collab, "협업"),
    (SettingsSection::Roles, "역할·작업 방식"),
    (SettingsSection::Automation, "자동 작업"),
    (SettingsSection::Devices, "기기"),
    (SettingsSection::Backup, "백업·동기화"),
];

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::left("settings-nav")
        .exact_size(232.0)
        .resizable(false)
        .show_separator_line(true)
        .frame(egui::Frame::new().fill(c::NAV).inner_margin(egui::Margin::symmetric(8, 12)))
        .show(ui, |ui| nav(app, ui));
    egui::CentralPanel::default().frame(egui::Frame::new().fill(c::DOC)).show(ui, |ui| {
        let section = app.ui.settings_section;
        egui::ScrollArea::vertical().id_salt(("settings-body", section as u8)).auto_shrink([false, false]).show(
            ui,
            |ui| {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(32, 28)).show(ui, |ui| {
                    let w = ui.available_width().min(800.0);
                    let pad = ((ui.available_width() - w) / 2.0).max(0.0);
                    ui.horizontal(|ui| {
                        ui.add_space(pad);
                        ui.vertical(|ui| {
                            ui.set_width(w);
                            crate::views::settings::show_section(app, ui, section);
                        });
                    });
                });
            },
        );
    });
}

fn nav(app: &mut App, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
    let back = widgets::row(ui, 30.0, false, "작업실로 돌아가기");
    let r = back.rect;
    let fg = if back.hovered() { c::TEXT_STRONG } else { c::TEXT_2 };
    ui.painter().text(
        Pos2::new(r.left() + 14.0, r.center().y),
        Align2::CENTER_CENTER,
        icons::ARROW_LEFT,
        icon_font(12.0),
        fg,
    );
    ui.painter().text(
        Pos2::new(r.left() + 26.0, r.center().y),
        Align2::LEFT_CENTER,
        "작업실로 돌아가기",
        font(12.5),
        fg,
    );
    if back.on_hover_text("Esc").clicked() {
        app.ui.page = Page::Work;
    }
    ui.add_space(8.0);
    for (section, label) in SECTIONS {
        let on = app.ui.settings_section == section;
        let w = ui.available_width();
        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, label));
        if on {
            ui.painter().rect_filled(r, CornerRadius::same(6), c::CONTROL);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(6), c::ROW);
        }
        let fg = if on { c::TEXT_STRONG } else { c::TEXT_2 };
        ui.painter().text(Pos2::new(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, label, font(13.0), fg);
        crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
        if resp.clicked() {
            app.ui.settings_section = section;
        }
    }
}
