//! Session states shown with the terminal (S02): starting, ended, record only, start failure,
//! connecting for the first time, connection lost (or a device disconnected on purpose), and
//! input held by another client.
//! The CLI's own screen stays visible; nothing here invents approval buttons.

use crate::icons;
use crate::theme::{self, Tone, c, font, medium, ty};
use egui::{Rect, RichText, Vec2};
use runwick_core::api::sessions::{Phase, SessionInfo, SessionLive, StartErrorKind, Tool};
use runwick_core::josa;

/// Height reserved at the bottom of the terminal for a state bar.
pub const BAR_HEIGHT: f32 = 46.0;

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Top chip over the output: not connected yet since the window opened.
    Connecting {
        reason: String,
    },
    /// Top chip over the output: the connection dropped unexpectedly and is being retried.
    Reconnecting {
        reason: String,
    },
    /// The person disconnected this device (`연결 끊기`); nothing reconnects by itself.
    Disconnected,
    Starting {
        tool: Tool,
        secs: u64,
    },
    /// Bottom bars.
    InputElsewhere {
        label: String,
    },
    Ended {
        code: Option<i32>,
        end: End,
        /// Processes still alive after stopping ("name (pid N)").
        leftovers: Vec<String>,
    },
    RecordOnly {
        interrupted: bool,
        end: End,
    },
    StartFailed {
        tool: Tool,
        kind: Option<StartErrorKind>,
        message: String,
    },
    /// A record synced from another computer: read-only here, never run or resumed.
    Synced {
        device: String,
    },
}

/// What can be done with a session that is not running.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum End {
    /// Agent whose conversation the CLI saved: `--resume` continues it.
    Resume,
    /// Shell: a new process.
    Rerun,
    /// Agent with nothing saved (it never got past its first screen): only a new conversation.
    /// Every Claude session has an id from launch, so an id alone is not enough.
    NoConversation,
    /// The records are not loaded yet.
    Checking,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    Cancel,
    Resume,
    Rerun,
    NewConversation,
    TakeInput,
    ToolSettings,
    Reconnect,
}

impl State {
    pub fn is_bar(&self) -> bool {
        !matches!(self, State::Starting { .. }) && !self.is_connection()
    }

    /// About the connection to the session's computer (a device tab's banner may say it instead).
    pub fn is_connection(&self) -> bool {
        matches!(self, State::Connecting { .. } | State::Reconnecting { .. } | State::Disconnected)
    }
}

pub struct Inputs<'a> {
    pub record: Option<&'a SessionInfo>,
    pub live: Option<&'a SessionLive>,
    pub connected: bool,
    /// The connection to this host worked at least once since the window opened.
    pub ever_connected: bool,
    pub disconnect_reason: Option<String>,
    /// The host is a device the person disconnected from this computer.
    pub disconnected_on_purpose: bool,
    /// This client's connection id on the host.
    pub me: Option<u64>,
    pub interactive: bool,
    pub starting_secs: u64,
    /// The records hold the session's conversation (`None` while they load). Same rule as the
    /// quit dialog and the host's `sessions.restart`.
    pub saved_conversation: Option<bool>,
}

/// The states that apply now, most important first (at most one chip and one bar).
pub fn decide(i: &Inputs<'_>) -> Vec<State> {
    let mut out = Vec::new();
    if !i.connected {
        let reason = i.disconnect_reason.clone().unwrap_or_default();
        out.push(if i.disconnected_on_purpose {
            State::Disconnected
        } else if i.ever_connected {
            State::Reconnecting { reason }
        } else {
            State::Connecting { reason }
        });
        return out;
    }
    let Some(record) = i.record else { return out };
    if let Some(device) = &record.session.origin_device {
        out.push(State::Synced { device: device.clone() });
        return out;
    }
    let tool = record.session.tool;
    let end = match (tool.is_agent(), record.session.provider_conv_id.is_some(), i.saved_conversation) {
        (false, _, _) => End::Rerun,
        (true, true, Some(true)) => End::Resume,
        (true, true, None) => End::Checking,
        (true, _, _) => End::NoConversation,
    };
    let Some(live) = i.live else {
        out.push(State::RecordOnly { interrupted: record.session.interrupted, end });
        return out;
    };
    match live.phase {
        Phase::Starting => out.push(State::Starting { tool, secs: i.starting_secs }),
        Phase::Running => {}
        Phase::Exited => {
            if let Some(message) = &live.error {
                out.push(State::StartFailed { tool, kind: live.error_kind, message: message.clone() });
            } else {
                // The exit code is part of the record once the end was saved.
                out.push(State::Ended { code: record.session.exit_code, end, leftovers: live.leftovers.clone() });
            }
            return out;
        }
        Phase::NotRunning => {
            out.push(State::RecordOnly { interrupted: record.session.interrupted, end });
            return out;
        }
    }
    if i.interactive
        && let Some(owner) = live.input_owner_client
        && Some(owner) != i.me
    {
        out.push(State::InputElsewhere { label: live.input_owner.clone().unwrap_or_else(|| "다른 곳".into()) });
    }
    out
}

/// A chip at the top-left of the terminal area.
pub fn chip(ui: &mut egui::Ui, area: Rect, state: &State) -> Option<Act> {
    let mut act = None;
    let pos = area.min + Vec2::new(10.0, 8.0);
    egui::Area::new(ui.scope_id().with("state-chip"))
        .fixed_pos(pos)
        .order(egui::Order::Middle)
        .constrain_to(area)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(c::PANEL_2)
                .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
                .corner_radius(egui::CornerRadius::same(crate::theme::size::RADIUS))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| match state {
                        State::Connecting { reason } => {
                            let r =
                                theme::chip_with_icon(ui, Tone::Waiting, Some(icons::LOADER_CIRCLE), "연결하는 중…");
                            if !reason.is_empty() {
                                r.on_hover_text(reason);
                            }
                        }
                        State::Reconnecting { reason } => {
                            theme::chip(ui, Tone::Waiting, "연결 끊김");
                            let r = ui.label(
                                RichText::new("다시 연결하는 중… 마지막 화면을 보여 주고 있어요")
                                    .font(font(ty::SMALL))
                                    .color(c::TEXT_2),
                            );
                            if !reason.is_empty() {
                                r.on_hover_text(reason);
                            }
                        }
                        State::Disconnected => {
                            theme::chip(ui, Tone::Waiting, "연결을 끊었어요");
                            ui.label(
                                RichText::new("다시 연결하기 전까지 마지막 화면만 보여요")
                                    .font(font(ty::SMALL))
                                    .color(c::TEXT_2),
                            );
                            let r = theme::link_button(ui, "다시 연결");
                            super::verify::target("chip:다시 연결", r.rect);
                            if r.on_hover_text("이 컴퓨터에서 다시 연결해 지금 화면을 받아 와요").clicked()
                            {
                                act = Some(Act::Reconnect);
                            }
                        }
                        State::Starting { tool, secs } => {
                            theme::status_dot(ui, Tone::Running, true);
                            let mut text = format!("{} 시작 중 · {secs}초", tool.label());
                            if *secs >= 10 {
                                text.push_str(" · 아직 화면 출력이 없어요");
                            }
                            ui.label(RichText::new(text).font(medium(ty::SMALL)).color(c::TEXT));
                            if theme::link_button(ui, "취소").on_hover_text("실행을 멈춰요").clicked() {
                                act = Some(Act::Cancel);
                            }
                        }
                        _ => {}
                    });
                });
        });
    act
}

/// A bar across the bottom of the terminal area.
pub fn bar(ui: &mut egui::Ui, rect: Rect, state: &State) -> Option<Act> {
    ui.painter().rect_filled(rect, 0.0, c::PANEL);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, egui::Stroke::new(1.0, c::LINE));
    let mut act = None;
    let inner = rect.shrink2(Vec2::new(14.0, 0.0));
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let text = |ui: &mut egui::Ui, s: &str| {
                theme::truncated(ui, s, font(ty::SMALL), c::TEXT_2);
            };
            // Right side first so long Korean text truncates instead of pushing buttons out.
            let buttons = |ui: &mut egui::Ui, act: &mut Option<Act>| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match state {
                    State::InputElsewhere { .. } => {
                        if theme::secondary_button(ui, "입력 가져오기").clicked() {
                            *act = Some(Act::TakeInput);
                        }
                    }
                    State::Ended { end, .. } | State::RecordOnly { end, .. } => match end {
                        End::Resume => {
                            if theme::primary_button(ui, "같은 대화 재개").clicked() {
                                *act = Some(Act::Resume);
                            }
                        }
                        End::Rerun => {
                            let label = if matches!(state, State::RecordOnly { .. }) {
                                "새 셸로 시작"
                            } else {
                                "새로 실행"
                            };
                            if theme::primary_button(ui, label).clicked() {
                                *act = Some(Act::Rerun);
                            }
                        }
                        End::NoConversation => {
                            if theme::secondary_button(ui, "새 대화로 시작").clicked() {
                                *act = Some(Act::NewConversation);
                            }
                        }
                        End::Checking => {}
                    },
                    State::StartFailed { kind, .. } => {
                        if matches!(kind, Some(StartErrorKind::NotInstalled))
                            && theme::secondary_button(ui, "도구 설정 보기").clicked()
                        {
                            *act = Some(Act::ToolSettings);
                        }
                        if matches!(kind, Some(StartErrorKind::NotResumable)) {
                            if theme::secondary_button(ui, "새 대화로 시작").clicked() {
                                *act = Some(Act::NewConversation);
                            }
                        } else if theme::primary_button(ui, "다시 시도").clicked() {
                            *act = Some(Act::Rerun);
                        }
                    }
                    _ => {}
                });
            };
            match state {
                State::InputElsewhere { label } => {
                    ui.label(theme::icon(icons::KEYBOARD, 13.0, Tone::Direct.fg()));
                    text(ui, &format!("{label}에서 입력 중이에요. 여기서는 보기만 할 수 있어요."));
                }
                State::Ended { leftovers, .. } if !leftovers.is_empty() => {
                    // Stopping did not end everything: never shown as a clean stop.
                    theme::chip(ui, Tone::Recover, &format!("남은 프로세스 {}개", leftovers.len()));
                    text(ui, &format!("멈추지 않았어요: {}", leftovers.join(", ")));
                }
                State::Ended { code, end, .. } => {
                    let (tone, label) = ended_label(*code);
                    theme::chip(ui, tone, &label);
                    text(ui, end_hint(*end, false));
                }
                State::RecordOnly { interrupted, end } => {
                    theme::chip(ui, Tone::Waiting, "기록만 남아 있어요");
                    let hint = if *interrupted {
                        "다시 시작되며 실행이 끝났어요. 자동으로 다시 실행하지 않았어요."
                    } else {
                        end_hint(*end, true)
                    };
                    text(ui, hint);
                }
                State::Synced { device } => {
                    theme::chip(ui, Tone::Neutral, "다른 컴퓨터의 기록");
                    text(ui, &format!("{device}에서 실행한 세션이에요. 대화는 기록 패널과 검색에서 볼 수 있고, 여기서 재개하거나 실행하지 않아요."));
                }
                State::StartFailed { tool, kind, message } => {
                    let title = match kind {
                        Some(StartErrorKind::NotInstalled) => format!("{} CLI를 찾지 못했어요", tool.label()),
                        Some(StartErrorKind::FolderMissing) => "작업 폴더를 찾을 수 없어요".to_string(),
                        Some(StartErrorKind::NotResumable) => "같은 대화를 재개할 수 없어요".to_string(),
                        _ => format!("{} 시작하지 못했어요", josa::eul_reul(tool.label())),
                    };
                    theme::chip(ui, Tone::Recover, &title);
                    text(ui, message);
                }
                _ => {}
            }
            buttons(ui, &mut act);
        },
    );
    act
}

/// Exit codes above 128 mean the process was ended by a signal (as when it is stopped here).
fn ended_label(code: Option<i32>) -> (Tone, String) {
    match code {
        Some(0) => (Tone::Waiting, "실행이 끝났어요 · 종료 코드 0".into()),
        Some(c) if c > 128 => {
            let name = match c - 128 {
                1 => "SIGHUP",
                2 => "SIGINT",
                9 => "SIGKILL",
                15 => "SIGTERM",
                _ => "",
            };
            let how = if name.is_empty() { format!("신호 {}", c - 128) } else { name.to_string() };
            (Tone::Waiting, format!("실행이 끝났어요 · {how}로 종료 (코드 {c})"))
        }
        Some(c) => (Tone::Check, format!("실행이 끝났어요 · 종료 코드 {c}")),
        None => (Tone::Waiting, "실행이 끝났어요".into()),
    }
}

fn end_hint(end: End, record_only: bool) -> &'static str {
    match (end, record_only) {
        (End::Resume, _) => "출력은 그대로 남아 있어요. 같은 대화를 이어서 열 수 있어요.",
        (End::Rerun, false) => "출력은 그대로 남아 있어요. 셸은 새 프로세스로 다시 실행해요.",
        (End::Rerun, true) => "셸은 새 프로세스로 시작해요.",
        (End::NoConversation, _) => "저장된 대화가 없어 같은 대화로 재개할 수 없어요.",
        (End::Checking, _) => "저장된 대화가 있는지 확인하고 있어요.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(tool: Tool, conv: Option<&str>, interrupted: bool) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "session": {
                "id": uuid::Uuid::nil(), "project_id": uuid::Uuid::nil(), "workspace_id": uuid::Uuid::nil(),
                "title": "t", "title_user_set": false, "tool": tool, "provider_conv_id": conv,
                "parent_id": null, "role": null, "cwd": "/", "created_at": "2026-01-01T00:00:00Z",
                "last_active_at": null, "ended_at": null, "exit_code": 0, "hidden": false, "archived": false,
                "env_version": 0, "interrupted": interrupted
            },
            "live": null
        }))
        .unwrap()
    }

    fn live(phase: Phase, owner: Option<u64>, error: Option<&str>) -> SessionLive {
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::nil(), "phase": phase, "busy": false, "attention": null, "last_output_at": null,
            "cols": 80, "rows": 24, "pid": null, "input_owner": owner.map(|_| "MacBook"),
            "unread": false, "error": error, "error_kind": error.map(|_| "not_installed"),
            "input_owner_client": owner
        }))
        .unwrap()
    }

    fn inputs<'a>(record: &'a SessionInfo, live: Option<&'a SessionLive>) -> Inputs<'a> {
        Inputs {
            record: Some(record),
            live,
            connected: true,
            ever_connected: true,
            disconnect_reason: None,
            disconnected_on_purpose: false,
            me: Some(1),
            interactive: true,
            starting_secs: 3,
            saved_conversation: Some(true),
        }
    }

    fn decide_for(record: &SessionInfo, live: Option<&SessionLive>, me: u64) -> Vec<State> {
        decide(&Inputs { me: Some(me), ..inputs(record, live) })
    }

    #[test]
    fn states_follow_phase_tool_and_owner() {
        let claude = info(Tool::Claude, Some("c1"), false);
        assert_eq!(decide_for(&claude, Some(&live(Phase::Running, Some(1), None)), 1), vec![]);
        assert_eq!(
            decide_for(&claude, Some(&live(Phase::Running, Some(2), None)), 1),
            vec![State::InputElsewhere { label: "MacBook".into() }]
        );
        assert_eq!(
            decide_for(&claude, Some(&live(Phase::Exited, None, None)), 1),
            vec![State::Ended { code: Some(0), end: End::Resume, leftovers: vec![] }]
        );
        // An id alone (every Claude session gets one at launch) is not a saved conversation.
        let unsaved = |saved| {
            decide(&Inputs { saved_conversation: saved, ..inputs(&claude, Some(&live(Phase::Exited, None, None))) })
        };
        assert!(matches!(unsaved(Some(false))[0], State::Ended { end: End::NoConversation, .. }));
        assert!(matches!(unsaved(None)[0], State::Ended { end: End::Checking, .. }));
        let codex = info(Tool::Codex, None, true);
        assert_eq!(
            decide_for(&codex, None, 1),
            vec![State::RecordOnly { interrupted: true, end: End::NoConversation }]
        );
        let shell = info(Tool::Shell, None, false);
        assert_eq!(
            decide_for(&shell, Some(&live(Phase::Starting, None, None)), 1),
            vec![State::Starting { tool: Tool::Shell, secs: 3 }]
        );
        let failed = decide_for(&claude, Some(&live(Phase::Exited, None, Some("없음"))), 1);
        assert!(matches!(&failed[0], State::StartFailed { kind: Some(StartErrorKind::NotInstalled), .. }));
        let lost = |on_purpose, ever_connected| {
            decide(&Inputs {
                record: Some(&claude),
                live: Some(&live(Phase::Running, Some(1), None)),
                connected: false,
                ever_connected,
                disconnect_reason: Some("network".into()),
                disconnected_on_purpose: on_purpose,
                me: None,
                interactive: true,
                starting_secs: 0,
                saved_conversation: None,
            })
        };
        // An unexpected loss is retried; a deliberate disconnect says so and waits for the person.
        assert_eq!(lost(false, true), vec![State::Reconnecting { reason: "network".into() }]);
        assert_eq!(lost(true, true), vec![State::Disconnected]);
        // Before the first connection nothing was lost yet.
        assert_eq!(lost(false, false), vec![State::Connecting { reason: "network".into() }]);
        assert!(!State::Disconnected.is_bar(), "a chip over the last screen, like reconnecting");
        assert!(State::Connecting { reason: String::new() }.is_connection());
        assert!(!State::Starting { tool: Tool::Shell, secs: 1 }.is_connection());
    }

    #[test]
    fn a_synced_record_is_read_only() {
        let mut record = info(Tool::Claude, Some("uuid-1"), false);
        record.session.origin_device = Some("MacBook Air".into());
        assert_eq!(decide_for(&record, None, 1), vec![State::Synced { device: "MacBook Air".into() }]);
    }
}
