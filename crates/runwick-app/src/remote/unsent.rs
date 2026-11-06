//! Input typed into a device's session while the device was unreachable ("보내지 않은 입력").
//! It is kept on this computer and never sent by itself: once the connection is back the
//! person sees the session's current state and chooses to send it or throw it away.

use crate::client::HostKey;
use runwick_core::api::sessions::SessionId;
use std::collections::HashMap;
use std::time::Instant;

pub type Key = (HostKey, SessionId);

#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    /// What was typed, with `\n` for Enter.
    pub text: String,
    pub since: Instant,
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    Dropped,
}

#[derive(Default)]
pub struct Unsent {
    held: HashMap<Key, Held>,
}

/// What a typed key does to the held text.
#[derive(Debug, Clone, PartialEq)]
pub enum Typed {
    Text(String),
    Backspace,
    Enter,
}

/// Keys and text from one frame's events that count as typing.
pub fn typed(events: &[egui::Event]) -> Vec<Typed> {
    let mut out = Vec::new();
    for ev in events {
        match ev {
            egui::Event::Text(t) => out.push(Typed::Text(t.clone())),
            egui::Event::Paste(t) => out.push(Typed::Text(t.clone())),
            egui::Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_empty() && t != "\n" && t != "\r" => {
                out.push(Typed::Text(t.clone()))
            }
            egui::Event::Key { key: egui::Key::Backspace, pressed: true, modifiers, .. } if !modifiers.command => {
                out.push(Typed::Backspace)
            }
            egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. } if !modifiers.command => {
                out.push(Typed::Enter)
            }
            _ => {}
        }
    }
    out
}

impl Unsent {
    /// Add keys typed while disconnected. Returns true when something changed.
    pub fn capture(&mut self, key: Key, keys: &[Typed]) -> bool {
        if keys.is_empty() {
            return false;
        }
        let h =
            self.held.entry(key).or_insert_with(|| Held { text: String::new(), since: Instant::now(), outcome: None });
        if h.outcome.is_some() {
            // A new draft after the last one was sent or dropped.
            *h = Held { text: String::new(), since: Instant::now(), outcome: None };
        }
        for k in keys {
            match k {
                Typed::Text(t) => h.text.push_str(t),
                Typed::Enter => h.text.push('\n'),
                Typed::Backspace => {
                    h.text.pop();
                }
            }
        }
        if h.text.is_empty() {
            self.held.remove(&key);
        }
        true
    }

    pub fn get(&self, key: &Key) -> Option<&Held> {
        self.held.get(key)
    }

    /// Held text waiting for a decision (not yet sent or dropped).
    pub fn pending(&self, key: &Key) -> Option<&str> {
        self.held.get(key).filter(|h| h.outcome.is_none()).map(|h| h.text.as_str())
    }

    /// The person chose to send: the bytes for the terminal (Enter as CR). Only this returns
    /// bytes; nothing else in this module produces input.
    pub fn take_for_send(&mut self, key: &Key) -> Option<Vec<u8>> {
        let h = self.held.get_mut(key).filter(|h| h.outcome.is_none())?;
        h.outcome = Some(Outcome::Sent);
        Some(h.text.replace('\n', "\r").into_bytes())
    }

    pub fn discard(&mut self, key: &Key) {
        if let Some(h) = self.held.get_mut(key) {
            h.outcome = Some(Outcome::Dropped);
        }
    }

    /// Hide the note about a sent or dropped draft.
    pub fn dismiss(&mut self, key: &Key) {
        if self.held.get(key).is_some_and(|h| h.outcome.is_some()) {
            self.held.remove(key);
        }
    }

    /// Forget everything for a device (it was removed).
    pub fn forget_host(&mut self, host: HostKey) {
        self.held.retain(|(h, _), _| *h != host);
    }
}

/// One line for display: Enter shown as ⏎, long text cut in the middle.
pub fn preview(text: &str) -> String {
    let one = text.replace('\n', " ⏎ ");
    let chars: Vec<char> = one.chars().collect();
    if chars.len() <= 80 {
        return one.trim_end().to_string();
    }
    let head: String = chars[..50].iter().collect();
    let tail: String = chars[chars.len() - 24..].iter().collect();
    format!("{head} … {tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn key() -> Key {
        (HostKey::Device(Uuid::now_v7()), Uuid::now_v7())
    }

    #[test]
    fn input_while_disconnected_is_held_and_never_sent_by_itself() {
        let mut u = Unsent::default();
        let k = key();
        let ev = [
            egui::Event::Text("통합 테스트도".into()),
            egui::Event::Ime(egui::ImeEvent::Commit(" 같이".into())),
            egui::Event::Text(" 돌려줘x".into()),
            egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        assert!(u.capture(k, &typed(&ev)));
        assert_eq!(u.pending(&k), Some("통합 테스트도 같이 돌려줘\n"));
        // Reconnecting changes nothing here: the draft waits for the person.
        assert_eq!(u.pending(&k), Some("통합 테스트도 같이 돌려줘\n"));
        assert_eq!(preview(u.pending(&k).unwrap()), "통합 테스트도 같이 돌려줘 ⏎");
        // Sending is explicit and happens once.
        assert_eq!(u.take_for_send(&k).unwrap(), "통합 테스트도 같이 돌려줘\r".as_bytes());
        assert!(u.take_for_send(&k).is_none());
        assert_eq!(u.get(&k).unwrap().outcome, Some(Outcome::Sent));
        u.dismiss(&k);
        assert!(u.get(&k).is_none());
    }

    #[test]
    fn discarded_drafts_cannot_be_sent_and_erasing_everything_forgets() {
        let mut u = Unsent::default();
        let k = key();
        u.capture(k, &[Typed::Text("rm".into())]);
        u.discard(&k);
        assert!(u.take_for_send(&k).is_none());
        assert!(u.pending(&k).is_none());
        // Typing again starts a new draft.
        u.capture(k, &[Typed::Text("ls".into())]);
        assert_eq!(u.pending(&k), Some("ls"));
        u.capture(k, &[Typed::Backspace, Typed::Backspace]);
        assert!(u.get(&k).is_none());
        u.capture(k, &[Typed::Text("x".into())]);
        u.forget_host(k.0);
        assert!(u.get(&k).is_none());
        assert!(!u.capture(k, &[]));
    }

    #[test]
    fn long_preview_is_cut() {
        let p = preview(&"가".repeat(200));
        assert!(p.contains(" … ") && p.chars().count() < 90);
    }
}
