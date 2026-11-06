//! "화면에서 찾기": search in one terminal's screen and scrollback (Cmd+F while the terminal has
//! focus). Separate from the conversation archive search, and named differently.

use super::model::{ClientTerm, Events};
use crate::icons;
use crate::theme::{self, c, font, size, ty};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use egui::{Key, Modifiers, Rect, Vec2};
use std::time::{Duration, Instant};

const MAX_MATCHES: usize = 5000;
const REFRESH: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub query: String,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    /// Content version and query the matches were computed for.
    computed: Option<(u64, String)>,
    computed_at: Option<Instant>,
    focus_field: bool,
}

pub enum Outcome {
    None,
    Closed,
}

impl Find {
    pub fn open(&mut self) {
        self.open = true;
        self.focus_field = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.current = None;
        self.computed = None;
    }

    /// Recompute matches when the query or the content changed (at most every 250 ms while
    /// output streams). Returns when a refresh is next due.
    pub fn refresh(&mut self, ct: &ClientTerm) -> Option<Instant> {
        if !self.open {
            return None;
        }
        let key = (ct.version, self.query.clone());
        if self.computed.as_ref() == Some(&key) {
            return None;
        }
        let query_changed = self.computed.as_ref().is_none_or(|(_, q)| *q != self.query);
        if !query_changed
            && let Some(at) = self.computed_at
            && at.elapsed() < REFRESH
        {
            return Some(at + REFRESH);
        }
        let keep = self.current.and_then(|i| self.matches.get(i).cloned());
        self.matches = find_all(&ct.term, &self.query).unwrap_or_default();
        self.computed = Some(key);
        self.computed_at = Some(Instant::now());
        self.current = match keep {
            Some(m) if !query_changed => self.matches.iter().position(|x| *x == m),
            // A new query starts from the match nearest the bottom (latest output).
            _ => self.matches.len().checked_sub(1),
        };
        None
    }

    pub fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.current = Some(match self.current {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => n - 1,
        });
    }

    pub fn current_match(&self) -> Option<&Match> {
        self.current.and_then(|i| self.matches.get(i))
    }

    /// The bar in the top-right corner of `area`. Returns whether it closed.
    pub fn show(&mut self, ui: &mut egui::Ui, area: Rect) -> (Outcome, bool) {
        let mut moved = false;
        let w = 300.0f32.min(area.width() - 16.0);
        let bar = Rect::from_min_size(egui::pos2(area.right() - w - 8.0, area.top() + 8.0), Vec2::new(w, 34.0));
        let mut outcome = Outcome::None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            egui::Frame::new()
                .fill(c::PANEL_2)
                .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
                .corner_radius(egui::CornerRadius::same(size::RADIUS))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(theme::icon(icons::SEARCH, 13.0, c::TEXT_3));
                        let edit = egui::TextEdit::singleline(&mut self.query)
                            .hint_text("화면에서 찾기")
                            .font(font(ty::SMALL))
                            .frame(egui::Frame::NONE)
                            .desired_width(w - 150.0);
                        let resp = ui.add(edit);
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "화면에서 찾기"));
                        if self.focus_field {
                            resp.request_focus();
                            self.focus_field = false;
                        }
                        if resp.has_focus() || resp.lost_focus() {
                            let (enter, shift, esc) = ui
                                .input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
                            if esc {
                                ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                                outcome = Outcome::Closed;
                            } else if enter {
                                self.step(!shift);
                                moved = true;
                                resp.request_focus();
                            }
                        }
                        let label = if self.query.is_empty() {
                            String::new()
                        } else if self.matches.is_empty() {
                            "결과 없음".to_string()
                        } else {
                            format!("{}/{}", self.current.map_or(0, |i| i + 1), self.matches.len())
                        };
                        ui.label(egui::RichText::new(label).font(font(ty::SMALL)).color(c::TEXT_3));
                        if theme::icon_button(ui, icons::CHEVRON_UP, "이전 결과 (Shift+Enter)").clicked() {
                            self.step(false);
                            moved = true;
                        }
                        if theme::icon_button(ui, icons::CHEVRON_DOWN, "다음 결과 (Enter)").clicked() {
                            self.step(true);
                            moved = true;
                        }
                        if theme::icon_button(ui, icons::X, "찾기 닫기 (Esc)").clicked() {
                            outcome = Outcome::Closed;
                        }
                    });
                });
        });
        if matches!(outcome, Outcome::Closed) {
            self.close();
        }
        (outcome, moved)
    }
}

/// Literal, smart-case search over history and screen.
fn find_all(term: &Term<Events>, query: &str) -> Option<Vec<Match>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut regex = RegexSearch::new(&escape(query)).ok()?;
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    Some(RegexIter::new(start, end, Direction::Right, term, &mut regex).take(MAX_MATCHES).collect())
}

fn escape(q: &str) -> String {
    let mut s = String::with_capacity(q.len() * 2);
    for ch in q.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(ch) {
            s.push('\\');
        }
        s.push(ch);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::GridSize;

    #[test]
    fn finds_literal_text_in_history_and_screen() {
        let mut ct = ClientTerm::new(GridSize { cols: 20, rows: 3 });
        for i in 0..10 {
            ct.feed(format!("line {i} a.b 한글\r\n").as_bytes());
        }
        let mut f = Find::default();
        f.open();
        f.query = "a.b".into();
        f.refresh(&ct);
        assert_eq!(f.matches.len(), 10, "a literal dot, not any character");
        assert_eq!(f.current, Some(9));
        f.step(true);
        assert_eq!(f.current, Some(0));
        f.query = "한글".into();
        f.refresh(&ct);
        assert_eq!(f.matches.len(), 10);
        f.query = "LINE".into();
        f.refresh(&ct);
        assert!(f.matches.is_empty(), "upper case makes the search case-sensitive");
    }
}
