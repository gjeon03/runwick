//! Settings › 백업·동기화 (S14). The screens live in `views::backup`.

use crate::App;

#[derive(Default)]
pub struct State {}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    crate::views::backup::show_page(app, ui);
}
