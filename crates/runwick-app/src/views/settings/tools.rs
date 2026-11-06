//! Settings › CLI 도구: where each CLI is, its version and login state as observed, what Runwick
//! can do with it (재개 · 기록 수집 · 협업 · 새로고침), a re-check, and official install and
//! login commands as text to copy. Runwick never runs installers.

use super::general::{group, header};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::data;
use crate::theme::{self, Tone, c, font, icon, medium, mono, ty};
use egui::{CornerRadius, RichText};
use runwick_core::api::sessions::Tool;
use runwick_core::api::tools::*;

#[derive(Default)]
pub struct State {
    checking: bool,
}

const HOST: HostKey = HostKey::Local;

/// Official install commands (copy only).
fn install_commands(tool: Tool) -> &'static [(&'static str, &'static str)] {
    match tool {
        Tool::Claude => &[
            ("공식 설치 스크립트", "curl -fsSL https://claude.ai/install.sh | bash"),
            ("npm", "npm install -g @anthropic-ai/claude-code"),
        ],
        Tool::Codex => &[("npm", "npm install -g @openai/codex"), ("Homebrew", "brew install --cask codex")],
        Tool::Shell => &[],
    }
}

fn login_hint(tool: Tool) -> Option<(&'static str, &'static str)> {
    match tool {
        Tool::Claude => Some(("터미널에서 Claude Code를 실행한 뒤 /login 으로 로그인해요", "claude")),
        Tool::Codex => Some(("터미널에서 로그인해요", "codex login")),
        Tool::Shell => None,
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(
        ui,
        "CLI 도구",
        "이 컴퓨터에서 Runwick이 찾은 CLI예요. 로그인 셸의 PATH 기준으로 찾고, 로그인 여부는 설정 파일로만 판단해요.",
    );
    let q = data::tools(app, HOST);
    let checking = app.feat.settings.tools.checking || q.loading;
    ui.horizontal(|ui| {
        let when = q.data.as_ref().and_then(|l| l.iter().map(|t| t.checked_at).max());
        let text = match (checking, when) {
            (true, _) => "확인하는 중…".to_string(),
            (false, Some(t)) => format!("{} 확인", data::ago(t)),
            (false, None) => "아직 확인하지 않았어요".to_string(),
        };
        ui.label(RichText::new(text).font(font(12.0)).color(c::TEXT_3));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let b = ui.add_enabled_ui(!checking, |ui| theme::secondary_button(ui, "다시 확인")).inner;
            if b.clicked() {
                app.feat.settings.tools.checking = true;
                app.hub.call::<ToolsList>(HOST, ToolsQuery { refresh: true }, |app, res| {
                    app.feat.settings.tools.checking = false;
                    if let Err(e) = res {
                        app.toast_error(&format!("CLI를 다시 확인하지 못했어요: {}", e.message));
                    }
                });
            }
        });
    });
    ui.add_space(8.0);
    let Some(list) = q.data else {
        let text = match &q.error {
            Some(e) => format!("CLI 정보를 불러오지 못했어요: {}", e.message),
            None => "CLI를 찾는 중…".into(),
        };
        ui.label(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3));
        return;
    };
    for t in list.iter() {
        tool_card(ui, t);
    }
}

fn status(t: &ToolInfo) -> (Tone, String) {
    if !t.installed {
        return (Tone::Check, data::NOT_INSTALLED.into());
    }
    match (t.tool, t.auth) {
        (Tool::Shell, _) => (Tone::Running, "사용 가능".into()),
        (_, AuthState::LoggedIn) => (Tone::Running, "로그인됨".into()),
        (_, AuthState::NeedsLogin) => (Tone::Attention, "로그인 필요".into()),
        (_, AuthState::Unknown) => (Tone::Waiting, "로그인 확인 불가".into()),
    }
}

fn tool_card(ui: &mut egui::Ui, t: &ToolInfo) {
    let name = match t.tool {
        Tool::Shell => format!("기본 셸 · {}", data::shell_name(t)),
        tool => tool.label().to_string(),
    };
    group(ui, &name, |ui| {
        ui.horizontal(|ui| {
            theme::tool_badge(ui, data::tool_mark(t.tool), 28.0, c::CONTROL);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    let version = t.version.clone().unwrap_or_else(|| "버전 확인 불가".into());
                    let v = if t.installed { version } else { "—".into() };
                    ui.label(RichText::new(v).font(medium(ty::BODY)).color(c::TEXT_STRONG));
                    let (tone, text) = status(t);
                    let glyph = if t.installed { tone.icon() } else { data::NOT_INSTALLED_ICON };
                    theme::chip_with_icon(ui, tone, Some(glyph), &text);
                });
                let path = t.path.clone().unwrap_or_else(|| "경로 없음".into());
                theme::truncated(ui, path, mono(12.0), c::TEXT_3);
            });
        });
        support_row(ui, t);
        if let Some(n) = &t.note {
            ui.add(egui::Label::new(RichText::new(n).font(font(12.0)).color(c::TEXT_3)).wrap());
        }
        if !t.installed && t.tool != Tool::Shell {
            ui.label(RichText::new("설치 방법").font(medium(12.0)).color(c::TEXT_2));
            for (label, cmd) in install_commands(t.tool) {
                command_line(ui, label, cmd);
            }
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Runwick은 설치 명령을 대신 실행하지 않아요. 터미널에서 직접 실행한 뒤 ‘다시 확인’을 누르세요.",
                    )
                    .font(font(12.0))
                    .color(c::TEXT_3),
                )
                .wrap(),
            );
        } else if t.installed
            && t.auth != AuthState::LoggedIn
            && let Some((what, cmd)) = login_hint(t.tool)
        {
            command_line(ui, what, cmd);
        }
    });
}

fn support_row(ui: &mut egui::Ui, t: &ToolInfo) {
    let s = &t.support;
    let items = [("재개", s.resume), ("기록 수집", s.transcript), ("협업", s.delegate), ("새로고침", s.refresh)];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (label, on) in items {
            let (glyph, fg, word) = match (on, label) {
                // What Runwick can do once the CLI is there; nothing works before that, so no
                // check mark (it would read as available).
                (true, _) if !t.installed => (icons::CIRCLE, c::TEXT_OFF, "설치 후 지원"),
                (true, _) => (icons::CHECK, c::MINT, "지원"),
                // A shell keeps no conversation, but a refresh still applies new settings by
                // starting a new shell in the same session.
                (false, "새로고침") if t.tool == Tool::Shell => (icons::ROTATE_CCW, c::TEXT_2, "새 셸로 다시 시작"),
                (false, _) => (icons::MINUS, c::TEXT_OFF, "미지원"),
            };
            egui::Frame::new()
                .fill(c::ROW)
                .corner_radius(CornerRadius::same(5))
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.horizontal(|ui| {
                        ui.label(icon(glyph, 11.0, fg));
                        let label_fg = if t.installed { c::TEXT } else { c::TEXT_3 };
                        ui.label(RichText::new(label).font(font(12.0)).color(label_fg));
                        ui.label(RichText::new(word).font(font(11.5)).color(c::TEXT_3));
                    });
                })
                .response
                .on_hover_text(format!("{label}: {word}"));
        }
    });
}

/// A command shown as text with a copy button.
fn command_line(ui: &mut egui::Ui, label: &str, cmd: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).font(font(12.0)).color(c::TEXT_3));
        egui::Frame::new()
            .fill(c::TERMINAL)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(cmd).font(mono(12.0)).color(c::TEXT)).selectable(true));
            });
        if theme::icon_button(ui, icons::COPY, &format!("복사: {cmd}")).clicked() {
            ui.ctx().copy_text(cmd.to_string());
        }
    });
}
