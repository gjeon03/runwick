//! Toasts at the bottom right. Information disappears after ~5 s; errors stay until closed.

use crate::App;
use crate::icons;
use crate::state::ToastKind;
use crate::theme::{c, font, icon_font};
use egui::{Align2, CornerRadius, RichText, Vec2};
use std::time::Duration;

const LIFETIME: Duration = Duration::from_secs(5);
const MAX_SHOWN: usize = 4;

pub fn show(app: &mut App, ctx: &egui::Context) {
    app.toasts.retain(|t| t.kind == ToastKind::Error || t.created.elapsed() < LIFETIME);
    if app.toasts.is_empty() {
        return;
    }
    // Wake up when the next one expires.
    if let Some(next) = app
        .toasts
        .iter()
        .filter(|t| t.kind != ToastKind::Error)
        .map(|t| LIFETIME.saturating_sub(t.created.elapsed()))
        .min()
    {
        ctx.request_repaint_after(next + Duration::from_millis(20));
    }
    let start = app.toasts.len().saturating_sub(MAX_SHOWN);
    let mut close = None;
    egui::Area::new(egui::Id::unique("toasts"))
        .order(egui::Order::Tooltip)
        .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -16.0))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            for (i, t) in app.toasts.iter().enumerate().skip(start) {
                let (glyph, color) = match t.kind {
                    ToastKind::Info => (icons::INFO, c::SKY),
                    ToastKind::Success => (icons::CIRCLE_CHECK, c::MINT),
                    ToastKind::Error => (icons::CIRCLE_ALERT, c::ROSE),
                };
                egui::Frame::new()
                    .fill(c::PANEL_2)
                    .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
                    .corner_radius(CornerRadius::same(9))
                    .inner_margin(egui::Margin { left: 12, right: 6, top: 8, bottom: 8 })
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(110),
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(360.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.label(RichText::new(glyph).font(icon_font(14.0)).color(color));
                            ui.add(egui::Label::new(RichText::new(&t.text).font(font(12.5)).color(c::TEXT)).wrap());
                            let tip = if t.kind == ToastKind::Error {
                                "알림 닫기"
                            } else {
                                "알림 닫기 (5초 뒤 사라져요)"
                            };
                            if super::widgets::icon_btn(ui, icons::X, tip, 22.0, 22.0, 12.0).clicked() {
                                close = Some(i);
                            }
                        });
                    });
            }
        });
    if let Some(i) = close {
        app.toasts.remove(i);
    }
}
