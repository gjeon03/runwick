//! Settings › MCP: what Claude Code and Codex really use, read from their own files (scope and
//! location per server, values hidden), connection status from the CLI on request, and the
//! running sessions that started with an older configuration. Changes are made in the
//! provider's files ("파일 열기"); Runwick never edits them.

use super::general::{group, header};
use crate::App;
use crate::client::HostKey;
use crate::shell::data;
use crate::theme::{self, Tone, c, font, medium, mono, ty};
use crate::views::refresh::{self, PendingFilter};
use egui::RichText;
use runwick_core::api::mcp::*;
use runwick_core::api::sessions::Tool;
use std::collections::BTreeSet;

const HOST: HostKey = HostKey::Local;

#[derive(Default)]
pub struct State {
    /// Status checks in flight.
    checking: BTreeSet<ToolKey>,
}

type ToolKey = u8;

fn key(t: Tool) -> ToolKey {
    t as u8
}

/// Folder whose local and project scopes are shown: the selected workspace on this computer.
fn folder(app: &mut App) -> Option<(String, String)> {
    let ws = app.ui.selected_ws.filter(|w| w.host == HOST)?;
    let list = data::workspaces(app).data?;
    let w = data::find_ws(&list, &ws)?;
    let project = data::projects(app).data.and_then(|p| data::project_of(&p, w.project_id).map(|p| p.name.clone()));
    let name = match project {
        Some(p) => format!("{p} / {}", w.name),
        None => w.name.clone(),
    };
    Some((w.path.clone(), name))
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(
        ui,
        "MCP",
        "각 CLI가 실제로 쓰는 MCP 설정을 그대로 보여줘요. 바꾸려면 그 CLI의 설정 파일에서 고치세요. 바꾼 뒤에는 실행 중인 세션을 새로고침해야 적용돼요.",
    );
    let folder = folder(app);
    let cwd = folder.as_ref().map(|f| f.0.clone());
    let place = match &folder {
        Some((path, name)) => format!("기준 프로젝트: {name} · {}", data::tilde(path)),
        None => "프로젝트를 고르면 그 프로젝트의 로컬·프로젝트 범위도 보여요".into(),
    };
    ui.label(RichText::new(place).font(font(12.0)).color(c::TEXT_3));
    ui.add_space(8.0);
    let q = app.hub.query::<McpList>(HOST, &McpQuery { cwd: cwd.clone() });
    let Some(config) = q.data else {
        let text = match &q.error {
            Some(e) => format!("MCP 설정을 읽지 못했어요: {}", e.message),
            None => "MCP 설정을 읽는 중…".into(),
        };
        ui.label(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3));
        return;
    };
    for tool in [Tool::Claude, Tool::Codex] {
        provider(app, ui, tool, &config, &cwd);
    }
    ui.add_space(8.0);
    refresh::pending_list(app, ui, PendingFilter::Mcp, None);
}

fn provider(app: &mut App, ui: &mut egui::Ui, tool: Tool, config: &McpConfig, cwd: &Option<String>) {
    let check = app.hub.query::<McpLastCheck>(HOST, &CheckQuery { provider: tool, cwd: cwd.clone() }).data;
    let check: Option<McpCheckResult> = check.and_then(|c| (*c).clone());
    let checking = app.feat.settings.mcp.checking.contains(&key(tool));
    let mut run_check = false;
    let mut open_file = None;
    group(ui, tool.label(), |ui| {
        ui.horizontal(|ui| {
            let (command, how) = match tool {
                Tool::Claude => ("claude mcp list", "로 서버마다 연결을 시험해요"),
                _ => ("codex mcp list", "로 설정과 로그인만 확인해요 (연결 시험은 Codex 안에서)"),
            };
            let when = match (&check, checking) {
                (_, true) => "확인하는 중… 서버가 많으면 몇십 초 걸려요".to_string(),
                (Some(c), false) => format!("{} 상태 확인 · ", data::ago(c.checked_at)),
                (None, false) => "상태는 아직 확인하지 않았어요 · ".to_string(),
            };
            let mut job = egui::text::LayoutJob::default();
            let plain = egui::TextFormat::simple(font(12.0), c::TEXT_3);
            job.append(&when, 0.0, plain.clone());
            if !checking {
                // The command in code style, as it would be typed in a terminal.
                let code = egui::TextFormat { background: c::ROW, ..egui::TextFormat::simple(mono(11.5), c::TEXT_2) };
                job.append(command, 0.0, code);
                job.append(how, 0.0, plain);
            }
            ui.add(egui::Label::new(job).wrap());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                run_check = ui.add_enabled_ui(!checking, |ui| theme::secondary_button(ui, "상태 확인")).inner.clicked();
            });
        });
        if let Some(e) = check.as_ref().and_then(|c| c.error.clone()) {
            ui.label(RichText::new(format!("상태를 확인하지 못했어요: {e}")).font(font(12.0)).color(c::ROSE));
        }
        for s in config.sources.iter().filter(|s| s.provider == tool) {
            if let Some(path) = source_row(ui, s) {
                open_file = Some(path);
            }
        }
        crate::views::settings::general::divider(ui);
        let servers: Vec<&McpServer> = config.servers.iter().filter(|s| s.provider == tool).collect();
        let reported: Vec<&McpStatus> = check
            .as_ref()
            .map(|c| c.statuses.iter().filter(|st| !servers.iter().any(|s| s.name == st.name)).collect())
            .unwrap_or_default();
        if servers.is_empty() && reported.is_empty() {
            ui.label(RichText::new("설정된 MCP 서버가 없어요").font(font(12.5)).color(c::TEXT_3));
        }
        for s in &servers {
            let status = check.as_ref().and_then(|c| c.statuses.iter().find(|st| st.name == s.name));
            server_row(ui, &s.name, Some(s), status, check.is_some());
        }
        for st in reported {
            server_row(ui, &st.name, None, Some(st), true);
        }
    });
    if run_check {
        app.feat.settings.mcp.checking.insert(key(tool));
        let q = CheckQuery { provider: tool, cwd: cwd.clone() };
        app.hub.call::<McpCheck>(HOST, q, move |app, res| {
            app.feat.settings.mcp.checking.remove(&key(tool));
            if let Err(e) = res {
                app.toast_error(&format!("MCP 상태를 확인하지 못했어요: {}", e.message));
            }
        });
    }
    if let Some(path) = open_file {
        open_in_editor(app, &path);
    }
}

/// One configuration file; returns its path when "파일 열기" was clicked.
fn source_row(ui: &mut egui::Ui, s: &McpSource) -> Option<String> {
    let mut open = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new(s.scope.label()).font(medium(12.0)).color(c::TEXT_2));
        theme::truncated(ui, &s.display, mono(12.0), if s.exists { c::TEXT } else { c::TEXT_3 });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if s.exists {
                if theme::link_button(ui, "파일 열기").on_hover_text(&s.path).clicked() {
                    open = Some(s.path.clone());
                }
            } else {
                ui.label(RichText::new("파일 없음").font(font(12.0)).color(c::TEXT_3));
            }
        });
    });
    if let Some(e) = &s.error {
        ui.label(RichText::new(format!("{}: {e}", s.display)).font(font(12.0)).color(c::ROSE));
    }
    open
}

fn health_chip(h: McpHealth) -> (Tone, &'static str) {
    match h {
        McpHealth::Connected => (Tone::Running, "연결됨"),
        McpHealth::Failed => (Tone::Recover, "실패"),
        McpHealth::NeedsAuth => (Tone::Attention, "인증 필요"),
        McpHealth::PendingApproval => (Tone::Attention, "승인 대기"),
        McpHealth::Disabled => (Tone::Waiting, "꺼짐"),
        McpHealth::Unknown => (Tone::Waiting, "확인 불가"),
    }
}

fn server_row(ui: &mut egui::Ui, name: &str, s: Option<&McpServer>, status: Option<&McpStatus>, checked: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 140.0).max(160.0));
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                theme::truncated(ui, name, medium(ty::BODY), c::TEXT_STRONG);
                let scope = s.map(|s| s.scope).unwrap_or(McpScope::Other);
                theme::chip_with_icon(ui, Tone::Neutral, None, scope.label());
                if s.is_some_and(|s| !s.enabled) {
                    theme::chip(ui, Tone::Waiting, "꺼짐");
                }
            });
            match s {
                Some(s) => {
                    let kind = match s.transport {
                        McpTransport::Stdio => "stdio",
                        McpTransport::Http => "HTTP",
                        McpTransport::Sse => "SSE",
                        McpTransport::Unknown => "알 수 없는 방식",
                    };
                    theme::truncated(ui, format!("{kind} · {}", s.target), mono(12.0), c::TEXT_2);
                    if !s.secret_keys.is_empty() {
                        ui.label(
                            RichText::new(format!("숨긴 값 {}개", s.secret_keys.len()))
                                .font(font(11.5))
                                .color(c::TEXT_3),
                        )
                        .on_hover_text(format!("값은 표시하지 않아요: {}", s.secret_keys.join(", ")));
                    }
                }
                None => {
                    ui.label(
                        RichText::new("플러그인이나 계정 연결처럼 CLI가 직접 제공해요")
                            .font(font(12.0))
                            .color(c::TEXT_3),
                    );
                }
            }
            if let Some(d) = status.and_then(|st| st.detail.as_deref()) {
                theme::truncated(ui, d, font(11.5), c::TEXT_3);
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match status {
            Some(st) => {
                let (tone, text) = health_chip(st.health);
                theme::chip(ui, tone, text);
            }
            None if checked => {
                theme::chip(ui, Tone::Waiting, "CLI가 보고하지 않음");
            }
            None => {
                ui.label(RichText::new("확인 전").font(font(12.0)).color(c::TEXT_3));
            }
        });
    });
    ui.add_space(4.0);
}

/// Open a provider's configuration file in the default text editor (this computer only).
fn open_in_editor(app: &mut App, path: &str) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("/usr/bin/open").args(["-t", path]).spawn();
    #[cfg(not(target_os = "macos"))]
    let r = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = r {
        app.toast_error(&format!("파일을 열지 못했어요: {e}"));
    }
}
