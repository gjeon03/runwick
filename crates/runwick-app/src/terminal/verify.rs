//! Verification aid for keyboard, IME and clipboard input in the real app.
//!
//! `RUNWICK_VERIFY_INPUT='step;step;…'` injects input events as if they came from the window
//! system, one step per frame (`a+b` puts several events in the same frame, as macOS delivers
//! an IME commit together with a key; write `\;` and `\+` for literal `;` and `+`). Steps:
//!
//! - `wait:<ms>` — pause the script
//! - `click:terminal` — press and release the primary button in the last drawn terminal
//!   (`click:doc` — in the last drawn document editor; `click:item:<label>` — on the last drawn
//!   menu item with that label). `rclick:` uses the secondary button; `shift-`/`ctrl-`/`cmd-`
//!   prefixes hold modifiers through the release (`shift-rclick:terminal`, `ctrl-click:terminal`)
//! - `text:<s>` — typed text; `paste:<s>` — Cmd+V with `<s>` on the clipboard; `copy` — Cmd+C
//! - `ime-preedit:<s>` (empty clears the composition), `ime-commit:<s>`
//! - `key:<name>` with optional `cmd-`, `ctrl-`, `alt-`, `shift-` prefixes (egui key names)
//! - `scroll:<points>` — mouse wheel over the middle of the window (positive scrolls down)
//! - `shot:<name>` — save `<RUNWICK_VERIFY_DIR>/<name>.png`
//! - `report` — write what the terminal sent and recorded to `<RUNWICK_VERIFY_DIR>/report.json`
//! - `exit` — close the window
//!
//! While a script runs, the terminal records the bytes it sends to the program, IME
//! pre-edit changes and copies (instead of replacing the person's clipboard).

use egui::{Event, ImeEvent, Key, Modifiers, PointerButton, RawInput, Rect};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static CTX: OnceLock<egui::Context> = OnceLock::new();
static LOG: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static TARGETS: Mutex<Vec<(String, Rect)>> = Mutex::new(Vec::new());

/// Record an observation while a script runs. Returns whether it was recorded.
pub fn record(kind: &str, value: &str) -> bool {
    if !ACTIVE.load(Ordering::Relaxed) {
        return false;
    }
    if let Ok(mut log) = LOG.lock() {
        log.push((kind.to_string(), value.to_string()));
    }
    true
}

/// Keep a running script moving while the window is covered. Called inside every pass (from
/// `App::logic`): repaint requests and screenshot commands made in the input hook, before the
/// pass, are lost when a covered window gets only logic passes, and the script would stall.
pub fn keep_alive() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let Some(ctx) = CTX.get() else { return };
    ctx.request_repaint_after(Duration::from_millis(16));
    if !ctx.input(|i| i.viewport().visible().unwrap_or(true)) {
        // A pending screenshot makes the next pass a full one, where the script's input lands.
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(|_| {})));
    }
}

/// A scripted-input run was asked for.
pub fn requested() -> bool {
    hook_env!("RUNWICK_VERIFY_INPUT").is_some()
}

/// Remember where a clickable item (a menu entry) was drawn, for `click:item:<label>`.
pub fn target(label: &str, rect: Rect) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut t) = TARGETS.lock() {
        t.retain(|(l, _)| l != label);
        t.push((label.to_string(), rect));
    }
}

fn target_rect(label: &str) -> Option<Rect> {
    TARGETS.lock().ok()?.iter().find(|(l, _)| l == label).map(|(_, r)| *r)
}

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Wait(Duration),
    Frame(Vec<Input>),
    Shot(String),
    Report,
    Exit,
}

#[derive(Debug, Clone, PartialEq)]
enum Input {
    Click(ClickTarget, PointerButton, Modifiers),
    Text(String),
    Paste(String),
    Copy,
    Preedit(String),
    Commit(String),
    Key(Key, Modifiers),
    Scroll(f32),
}

#[derive(Debug, Clone, PartialEq)]
enum ClickTarget {
    Terminal,
    Doc,
    Item(String),
}

pub struct Script {
    steps: VecDeque<Step>,
    resume_at: Instant,
    /// Key and button releases for the next frame.
    releases: Vec<Event>,
    /// Modifier releases for the frame after (a click with Shift or Ctrl ends with the button
    /// release while the modifier is still held, as with a real mouse).
    later: Vec<Event>,
    dir: PathBuf,
    /// Screenshots are being requested to keep a covered window painting.
    forcing: bool,
}

impl Script {
    pub fn from_env() -> Option<Self> {
        let spec = hook_env!("RUNWICK_VERIFY_INPUT")?;
        let dir = hook_env!("RUNWICK_VERIFY_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let steps = match parse(&spec) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("RUNWICK_VERIFY_INPUT: {e}");
                return None;
            }
        };
        ACTIVE.store(true, Ordering::Relaxed);
        Some(Self { steps, resume_at: Instant::now(), releases: Vec::new(), later: Vec::new(), dir, forcing: false })
    }

    /// Called from `App::raw_input_hook` before every frame.
    pub fn hook(&mut self, app: &crate::App, ctx: &egui::Context, raw: &mut RawInput) {
        let _ = CTX.set(ctx.clone());
        // Behave like a focused window so focus-dependent paths are exercised.
        raw.focused = true;
        ctx.request_repaint_after(Duration::from_millis(16));
        // A covered window gets logic-only passes and its input is saved for later, which would
        // deliver several steps in one frame. A pending screenshot forces full passes; inject
        // only once one is guaranteed.
        let visible = raw.viewport().visible().unwrap_or(true);
        if !visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(|_| {})));
            if !std::mem::replace(&mut self.forcing, true) {
                return;
            }
        } else {
            self.forcing = false;
        }
        raw.events.append(&mut self.releases);
        self.releases.append(&mut self.later);
        if Instant::now() < self.resume_at {
            return;
        }
        let Some(step) = self.steps.pop_front() else { return };
        match step {
            Step::Wait(d) => self.resume_at = Instant::now() + d,
            Step::Frame(inputs) => {
                for input in inputs {
                    self.inject(app, raw, input);
                }
            }
            Step::Shot(name) => {
                let path = self.dir.join(format!("{name}.png"));
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    if let Err(e) = save_png(&path, &img) {
                        eprintln!("verify screenshot failed: {e}");
                    }
                })));
                // Give the screenshot a couple of frames before the next step.
                self.resume_at = Instant::now() + Duration::from_millis(150);
            }
            Step::Report => {
                let log = LOG.lock().map(|l| l.clone()).unwrap_or_default();
                let json = serde_json::to_string_pretty(&log).unwrap_or_default();
                let _ = std::fs::create_dir_all(&self.dir);
                if let Err(e) = std::fs::write(self.dir.join("report.json"), json) {
                    eprintln!("verify report failed: {e}");
                }
            }
            Step::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn inject(&mut self, app: &crate::App, raw: &mut RawInput, input: Input) {
        match input {
            Input::Click(target, button, modifiers) => {
                let area = match &target {
                    ClickTarget::Terminal => crate::terminal::last_area(app),
                    ClickTarget::Doc => crate::views::docs::last_area(app),
                    ClickTarget::Item(label) => target_rect(label),
                };
                let Some(area) = area else {
                    eprintln!("verify: nothing to click ({target:?})");
                    return;
                };
                let pos = area.center();
                if modifiers != Modifiers::NONE {
                    raw.events.push(Event::ModifiersChanged(modifiers));
                    self.later.push(Event::ModifiersChanged(Modifiers::NONE));
                }
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(Event::PointerButton { pos, button, pressed: true, modifiers });
                self.releases.push(Event::PointerButton { pos, button, pressed: false, modifiers });
            }
            Input::Text(t) => raw.events.push(Event::Text(t)),
            Input::Paste(t) => raw.events.push(Event::Paste(t)),
            Input::Copy => raw.events.push(Event::Copy),
            Input::Preedit(text) => raw.events.push(Event::Ime(ImeEvent::Preedit { text, active_range_chars: None })),
            Input::Commit(t) => raw.events.push(Event::Ime(ImeEvent::Commit(t))),
            Input::Scroll(dy) => {
                let pos = raw.screen_rect.map(|r| r.center()).unwrap_or(egui::pos2(400.0, 300.0));
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -dy),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: Modifiers::NONE,
                });
            }
            Input::Key(key, modifiers) => {
                raw.events.push(Event::ModifiersChanged(modifiers));
                raw.events.push(Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers });
                self.releases.push(Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: false,
                    repeat: false,
                    modifiers,
                });
                self.releases.push(Event::ModifiersChanged(Modifiers::NONE));
            }
        }
    }
}

/// Split on `sep` unless it is escaped with a backslash. The escape before `sep` is removed;
/// an escaped other separator keeps its backslash for the next split.
fn split_escaped(s: &str, sep: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('\\', Some(n)) if n == sep => {
                cur.push(n);
                chars.next();
            }
            ('\\', Some(n @ (';' | '+'))) => {
                cur.push(c);
                cur.push(n);
                chars.next();
            }
            (c, _) if c == sep => parts.push(std::mem::take(&mut cur)),
            (c, _) => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

fn parse(spec: &str) -> Result<VecDeque<Step>, String> {
    let mut steps = VecDeque::new();
    for part in split_escaped(spec, ';').iter().filter(|p| !p.trim().is_empty()) {
        let part = part.trim_start();
        let (cmd, arg) = part.split_once(':').unwrap_or((part, ""));
        steps.push_back(match cmd {
            "wait" => Step::Wait(Duration::from_millis(arg.parse().map_err(|_| format!("bad wait: {arg}"))?)),
            "shot" => Step::Shot(arg.to_string()),
            "report" => Step::Report,
            "exit" => Step::Exit,
            _ => Step::Frame(split_escaped(part, '+').iter().map(|p| parse_input(p)).collect::<Result<_, _>>()?),
        });
    }
    Ok(steps)
}

fn parse_input(s: &str) -> Result<Input, String> {
    let (cmd, arg) = s.split_once(':').unwrap_or((s, ""));
    Ok(match cmd {
        "click" | "rclick" => click(cmd, arg, Modifiers::NONE)?,
        "text" => Input::Text(arg.to_string()),
        "paste" => Input::Paste(arg.to_string()),
        "copy" => Input::Copy,
        "scroll" => Input::Scroll(arg.parse().map_err(|_| format!("bad scroll: {arg}"))?),
        "ime-preedit" => Input::Preedit(arg.to_string()),
        "ime-commit" => Input::Commit(arg.to_string()),
        "key" => {
            let (mods, name) = modifiers(arg)?;
            Input::Key(Key::from_name(name).ok_or_else(|| format!("bad key: {name}"))?, mods)
        }
        other => match modifiers(other)? {
            (mods, name @ ("click" | "rclick")) => click(name, arg, mods)?,
            _ => return Err(format!("bad step: {other}")),
        },
    })
}

/// `click` / `rclick` on `terminal`, `doc` or `item:<label>`.
fn click(cmd: &str, arg: &str, mods: Modifiers) -> Result<Input, String> {
    let target = match arg {
        "" | "terminal" => ClickTarget::Terminal,
        "doc" => ClickTarget::Doc,
        _ => match arg.strip_prefix("item:") {
            Some(label) if !label.is_empty() => ClickTarget::Item(label.to_string()),
            _ => return Err(format!("bad click target: {arg}")),
        },
    };
    let button = if cmd == "rclick" { PointerButton::Secondary } else { PointerButton::Primary };
    Ok(Input::Click(target, button, mods))
}

/// Leading `cmd-`, `ctrl-`, `alt-`, `shift-` prefixes and the rest.
fn modifiers(mut name: &str) -> Result<(Modifiers, &str), String> {
    let mut mods = Modifiers::NONE;
    while let Some((m, rest)) = name.split_once('-').filter(|(m, rest)| !m.is_empty() && !rest.is_empty()) {
        match m {
            "cmd" => {
                mods.command = true;
                mods.mac_cmd = cfg!(target_os = "macos");
                mods.ctrl |= !cfg!(target_os = "macos");
            }
            "ctrl" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            other => return Err(format!("bad modifier: {other}")),
        }
        name = rest;
    }
    Ok((mods, name))
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    image::save_buffer(path, &rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps_frames_and_keys() {
        let steps = parse("wait:100;click:terminal;ime-preedit:+ime-commit:한;key:ctrl-C;key:cmd-A;shot:x").unwrap();
        assert_eq!(steps[0], Step::Wait(Duration::from_millis(100)));
        let primary = |t| Input::Click(t, PointerButton::Primary, Modifiers::NONE);
        assert_eq!(steps[1], Step::Frame(vec![primary(ClickTarget::Terminal)]));
        assert_eq!(parse("click:doc").unwrap()[0], Step::Frame(vec![primary(ClickTarget::Doc)]));
        assert_eq!(
            parse("click:item:핀으로 고정").unwrap()[0],
            Step::Frame(vec![primary(ClickTarget::Item("핀으로 고정".into()))])
        );
        let shift = Modifiers { shift: true, ..Default::default() };
        assert_eq!(
            parse("shift-rclick:terminal").unwrap()[0],
            Step::Frame(vec![Input::Click(ClickTarget::Terminal, PointerButton::Secondary, shift)])
        );
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(
            parse("ctrl-click:terminal").unwrap()[0],
            Step::Frame(vec![Input::Click(ClickTarget::Terminal, PointerButton::Primary, ctrl)])
        );
        assert!(parse("click:item:").is_err());
        assert!(parse("shift-tap:terminal").is_err());
        assert_eq!(steps[2], Step::Frame(vec![Input::Preedit(String::new()), Input::Commit("한".into())]));
        assert_eq!(steps[3], Step::Frame(vec![Input::Key(Key::C, Modifiers { ctrl: true, ..Default::default() })]));
        assert!(matches!(&steps[4], Step::Frame(v) if matches!(v[0], Input::Key(Key::A, m) if m.command)));
        assert_eq!(steps[5], Step::Shot("x".into()));
        assert!(parse("key:hyper-A").is_err());
        let steps = parse("text:a\\;b\\+c '\\e';shot:y").unwrap();
        assert_eq!(steps[0], Step::Frame(vec![Input::Text("a;b+c '\\e'".into())]));
    }
}
