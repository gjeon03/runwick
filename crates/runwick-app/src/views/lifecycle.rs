//! Quitting and coming back (Quit.dc.html, Reboot.dc.html).
//!
//! - ⌘Q opens `작업까지 종료` when managed runs are alive (otherwise the window just closes):
//!   it shows the runs that will stop (grouped with the session that delegated them) and whether
//!   each can be resumed, asks the host to stop them and closes once the host confirmed.
//!   `창만 닫기` closes the window; runs keep going. The menu's own "Quit" item (mouse) keeps
//!   closing the window only — the runs survive either way.
//! - After a reboot (runs that were alive when the host went away): choose which sessions to
//!   resume. Nothing runs by itself; an agent resumes its saved conversation, a shell starts anew,
//!   and a session without a saved conversation id can only be read.

use crate::App;
use crate::client::{EVENT_CONN_READY, HostKey};
use crate::shell::{data, dialog, dialog_title};
use crate::state::{Modal, TabKind};
use crate::theme::{self, Tone, c, font, medium};
use egui::{CornerRadius, Margin, RichText};
use runwick_core::api::host::{HostShutdown, Leftover, ShutdownParams};
use runwick_core::api::records::{CaptureStatus, RecordsStatus, StatusQuery};
use runwick_core::api::sessions::{
    Restart, SessionId, SessionInfo, SessionUpdate, SessionsRestart, SessionsUpdate, Tool,
};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Default)]
pub struct State {
    /// Look for interrupted sessions once this computer's host answers.
    check_resume: bool,
    offered: bool,
    stopping: bool,
    stopped: Option<String>,
    resume_pick: Option<HashSet<SessionId>>,
    /// ⌘Q was taken away from the application menu so the window sees it.
    quit_key_freed: bool,
    /// After 작업까지 종료: runs the host could not stop completely (title, live processes).
    leftovers: Option<Vec<(String, Vec<String>)>>,
}

pub fn on_event(app: &mut App, host: HostKey, topic: &str, _data: &Value) {
    if host == HostKey::Local && topic == EVENT_CONN_READY && !app.feat.lifecycle.offered {
        app.feat.lifecycle.check_resume = true;
    }
}

/// Every frame, before widgets see the keys: ⌘Q, and the resume choice once after connecting.
pub fn tick(app: &mut App, ctx: &egui::Context, composing: bool) {
    if !app.feat.lifecycle.quit_key_freed {
        app.feat.lifecycle.quit_key_freed = true;
        free_quit_key();
    }
    let quit = !composing
        && ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Q)));
    if quit {
        request_quit(app, ctx);
    }
    offer_resume(app);
}

/// ⌘Q: ask when managed runs are alive or documents are unsaved; otherwise close the window.
fn request_quit(app: &mut App, ctx: &egui::Context) {
    if matches!(app.ui.modal, Some(Modal::Quit | Modal::Resume)) {
        return;
    }
    let alive = match data::all_sessions(app, HostKey::Local).data {
        Some(list) => {
            list.iter().any(|s| data::status(&s.session, data::live_of(app, HostKey::Local, s).as_ref()).alive)
        }
        // Not known yet (host not answering): the dialog says what it can.
        None => true,
    };
    if alive || !unsaved_docs(app).is_empty() {
        app.ui.modal = Some(Modal::Quit);
    } else {
        crate::views::docs::save_drafts(app);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The application menu's "Quit" item owns ⌘Q and would end the app before the window sees the
/// key; leave the item (mouse) and give the key to the window.
#[cfg(target_os = "macos")]
fn free_quit_key() {
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(menu) = app.mainMenu().and_then(|m| m.itemAtIndex(0)).and_then(|i| i.submenu()) else {
        return;
    };
    for i in 0..menu.numberOfItems() {
        if let Some(item) = menu.itemAtIndex(i)
            && item.action() == Some(sel!(terminate:))
        {
            item.setKeyEquivalent(&NSString::from_str(""));
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn free_quit_key() {}

/// Open the resume choice by itself only for interrupted sessions it was not shown for yet
/// (`UiState::resume_seen`, saved with the layout, so it is read only after the layout is back).
fn offer_resume(app: &mut App) {
    if !app.feat.lifecycle.check_resume || app.ui.modal.is_some() || !crate::shell::layout::is_restored(app) {
        return;
    }
    let Some(list) = data::all_sessions(app, HostKey::Local).data else {
        return;
    };
    app.feat.lifecycle.check_resume = false;
    app.feat.lifecycle.offered = true;
    let pending = interrupted_ids(&list);
    app.ui.resume_seen.retain(|id| pending.contains(id));
    if pending.iter().any(|id| !app.ui.resume_seen.contains(id)) {
        app.ui.modal = Some(Modal::Resume);
    }
}

fn interrupted_ids(list: &[SessionInfo]) -> Vec<SessionId> {
    list.iter().filter(|s| s.session.interrupted && !s.session.archived).map(|s| s.session.id).collect()
}

/// Why runs that were alive are gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The computer started after they were last active.
    Reboot,
    /// The computer kept running: the host stopped without stopping its runs (crash, kill).
    HostStopped,
    /// The start time of the computer could not be read.
    Unknown,
}

/// `boot`: when the computer started; `last`: when the interrupted runs were last active
/// (seconds since the epoch).
fn why_ended(boot: Option<i64>, last: Option<i64>) -> Ended {
    match (boot, last) {
        (Some(b), Some(l)) if b > l => Ended::Reboot,
        (Some(_), Some(_)) => Ended::HostStopped,
        _ => Ended::Unknown,
    }
}

/// When this computer started (`kern.boottime`; `btime` in /proc/stat on Linux), read once.
fn boot_time() -> Option<i64> {
    static BOOT: std::sync::OnceLock<Option<i64>> = std::sync::OnceLock::new();
    *BOOT.get_or_init(|| {
        if cfg!(target_os = "macos") {
            let out = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "kern.boottime"]).output().ok()?;
            parse_boottime(&String::from_utf8_lossy(&out.stdout))
        } else {
            let stat = std::fs::read_to_string("/proc/stat").ok()?;
            stat.lines().find_map(|l| l.strip_prefix("btime ")).and_then(|v| v.trim().parse().ok())
        }
    })
}

/// `{ sec = 1793955586, usec = 805328 } Fri Nov  6 17:59:46 2026` → 1793955586.
fn parse_boottime(text: &str) -> Option<i64> {
    let rest = &text[text.find("sec = ")? + "sec = ".len()..];
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// What the resume choice says happened.
fn ended_line(ended: Ended) -> &'static str {
    match ended {
        Ended::Reboot => {
            "컴퓨터가 다시 시작돼 실행은 끝났어요. 화면과 기록은 복원했고, 어떤 명령도 자동으로 실행하지 않았습니다."
        }
        Ended::HostStopped => {
            "Runwick 호스트가 예기치 않게 종료돼 실행이 끝났어요. 화면과 기록은 남아 있고, 어떤 명령도 자동으로 실행하지 않았습니다."
        }
        Ended::Unknown => {
            "Runwick이 다시 시작되기 전에 실행 중이던 세션이에요. 실행은 끝났고, 어떤 명령도 자동으로 실행하지 않았습니다."
        }
    }
}

/// Paths of open documents with unsaved edits.
fn unsaved_docs(app: &mut App) -> Vec<String> {
    let docs: Vec<(crate::state::WsKey, String)> = app
        .ui
        .tabsets
        .values()
        .flat_map(|s| s.tabs.iter())
        .filter_map(|t| match &t.kind {
            TabKind::Doc { ws, path } => Some((*ws, path.clone())),
            _ => None,
        })
        .collect();
    docs.into_iter().filter(|(ws, path)| crate::views::docs::is_dirty(app, *ws, path)).map(|(_, p)| p).collect()
}

/// How a stopped or interrupted session can come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comeback {
    /// An agent whose saved conversation exists: `--resume` continues it.
    SameConversation,
    /// A shell, or an agent without a saved conversation (it never got past its first screen):
    /// only a new start is possible. Resuming would fail.
    Fresh,
    /// The conversation records are not loaded yet.
    Checking,
}

/// What the records say about each session's conversation (one query for all sessions).
fn capture_statuses(app: &mut App) -> Option<Vec<CaptureStatus>> {
    app.hub.query::<RecordsStatus>(HostKey::Local, &StatusQuery::default()).data.map(|l| l.as_ref().clone())
}

/// Resume only what really exists: an agent with a conversation id whose messages the records
/// hold. An id alone is not enough (Claude gets one at launch, before anything is saved).
fn comeback(s: &SessionInfo, statuses: Option<&[CaptureStatus]>) -> Comeback {
    if !s.session.tool.is_agent() || s.session.provider_conv_id.is_none() {
        return Comeback::Fresh;
    }
    let Some(list) = statuses else {
        return Comeback::Checking;
    };
    match list.iter().find(|c| c.session_id == s.session.id) {
        Some(c) if c.message_count > 0 => Comeback::SameConversation,
        _ => Comeback::Fresh,
    }
}

/// Resume possibility after stopping, in words.
fn after_stop(s: &SessionInfo, statuses: Option<&[CaptureStatus]>) -> &'static str {
    match (s.session.tool, comeback(s, statuses)) {
        (Tool::Shell, _) => "다시 시작만 가능",
        (_, Comeback::SameConversation) => "나중에 재개 가능",
        (_, Comeback::Fresh) => "새로 시작만 가능",
        (_, Comeback::Checking) => "재개 가능 여부 확인 중",
    }
}

fn ws_name(app: &mut App, s: &SessionInfo) -> String {
    let p = crate::views::collab::place(app, HostKey::Local, &s.session);
    format!("{} / {}", p.project, p.workspace)
}

/// A run in the quit dialog: the session, its line under the title, its status tone and pulse.
type QuitRow = (SessionInfo, String, Tone, bool);

enum QuitAct {
    StopAll,
    WindowOnly,
    Cancel,
}

pub fn show_quit_modal(app: &mut App, ctx: &egui::Context) -> bool {
    if let Some(left) = app.feat.lifecycle.leftovers.clone() {
        return show_leftovers(app, ctx, &left);
    }
    let q = data::all_sessions(app, HostKey::Local);
    // Without an answer from the host nothing can be stopped from here; the window can close.
    let known = q.data.is_some();
    let list: Vec<SessionInfo> = q.data.map(|l| l.as_ref().clone()).unwrap_or_default();
    let statuses = capture_statuses(app);
    let running: Vec<SessionInfo> = list
        .iter()
        .filter(|s| data::status(&s.session, data::live_of(app, HostKey::Local, s).as_ref()).alive)
        .cloned()
        .collect();
    let ids: HashSet<SessionId> = running.iter().map(|s| s.session.id).collect();
    // Groups: a running session with running workers, then everything else.
    let mut groups: Vec<(String, Vec<SessionInfo>)> = Vec::new();
    let mut alone: Vec<SessionInfo> = Vec::new();
    for s in running.iter().filter(|s| s.session.parent_id.is_none_or(|p| !ids.contains(&p))) {
        let kids: Vec<SessionInfo> =
            running.iter().filter(|w| w.session.parent_id == Some(s.session.id)).cloned().collect();
        if kids.is_empty() {
            alone.push(s.clone());
        } else {
            let mut g = vec![s.clone()];
            g.extend(kids);
            groups.push((format!("{}과 연결된 실행", s.session.title), g));
        }
    }
    if !alone.is_empty() {
        groups.push(("독립 실행".into(), alone));
    }
    let rows: Vec<(String, Vec<QuitRow>)> = groups
        .into_iter()
        .map(|(t, g)| {
            let g = g
                .into_iter()
                .map(|s| {
                    let st = data::status(&s.session, data::live_of(app, HostKey::Local, &s).as_ref());
                    let meta = format!("{} · {} · {}", s.session.tool.label(), ws_name(app, &s), st.label);
                    (s, meta, st.tone, st.pulse)
                })
                .collect();
            (t, g)
        })
        .collect();
    let unsaved = unsaved_docs(app);
    let st = &app.feat.lifecycle;
    let stopping = st.stopping;
    let stopped = st.stopped.clone();
    let mut act = None;
    let ((), close) = dialog(ctx, "quit", 560.0, |ui| {
        if !known {
            dialog_title(
                ui,
                "실행 상태를 확인하지 못했어요",
                Some("호스트가 응답하지 않아 실행을 멈출 수 없어요. 창만 닫으면 실행 중인 작업은 그대로 계속돼요."),
            );
        } else {
            let title = if running.is_empty() {
                "Runwick을 종료할까요?".to_string()
            } else {
                format!("실행 {}개를 종료할까요?", running.len())
            };
            dialog_title(
                ui,
                &title,
                Some("창만 닫으면 모두 계속 실행돼요. 대화와 기록은 어느 쪽이든 지워지지 않습니다."),
            );
        }
        if known && running.is_empty() {
            ui.label(RichText::new("실행 중인 작업이 없어요.").font(font(12.5)).color(c::TEXT_2));
        }
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (title, group) in &rows {
                ui.label(RichText::new(title).font(medium(12.0)).color(c::TEXT_3));
                egui::Frame::new().fill(c::CONTROL).corner_radius(CornerRadius::same(8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (s, meta, tone, pulse) in group {
                        egui::Frame::new().inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                theme::status_dot(ui, *tone, *pulse);
                                let after = after_stop(s, statuses.as_deref());
                                let w = (ui.available_width() - 110.0).max(80.0);
                                ui.vertical(|ui| {
                                    ui.set_width(w);
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    theme::truncated(ui, &s.session.title, medium(13.0), c::TEXT_STRONG);
                                    theme::truncated(ui, meta, font(12.0), c::TEXT_2);
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(RichText::new(after).font(font(12.0)).color(c::TEXT_2));
                                });
                            });
                        });
                    }
                });
                ui.add_space(6.0);
            }
        });
        if !unsaved.is_empty() {
            ui.label(
                RichText::new(format!("저장하지 않은 문서 {}는 복원용으로 보관합니다.", unsaved.join(", ")))
                    .font(font(12.5))
                    .color(c::TEXT),
            );
        }
        if !running.is_empty() {
            ui.label(RichText::new("실제로 멈췄는지 확인한 뒤에 종료로 표시해요.").font(font(12.5)).color(c::TEXT));
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "CLI가 따로 띄운 공유 서버(예: Codex 백그라운드 서버)는 다른 창과 함께 쓰므로 멈추지 않아요.",
                    )
                    .font(font(12.0))
                    .color(c::TEXT_3),
                )
                .wrap(),
            );
        }
        if let Some(s) = &stopped {
            ui.label(RichText::new(s).font(font(12.5)).color(c::MINT));
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if stopping {
                "멈추는 중…"
            } else if running.is_empty() {
                "종료"
            } else {
                "작업까지 종료"
            };
            let b = egui::Button::new(
                RichText::new(label).font(medium(12.5)).color(egui::Color32::from_rgb(0x2B, 0x0A, 0x0E)),
            )
            .fill(c::ROSE)
            .min_size(egui::vec2(0.0, 32.0))
            .corner_radius(CornerRadius::same(7));
            if known && ui.add_enabled(!stopping, b).clicked() {
                act = Some(QuitAct::StopAll);
            }
            if !running.is_empty() || !known {
                let w = egui::Button::new(RichText::new("창만 닫기").font(medium(12.5)).color(c::TEXT_STRONG))
                    .fill(c::LINE_STRONG)
                    .min_size(egui::vec2(0.0, 32.0))
                    .corner_radius(CornerRadius::same(7));
                if ui
                    .add_enabled(!stopping, w)
                    .on_hover_text("실행은 계속돼요. 다시 열면 같은 실행에 연결돼요")
                    .clicked()
                {
                    act = Some(QuitAct::WindowOnly);
                }
            }
            if ui.add_enabled(!stopping, egui::Button::new("취소").min_size(egui::vec2(0.0, 32.0))).clicked() {
                act = Some(QuitAct::Cancel);
            }
        });
    });
    match act {
        Some(QuitAct::StopAll) if !running.is_empty() => stop_and_quit(app, ctx),
        Some(QuitAct::StopAll | QuitAct::WindowOnly) => {
            crate::views::docs::save_drafts(app);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        Some(QuitAct::Cancel) => {
            app.feat.lifecycle.stopped = None;
            return false;
        }
        None => {}
    }
    stopping || !close
}

/// Runs the host could not stop completely, by session title (or a plain word when unknown).
fn leftover_rows(left: &[Leftover], sessions: &[SessionInfo]) -> Vec<(String, Vec<String>)> {
    left.iter()
        .map(|l| {
            let title = sessions
                .iter()
                .find(|s| s.session.id == l.session)
                .map(|s| s.session.title.clone())
                .unwrap_or_else(|| "이름을 확인할 수 없는 세션".into());
            (title, l.processes.clone())
        })
        .collect()
}

/// After 작업까지 종료 left processes alive: which runs were not stopped and what is still running.
fn show_leftovers(app: &mut App, ctx: &egui::Context, left: &[(String, Vec<String>)]) -> bool {
    let mut done = false;
    let ((), close) = dialog(ctx, "quit-leftovers", 560.0, |ui| {
        dialog_title(
            ui,
            &format!("실행 {}개를 다 멈추지 못했어요", left.len()),
            Some(
                "아래 프로세스가 아직 살아 있어요. 활동 모니터나 터미널에서 직접 종료해 주세요. 대화와 기록은 지워지지 않았어요.",
            ),
        );
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (title, processes) in left {
                egui::Frame::new()
                    .fill(c::CONTROL)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            theme::status_dot(ui, Tone::Recover, false);
                            theme::truncated(ui, title, medium(13.0), c::TEXT_STRONG);
                            ui.label(RichText::new("멈추지 못함").font(font(12.0)).color(Tone::Recover.fg()));
                        });
                        for p in processes {
                            ui.label(RichText::new(p).font(crate::theme::mono(12.0)).color(c::TEXT_2));
                        }
                    });
                ui.add_space(6.0);
            }
        });
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::secondary_button(ui, "창 닫기").clicked() {
                done = true;
            }
        });
    });
    if done || close {
        app.feat.lifecycle.leftovers = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return false;
    }
    true
}

fn stop_and_quit(app: &mut App, ctx: &egui::Context) {
    app.feat.lifecycle.stopping = true;
    let ctx = ctx.clone();
    app.hub.call::<HostShutdown>(HostKey::Local, ShutdownParams { stop_runs: true }, move |app, res| {
        app.feat.lifecycle.stopping = false;
        match res {
            Ok(r) if !r.leftovers.is_empty() => {
                // Not everything stopped: say which runs, by name, and keep the window open.
                let titles = data::all_sessions(app, HostKey::Local).data;
                let named = leftover_rows(&r.leftovers, titles.as_deref().map(|v| v.as_slice()).unwrap_or(&[]));
                app.hub.disconnect(HostKey::Local);
                app.feat.lifecycle.leftovers = Some(named);
                crate::views::docs::save_drafts(app);
            }
            Ok(r) => {
                // Do not reconnect (that would start a new host); the window closes now.
                app.hub.disconnect(HostKey::Local);
                app.feat.lifecycle.stopped =
                    Some(format!("실행 {}개를 멈췄어요 (강제 종료 {}개)", r.stopped.len(), r.killed.len()));
                crate::views::docs::save_drafts(app);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => app.toast_error(&format!("작업을 멈추지 못했어요: {}", e.message)),
        }
    });
}

/// How an interrupted session comes back.
/// How an interrupted session comes back: words, color, and whether it can be picked.
fn how(s: &SessionInfo, statuses: Option<&[CaptureStatus]>) -> (&'static str, egui::Color32, bool) {
    match (s.session.tool, comeback(s, statuses)) {
        (Tool::Shell, _) => ("새 셸로 시작", c::TEXT_2, true),
        (_, Comeback::SameConversation) => ("같은 대화 재개", c::MINT, true),
        (_, Comeback::Fresh) => ("저장된 대화가 없어 새로 시작해요", c::TEXT_2, true),
        (_, Comeback::Checking) => ("대화 확인 중…", c::TEXT_3, false),
    }
}

pub fn show_resume_modal(app: &mut App, ctx: &egui::Context) -> bool {
    let Some(list) = data::all_sessions(app, HostKey::Local).data else {
        return true;
    };
    let items: Vec<SessionInfo> =
        list.iter().filter(|s| s.session.interrupted && !s.session.archived).cloned().collect();
    if items.is_empty() {
        app.feat.lifecycle.resume_pick = None;
        return false;
    }
    let statuses = capture_statuses(app);
    let st = statuses.as_deref();
    // Preselect top-level agents whose conversation can really be resumed, once that is known.
    if app.feat.lifecycle.resume_pick.is_none() && statuses.is_some() {
        let pre = items
            .iter()
            .filter(|s| s.session.parent_id.is_none() && comeback(s, st) == Comeback::SameConversation)
            .map(|s| s.session.id)
            .collect();
        app.feat.lifecycle.resume_pick = Some(pre);
    }
    let mut pick = app.feat.lifecycle.resume_pick.clone().unwrap_or_default();
    let lines: Vec<(SessionInfo, String)> = items
        .iter()
        .map(|s| {
            let mut w = ws_name(app, s);
            let local = s.session.last_active_at.unwrap_or(s.session.created_at).with_timezone(&chrono::Local);
            w.push_str(&format!(" · {}", local.format("%-m월 %-d일 %H:%M")));
            if let Some(p) = s.session.parent_id.and_then(|p| list.iter().find(|x| x.session.id == p)) {
                w.push_str(&format!(" · {}에서 위임", p.session.title));
            }
            (s.clone(), w)
        })
        .collect();
    let last_alive = items.iter().map(|s| s.session.last_active_at.unwrap_or(s.session.created_at).timestamp()).max();
    let ended = why_ended(boot_time(), last_alive);
    let mut act: Option<bool> = None;
    let ((), close) = dialog(ctx, "resume", 680.0, |ui| {
        dialog_title(ui, "이어갈 세션을 골라 주세요", Some(ended_line(ended)));
        egui::Frame::new().fill(c::PANEL_2).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (s, w) in &lines {
                    let (h, color, can) = how(s, st);
                    egui::Frame::new().inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let mut on = pick.contains(&s.session.id);
                            let r = ui.add_enabled(can, egui::Checkbox::without_text(&mut on));
                            if r.changed() {
                                if on {
                                    pick.insert(s.session.id);
                                } else {
                                    pick.remove(&s.session.id);
                                }
                            }
                            theme::tool_badge(ui, data::tool_mark(s.session.tool), 22.0, c::CONTROL);
                            let fg = if can { c::TEXT_STRONG } else { c::TEXT_3 };
                            let width = (ui.available_width() - 130.0).max(80.0);
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                ui.spacing_mut().item_spacing.y = 0.0;
                                theme::truncated(ui, &s.session.title, medium(13.0), fg);
                                theme::truncated(ui, w, font(12.0), c::TEXT_3);
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(h).font(font(12.0)).color(color));
                            });
                        });
                    });
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let n = pick.len();
            let note_w = (ui.available_width() - 260.0).max(160.0);
            ui.allocate_ui(egui::vec2(note_w, 34.0), |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "재개할 때 대화 ID가 같은지 확인해요. 다르면 새 대화로 바꾸지 않고 알려 드립니다.",
                        )
                        .font(font(12.5))
                        .color(c::TEXT_3),
                    )
                    .wrap(),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = egui::Button::new(
                    RichText::new(format!("선택한 {n}개 재개")).font(medium(13.0)).color(c::ON_EMBER),
                )
                .fill(c::EMBER)
                .min_size(egui::vec2(0.0, 34.0))
                .corner_radius(CornerRadius::same(7));
                if ui.add_enabled(n > 0, b).clicked() {
                    act = Some(true);
                }
                if theme::secondary_button(ui, "기록만 보기").on_hover_text("아무것도 실행하지 않고 닫아요").clicked()
                {
                    act = Some(false);
                }
            });
        });
        ui.label(
            RichText::new(format!("{}개 세션이 재시작 전에 실행 중이었어요", lines.len()))
                .font(font(11.5))
                .color(c::TEXT_3),
        );
    });
    if statuses.is_some() {
        app.feat.lifecycle.resume_pick = Some(pick.clone());
    }
    match act {
        Some(resume) => {
            app.ui.resume_seen.extend(items.iter().map(|s| s.session.id));
            let (cols, rows) = app.shell.center.terminal_grid();
            for (s, _) in &lines {
                let id = s.session.id;
                if resume && pick.contains(&id) {
                    let restart = Restart { id, resume: comeback(s, st) == Comeback::SameConversation, cols, rows };
                    let title = s.session.title.clone();
                    app.hub.call::<SessionsRestart>(HostKey::Local, restart, move |app, res| {
                        if let Err(e) = res {
                            app.toast_error(&format!(
                                "{} 재개하지 못했어요: {}",
                                runwick_core::josa::eul_reul(&title),
                                e.message
                            ));
                        }
                    });
                } else {
                    app.hub.send::<SessionsUpdate>(
                        HostKey::Local,
                        SessionUpdate { id, clear_interrupted: true, ..Default::default() },
                    );
                }
            }
            app.feat.lifecycle.resume_pick = None;
            false
        }
        None if close => {
            // Closing without a choice changes nothing for the sessions, and the dialog does not
            // come back by itself for them: 전체 활동 keeps listing them with 재개 선택.
            app.feat.lifecycle.resume_pick = None;
            app.ui.resume_seen.extend(items.iter().map(|s| s.session.id));
            false
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runwick_core::api::records::CaptureState;
    use runwick_core::api::sessions::Session;

    fn info(tool: Tool, conv: Option<&str>) -> SessionInfo {
        let session = Session {
            id: uuid::Uuid::now_v7(),
            project_id: uuid::Uuid::nil(),
            workspace_id: uuid::Uuid::nil(),
            title: "t".into(),
            title_user_set: false,
            tool,
            provider_conv_id: conv.map(str::to_string),
            parent_id: None,
            role: None,
            cwd: "/".into(),
            created_at: runwick_core::now(),
            last_active_at: None,
            ended_at: None,
            exit_code: None,
            hidden: false,
            archived: false,
            env_version: 0,
            interrupted: true,
            origin_device: None,
        };
        SessionInfo { session, live: None }
    }

    fn status(s: &SessionInfo, messages: u64) -> CaptureStatus {
        CaptureStatus {
            session_id: s.session.id,
            state: if messages > 0 { CaptureState::UpToDate } else { CaptureState::NoFile },
            last_error: None,
            last_collected_at: None,
            last_checked_at: None,
            message_count: messages,
        }
    }

    #[test]
    fn runs_left_alive_are_named_with_their_processes() {
        let s = info(Tool::Claude, Some("c"));
        let left = vec![
            Leftover { session: s.session.id, processes: vec!["node (pid 41)".into()] },
            Leftover { session: uuid::Uuid::now_v7(), processes: vec!["sleep (pid 42)".into()] },
        ];
        let rows = leftover_rows(&left, std::slice::from_ref(&s));
        assert_eq!(rows[0], ("t".to_string(), vec!["node (pid 41)".to_string()]));
        assert_eq!(rows[1].0, "이름을 확인할 수 없는 세션");
    }

    #[test]
    fn the_resume_choice_says_what_really_ended_the_runs() {
        assert_eq!(parse_boottime("{ sec = 1793955586, usec = 805328 } Fri Nov  6 17:59:46 2026"), Some(1793955586));
        assert_eq!(parse_boottime("garbage"), None);
        // Started after the runs were last active: the computer restarted.
        assert_eq!(why_ended(Some(2_000), Some(1_000)), Ended::Reboot);
        // Active after the computer started: only the host went away.
        assert_eq!(why_ended(Some(1_000), Some(2_000)), Ended::HostStopped);
        assert_eq!(why_ended(None, Some(2_000)), Ended::Unknown);
        assert!(ended_line(Ended::HostStopped).starts_with("Runwick 호스트가 예기치 않게 종료돼"));
        assert!(!ended_line(Ended::Unknown).contains("컴퓨터"));
        assert!(boot_time().is_some_and(|b| b > 1_600_000_000), "this computer's start time is readable");
    }

    #[test]
    fn only_saved_conversations_are_offered_for_resume() {
        let talked = info(Tool::Claude, Some("c-1"));
        // Claude gets its id at launch; this one ended at the trust screen with nothing saved.
        let never = info(Tool::Claude, Some("c-2"));
        let no_id = info(Tool::Codex, None);
        let shell = info(Tool::Shell, None);
        let st = vec![status(&talked, 4), status(&never, 0)];
        assert_eq!(comeback(&talked, Some(&st)), Comeback::SameConversation);
        assert_eq!(comeback(&never, Some(&st)), Comeback::Fresh);
        assert_eq!(comeback(&no_id, Some(&st)), Comeback::Fresh);
        assert_eq!(comeback(&shell, Some(&st)), Comeback::Fresh);
        assert_eq!(comeback(&talked, None), Comeback::Checking, "unknown until the records answer");
        assert_eq!(how(&never, Some(&st)).0, "저장된 대화가 없어 새로 시작해요");
        assert_eq!(after_stop(&talked, Some(&st)), "나중에 재개 가능");
        assert_eq!(after_stop(&never, Some(&st)), "새로 시작만 가능");
    }
}
