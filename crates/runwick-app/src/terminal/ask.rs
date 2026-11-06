//! Questions a terminal asks before acting on someone's behalf: a program writing to the
//! clipboard (OSC 52), and pasting text with line breaks into a program that has not turned on
//! bracketed paste (each line would run the moment it arrives).

use crate::icons;
use crate::theme::{self, c, font, ty};
use egui::{Rect, RichText, Vec2};

#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// The program asked to put this text on the clipboard.
    Copy(String),
    /// The person pasted this text; it waits for confirmation.
    Paste(String),
}

impl Ask {
    fn message(&self) -> String {
        match self {
            Ask::Copy(text) => format!("프로그램이 클립보드에 복사하려고 해요 · {}자", text.chars().count()),
            Ask::Paste(text) => {
                format!("줄바꿈이 있어 줄마다 바로 실행될 수 있어요 · {}줄 붙여넣기", text.lines().count().max(1))
            }
        }
    }

    /// Labels of the accepting and refusing buttons.
    fn answers(&self) -> (&'static str, &'static str) {
        match self {
            Ask::Copy(_) => ("허용", "거절"),
            Ask::Paste(_) => ("붙여넣기", "취소"),
        }
    }

    fn preview(&self) -> String {
        let (Ask::Copy(text) | Ask::Paste(text)) = self;
        let short: String = text.chars().take(400).collect();
        if short.len() < text.len() { format!("{short}…") } else { short }
    }
}

/// A small bar in the bottom-right corner of `area`. Returns the answer once given.
pub fn show(ui: &mut egui::Ui, area: Rect, ask: &Ask) -> Option<bool> {
    let mut answer = None;
    let (yes, no) = ask.answers();
    egui::Area::new(ui.scope_id().with("terminal-ask"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(area.right_bottom() - Vec2::new(10.0, 10.0))
        .constrain_to(area)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(c::PANEL_2)
                .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
                .corner_radius(egui::CornerRadius::same(theme::size::RADIUS))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let glyph = if matches!(ask, Ask::Copy(_)) { icons::CLIPBOARD } else { icons::TRIANGLE_ALERT };
                        ui.label(theme::icon(glyph, 13.0, c::YELLOW));
                        ui.label(RichText::new(ask.message()).font(font(ty::SMALL)).color(c::TEXT))
                            .on_hover_text(ask.preview());
                        let r = theme::secondary_button(ui, yes);
                        super::verify::target(&format!("ask:{yes}"), r.rect);
                        if r.clicked() {
                            answer = Some(true);
                        }
                        let r = theme::link_button(ui, no);
                        super::verify::target(&format!("ask:{no}"), r.rect);
                        if r.clicked() {
                            answer = Some(false);
                        }
                    });
                });
        });
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_say_what_would_happen() {
        assert_eq!(Ask::Copy("hello".into()).message(), "프로그램이 클립보드에 복사하려고 해요 · 5자");
        assert_eq!(
            Ask::Paste("ls\nrm -rf x\n".into()).message(),
            "줄바꿈이 있어 줄마다 바로 실행될 수 있어요 · 2줄 붙여넣기"
        );
        assert!(Ask::Copy("가".repeat(500)).preview().ends_with('…'));
    }
}
