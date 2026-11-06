//! `이 공간` panel for a device's workspace (RemoteSpace.dc.html): where the folder is, who
//! holds the input, whose logins and environment apply, and the two different ways out —
//! `연결 끊기` (this GUI only; runs continue there) and `원격 작업 종료` (confirm the runs on
//! that device, stop them and check that they stopped).

use crate::App;
use crate::client::HostKey;
use crate::remote::{self, Status};
use crate::shell::data;
use crate::state::{SettingsSection, TabKind, WsKey};
use crate::theme::{self, c, font, medium, semibold, ty};
use egui::{CornerRadius, RichText};
use runwick_core::api::devices::DeviceId;
use runwick_core::api::env::EnvList;
use runwick_core::api::mcp::{McpList, McpQuery};
use runwick_core::api::sessions::{SessionId, SessionInfo, SessionsStop, Stop};
use runwick_core::api::tools::AuthState;
use std::collections::HashMap;

#[derive(Default)]
pub struct State {
    ask_stop: Option<DeviceId>,
    stopping: HashMap<DeviceId, Stopping>,
}

struct Stopping {
    targets: Vec<SessionId>,
    pending: usize,
    failed: Vec<String>,
}

/// After `원격 작업 종료`: what is known about the stop (shown above the device's terminals).
pub fn stop_note(app: &mut App, id: DeviceId) -> Option<(String, String)> {
    let (targets, pending, failed) = {
        let s = app.feat.devices.space.stopping.get(&id)?;
        (s.targets.clone(), s.pending, s.failed.clone())
    };
    let name = remote::device_name(app, id);
    if pending > 0 {
        return Some((format!("{name}의 실행을 멈추는 중이에요"), "멈췄는지 확인하고 있어요.".into()));
    }
    if !failed.is_empty() {
        return Some((format!("{name}의 실행 {}개를 멈추지 못했어요", failed.len()), failed.join(" · ")));
    }
    let host = remote::key(id);
    let still = data::all_sessions(app, host)
        .data
        .map(|l| l.iter().filter(|s| targets.contains(&s.session.id) && remote::is_running(app, host, s)).count());
    match still {
        Some(0) => Some((
            format!("{name}의 실행 {}개를 멈췄어요", targets.len()),
            format!("기록과 결과는 {name}에 남아요. 이 컴퓨터의 공간은 그대로예요."),
        )),
        Some(n) => Some((format!("{name}에서 실행 {n}개가 아직 돌고 있어요"), "멈췄는지 계속 확인하고 있어요.".into())),
        None => Some((format!("{name}의 실행을 멈추라고 요청했어요"), "멈췄는지는 연결되면 확인해요.".into())),
    }
}

pub fn dismiss_stop(app: &mut App, id: DeviceId) {
    app.feat.devices.space.stopping.remove(&id);
}

fn running_sessions(app: &mut App, host: HostKey) -> Option<Vec<SessionInfo>> {
    let list = data::all_sessions(app, host).data?;
    Some(list.iter().filter(|s| remote::is_running(app, host, s)).cloned().collect())
}

enum Act {
    Disconnect,
    Reconnect,
    AskStop,
    CancelStop,
    Stop(Vec<SessionInfo>),
    EnvSettings,
}

pub fn show(app: &mut App, ui: &mut egui::Ui, ws: WsKey) {
    let HostKey::Device(id) = ws.host else {
        crate::shell::widgets::empty_note(
            ui,
            "이 컴퓨터의 공간이에요. 다른 기기의 공간을 고르면 연결 정보를 보여 드려요.",
        );
        return;
    };
    let name = remote::device_name(app, id);
    let status = remote::status(app, id);
    let live = status.is_connected();
    let wl = data::workspaces(app).data;
    let w = wl.as_ref().and_then(|l| data::find_ws(l, &ws).cloned());
    let mut acts = Vec::new();

    let (conn_text, conn_color) = match &status {
        Status::Connected(_) => {
            let d = remote::device(app, id);
            let how = match d.as_ref() {
                Some(d) if d.target.starts_with(runwick_core::api::devices::SIMULATED_PREFIX) => {
                    "시뮬레이션 (개발용)".to_string()
                }
                Some(d) => match &d.via {
                    Some(v) => format!("{v} · SSH"),
                    None => "SSH".to_string(),
                },
                None => "SSH".to_string(),
            };
            (format!("연결됨 · {how}"), c::MINT)
        }
        Status::Off => ("직접 끊음 · 실행은 계속".into(), c::GREY),
        Status::Unreachable { lost: Some(l), .. } | Status::Connecting { lost: Some(l), .. } => {
            (format!("끊김 · 마지막 확인 {}", remote::clock(l.at_ts)), c::GREY)
        }
        other => (remote::summary(other).1, c::GREY),
    };

    egui::ScrollArea::vertical().id_salt("space-panel").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // Header.
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 12 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(RichText::new("이 공간").font(font(11.5)).color(c::TEXT_3));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let title = w.as_ref().map(|w| w.name.clone()).unwrap_or_else(|| "공간".into());
                ui.label(RichText::new(title).font(semibold(ty::PANEL_TITLE)).color(c::TEXT_STRONG));
                device_tag(ui, &name);
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 3.5, conn_color);
                ui.label(RichText::new(&conn_text).font(font(12.5)).color(conn_color));
            });
        });
        line(ui);

        // Facts.
        let path = w.as_ref().map(|w| data::tilde(&w.path)).unwrap_or_else(|| "확인 불가".into());
        fact(ui, "폴더", &format!("{path} · {name}의 폴더예요"), c::TEXT_STRONG, None);
        let (input, input_color) = input_line(app, ws, live, &name);
        fact(ui, "입력", &input, input_color, None);
        fact(
            ui,
            "파일과 문서",
            &format!("이 공간에서 여는 파일은 {name}의 파일이에요. 저장할 때 그쪽 버전을 확인해요."),
            c::TEXT_STRONG,
            None,
        );
        let logins = login_line(app, ws.host, w.as_ref().map(|w| w.path.clone()), live, &name);
        fact(ui, "로그인과 MCP", &logins, c::TEXT_STRONG, None);
        let (env, env_color) = env_line(app, ws.host, live);
        if fact(ui, "공통 환경변수", &env, env_color, Some("기기 설정에서 적용")) {
            acts.push(Act::EnvSettings);
        }
        fact(
            ui,
            "자동 작업",
            &format!("{name} 전원 기준이에요. 이 컴퓨터가 배터리여도 계속돼요."),
            c::TEXT_STRONG,
            None,
        );

        // Ways out.
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 18 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            let wide = egui::vec2(ui.available_width(), 32.0);
            let can_retry =
                matches!(status, Status::Off | Status::Unreachable { .. } | Status::Refused(_) | Status::Setup(_));
            if live {
                if wide_button(ui, wide, "연결 끊기", c::CONTROL, c::TEXT_STRONG).clicked() {
                    acts.push(Act::Disconnect);
                }
                note(ui, &format!("이 컴퓨터에서만 끊어요. 실행은 {name}에서 계속돼요."));
            } else if can_retry && wide_button(ui, wide, "다시 연결", c::CONTROL, c::TEXT_STRONG).clicked() {
                acts.push(Act::Reconnect);
            }
            ui.add_space(10.0);
            let running = if live { running_sessions(app, ws.host) } else { None };
            let asking = app.feat.devices.space.ask_stop == Some(id);
            match (&running, asking) {
                (Some(list), true) if !list.is_empty() => stop_confirm(ui, &name, list, &mut acts),
                _ => {
                    let n = running.as_ref().map(|l| l.len()).unwrap_or(0);
                    let can = live && n > 0;
                    let (fg, ring) = if can {
                        (egui::Color32::from_rgb(0xFF, 0x9C, 0xA4), egui::Color32::from_rgb(0x5A, 0x2E, 0x33))
                    } else {
                        (c::TEXT_OFF, c::LINE_STRONG)
                    };
                    let b = egui::Button::new(RichText::new("원격 작업 종료…").font(font(12.5)).color(fg))
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::new(1.0, ring))
                        .corner_radius(CornerRadius::same(7))
                        .min_size(wide);
                    if super::mark("원격 작업 종료…", ui.add_enabled(can, b)).clicked() {
                        acts.push(Act::AskStop);
                    }
                    let text = if !live {
                        "연결돼야 종료를 요청하고 실제로 멈췄는지 확인할 수 있어요.".to_string()
                    } else if n == 0 {
                        format!("{name}에서 돌고 있는 실행이 없어요.")
                    } else {
                        format!("{name}의 실행 {n}개를 멈춰요. 대상 기기를 다시 보여 드려요.")
                    };
                    note(ui, &text);
                }
            }
        });
    });

    for a in acts {
        match a {
            Act::Disconnect => remote::disconnect(app, id),
            Act::Reconnect => remote::reconnect(app, id),
            Act::AskStop => app.feat.devices.space.ask_stop = Some(id),
            Act::CancelStop => app.feat.devices.space.ask_stop = None,
            Act::Stop(list) => stop_all(app, id, list),
            Act::EnvSettings => {
                app.feat.settings.devices.select(id);
                crate::shell::actions::open_settings(app, Some(SettingsSection::Devices));
            }
        }
    }
}

fn stop_all(app: &mut App, id: DeviceId, list: Vec<SessionInfo>) {
    app.feat.devices.space.ask_stop = None;
    let host = remote::key(id);
    app.feat.devices.space.stopping.insert(
        id,
        Stopping { targets: list.iter().map(|s| s.session.id).collect(), pending: list.len(), failed: vec![] },
    );
    for s in list {
        let title = s.session.title.clone();
        app.hub.call::<SessionsStop>(host, Stop { id: s.session.id, force: false }, move |app, res| {
            if let Some(st) = app.feat.devices.space.stopping.get_mut(&id) {
                st.pending = st.pending.saturating_sub(1);
                if let Err(e) = res {
                    st.failed.push(format!("{title}: {}", e.message));
                }
            }
        });
    }
}

fn stop_confirm(ui: &mut egui::Ui, name: &str, list: &[SessionInfo], acts: &mut Vec<Act>) {
    egui::Frame::new()
        .fill(c::ROSE_BG)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0x4A, 0x24, 0x28)))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{name}의 실행 {}개를 멈출까요?", list.len()))
                        .font(semibold(ty::BODY))
                        .color(c::TEXT_STRONG),
                )
                .wrap(),
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for s in list {
                    let tool = s.session.tool.label();
                    let mut t = if s.session.title == tool {
                        format!("• {tool}")
                    } else {
                        format!("• {} · {tool}", s.session.title)
                    };
                    if s.live.as_ref().and_then(|l| l.attention.as_ref()).is_some() {
                        t.push_str(" · 확인 대기");
                    }
                    theme::truncated(ui, t, font(12.5), c::TEXT);
                }
            });
            ui.add(
                egui::Label::new(
                    RichText::new(format!("이 컴퓨터의 공간과 실행은 영향 없어요. 기록은 {name}에 남아요."))
                        .font(font(ty::SMALL))
                        .color(c::TEXT_2),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                let stop = egui::Button::new(
                    RichText::new("멈추기").font(medium(12.5)).color(egui::Color32::from_rgb(0x2A, 0x0D, 0x10)),
                )
                .fill(c::ROSE)
                .corner_radius(CornerRadius::same(7))
                .min_size(egui::vec2(0.0, 30.0));
                if super::mark("멈추기", ui.add(stop)).clicked() {
                    acts.push(Act::Stop(list.to_vec()));
                }
                let cancel = egui::Button::new(RichText::new("취소").font(font(12.5)).color(c::TEXT_STRONG))
                    .fill(c::CONTROL_HOVER)
                    .corner_radius(CornerRadius::same(7))
                    .min_size(egui::vec2(0.0, 30.0));
                if ui.add(cancel).clicked() {
                    acts.push(Act::CancelStop);
                }
            });
        });
}

/// Who types into this workspace's active session.
fn input_line(app: &mut App, ws: WsKey, live: bool, name: &str) -> (String, egui::Color32) {
    if !live {
        return ("보내지 않음 · 연결되면 다시 확인해요".into(), c::TEXT_3);
    }
    let active = app.ui.tabset(&ws).and_then(|s| {
        let id = s.active?;
        s.tabs.iter().find(|t| t.id == id).map(|t| t.kind.clone())
    });
    let Some(TabKind::Terminal { host, session }) = active else {
        return ("세션마다 한 화면에서만 입력해요".into(), c::TEXT_STRONG);
    };
    let me = app.hub.state(host).info().map(|i| i.client_id);
    match crate::terminal::live(app, host, session) {
        Some(l) => match (l.input_owner_client, l.input_owner) {
            (Some(o), Some(label)) if Some(o) != me => (format!("{label} 화면 · {name}"), c::VIOLET),
            (Some(o), None) if Some(o) != me => ("다른 화면".into(), c::VIOLET),
            _ => ("이 컴퓨터".into(), c::TEXT_STRONG),
        },
        None => ("확인 중…".into(), c::TEXT_3),
    }
}

fn login_line(app: &mut App, host: HostKey, cwd: Option<String>, live: bool, name: &str) -> String {
    if !live {
        return format!("{name} 기준 · 연결되면 확인해요");
    }
    let tools = data::tools(app, host).data;
    let mcp = app.hub.query::<McpList>(host, &McpQuery { cwd }).data;
    let mut parts = Vec::new();
    match tools {
        Some(list) => {
            for t in list.iter().filter(|t| t.tool.is_agent()) {
                let s = match (t.installed, t.auth) {
                    (false, _) => data::NOT_INSTALLED,
                    (true, AuthState::LoggedIn) => "로그인됨",
                    (true, AuthState::NeedsLogin) => "로그인 필요",
                    (true, AuthState::Unknown) => "로그인 확인 불가",
                };
                parts.push(format!("{} {s}", t.tool.label()));
            }
        }
        None => parts.push("도구 확인 중…".into()),
    }
    let mcp = match mcp {
        Some(m) => format!("MCP {}개", m.servers.iter().filter(|s| s.enabled).count()),
        None => "MCP 확인 중…".into(),
    };
    format!("{name} 기준 · {} · {mcp}", parts.join(", "))
}

fn env_line(app: &mut App, host: HostKey, live: bool) -> (String, egui::Color32) {
    if !live {
        return ("연결되면 확인해요".into(), c::TEXT_3);
    }
    let local = app.hub.query::<EnvList>(HostKey::Local, &()).data;
    let there = app.hub.query::<EnvList>(host, &()).data;
    match (local, there) {
        (Some(l), Some(t)) => {
            let d = remote::envsync::diff(&l, &t);
            if d.in_sync() {
                (format!("v{} 적용 · 이 컴퓨터와 같은 값이에요", t.version), c::TEXT_STRONG)
            } else {
                (format!("v{} 적용 · 이 컴퓨터와 다른 값 {}개", t.version, d.to_send.len()), c::YELLOW)
            }
        }
        _ => ("확인 중…".into(), c::TEXT_3),
    }
}

fn device_tag(ui: &mut egui::Ui, name: &str) {
    egui::Frame::new()
        .fill(c::CONTROL)
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.horizontal(|ui| {
                ui.label(theme::icon(crate::icons::MONITOR, 10.5, c::TEXT));
                ui.label(RichText::new(name).font(font(11.5)).color(c::TEXT));
            });
        });
}

fn line(ui: &mut egui::Ui) {
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y, egui::Stroke::new(1.0, c::LINE));
}

/// A fact row; returns true when its link was clicked.
fn fact(ui: &mut egui::Ui, k: &str, v: &str, color: egui::Color32, link: Option<&str>) -> bool {
    let mut clicked = false;
    egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 9)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(k).font(font(11.5)).color(c::TEXT_3));
        ui.add(egui::Label::new(RichText::new(v).font(font(12.5)).color(color)).wrap());
        if let Some(l) = link {
            clicked =
                ui.add(egui::Link::new(RichText::new(l).font(font(ty::SMALL)).color(c::TEXT_2).underline())).clicked();
        }
    });
    line(ui);
    clicked
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_3)).wrap());
}

fn wide_button(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    text: &str,
    fill: egui::Color32,
    fg: egui::Color32,
) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).font(font(12.5)).color(fg))
        .fill(fill)
        .corner_radius(CornerRadius::same(7))
        .min_size(size);
    super::mark(text, ui.add(b))
}
