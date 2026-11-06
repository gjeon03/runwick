//! Settings › 기기 (Devices.dc.html): this computer and the registered devices with their
//! connection state, Runwick version and last confirmed time; per device the tools and logins,
//! MCP servers and global environment *as that device has them* (logins and MCP are never
//! copied between computers; non-secret variables can be sent on request, secrets are entered
//! on each device); the workspaces shown here; and removing a device.

use super::general::header;
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::remote::{self, Status, envsync};
use crate::shell::data;
use crate::theme::{self, Tone, c, font, medium, semibold, ty};
use crate::views::devices::{self as dv, Mark, card, card_head, check_row, small_button};
use egui::{CornerRadius, RichText};
use runwick_core::api::devices::{
    Device, DeviceId, DeviceUpdate, DevicesRemove, DevicesUpdate, WorkspacesUnlinkRemote,
};
use runwick_core::api::env::{EnvList, EnvSet};
use runwick_core::api::host::HostPing;
use runwick_core::api::mcp::{McpList, McpQuery};
use runwick_core::api::projects::{ById, Workspace};
use runwick_core::api::sessions::Tool;
use runwick_core::api::tools::AuthState;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct State {
    selected: Option<HostKey>,
    unpair: Option<DeviceId>,
    renaming: Option<(DeviceId, String)>,
    pings: HashMap<DeviceId, Ping>,
    applying: HashSet<DeviceId>,
}

enum Ping {
    Waiting,
    Done(Instant, Result<Duration, String>),
}

impl State {
    /// Show this device when the section opens.
    pub fn select(&mut self, id: DeviceId) {
        self.selected = Some(HostKey::Device(id));
    }
}

enum Act {
    Add,
    Select(HostKey),
    Check(DeviceId),
    OpenSpace(Workspace),
    AskUnpair(DeviceId),
    CancelUnpair,
    Unpair(Device),
    Rename(DeviceId, String),
    StartRename(DeviceId, String),
    CancelRename,
    ApplyEnv(DeviceId),
    Unlink(Workspace),
    AddFolder(Device),
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    dv::pump(app);
    if app.feat.devices.add.is_some() {
        dv::add::show(app, ui);
        return;
    }
    let mut acts = Vec::new();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width() - 110.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            header(
                ui,
                "기기",
                "이 컴퓨터는 등록 없이 바로 써요. 다른 기기를 추가하면 그 기기의 작업 공간이 같은 사이드바에 함께 보이고, 실행은 각 기기에서 계속돼요.",
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if dv::mark("기기 추가", theme::primary_button(ui, "기기 추가")).clicked() {
                acts.push(Act::Add);
            }
        });
    });
    let q = app.hub.query::<runwick_core::api::devices::DevicesList>(HostKey::Local, &());
    let devices: Vec<Device> = match (&q.data, &q.error) {
        (Some(l), _) => l.as_ref().clone(),
        (None, Some(e)) => {
            ui.label(
                RichText::new(format!("기기 목록을 불러오지 못했어요: {}", e.message))
                    .font(font(ty::BODY))
                    .color(c::ROSE),
            );
            return;
        }
        (None, None) => {
            ui.label(RichText::new("기기 목록을 불러오는 중…").font(font(ty::BODY)).color(c::TEXT_3));
            return;
        }
    };
    let workspaces = data::workspaces(app).data.map(|l| l.as_ref().clone()).unwrap_or_default();
    let mut selected = app.feat.settings.devices.selected.unwrap_or(HostKey::Local);
    if let HostKey::Device(id) = selected
        && !devices.iter().any(|d| d.id == id)
    {
        selected = HostKey::Local;
    }

    // Device tabs.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        let local_name = app.hub.local_info().map(|i| i.device_name).unwrap_or_else(|| "이 컴퓨터".into());
        let local_spaces = workspaces.iter().filter(|w| w.remote.is_none()).count();
        let running = running_count(app, HostKey::Local);
        let meta = format!(
            "공간 {local_spaces} · 실행 {}",
            running.map(|n| n.to_string()).unwrap_or_else(|| "확인 중".into())
        );
        if device_tab(
            ui,
            selected == HostKey::Local,
            icons::LAPTOP,
            &local_name,
            Tone::Neutral,
            "이 컴퓨터 · 로컬",
            &meta,
        ) {
            acts.push(Act::Select(HostKey::Local));
        }
        for d in &devices {
            let st = remote::status(app, d.id);
            let (tone, words) = remote::summary(&st);
            let spaces = workspaces.iter().filter(|w| w.remote.as_ref().is_some_and(|r| r.device_id == d.id)).count();
            let mut meta = format!("공간 {spaces}");
            if st.is_connected() {
                if let Some(n) = running_count(app, remote::key(d.id)) {
                    meta.push_str(&format!(" · 실행 {n}"));
                }
            } else if let Some(t) = d.last_seen_at {
                meta.push_str(&format!(" · 마지막 확인 {}", data::ago(t)));
            }
            if let Some(v) = &d.remote_version {
                meta.push_str(&format!(" · Runwick {v}"));
            }
            let glyph = if d.os.as_deref() == Some("linux") { icons::SERVER } else { icons::MONITOR };
            if device_tab(ui, selected == remote::key(d.id), glyph, &d.name, tone, &words, &meta) {
                acts.push(Act::Select(remote::key(d.id)));
            }
        }
    });
    ui.add_space(12.0);
    if devices.is_empty() {
        ui.add(
            egui::Label::new(
                RichText::new("아직 등록한 기기가 없어요. ‘기기 추가’로 다른 컴퓨터를 등록하면 그 컴퓨터의 공간이 사이드바에 함께 보여요.")
                    .font(font(12.5))
                    .color(c::TEXT_3),
            )
            .wrap(),
        );
        ui.add_space(10.0);
    }

    match selected {
        HostKey::Local => local_detail(app, ui),
        HostKey::Device(id) => {
            if let Some(d) = devices.iter().find(|d| d.id == id).cloned() {
                device_detail(app, ui, &d, &workspaces, &mut acts);
            }
        }
    }

    for a in acts {
        apply(app, a);
    }
}

fn running_count(app: &mut App, host: HostKey) -> Option<usize> {
    let list = data::all_sessions(app, host).data?;
    Some(list.iter().filter(|s| remote::is_running(app, host, s)).count())
}

fn device_tab(ui: &mut egui::Ui, on: bool, glyph: &str, name: &str, tone: Tone, status: &str, meta: &str) -> bool {
    let w = 230.0_f32.min(ui.available_width());
    let r = egui::Frame::new()
        .fill(if on { c::ROW } else { c::PANEL })
        .stroke(if on { egui::Stroke::new(1.5, c::TEXT) } else { egui::Stroke::NONE })
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(w - 24.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(theme::icon(glyph, 13.0, c::TEXT_2));
                    theme::truncated(ui, name, medium(ty::BODY), c::TEXT_STRONG);
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(theme::icon(tone.icon(), 11.0, tone.fg()));
                    theme::truncated(ui, status, font(ty::SMALL), tone.fg());
                });
                theme::truncated(ui, meta, font(ty::SMALL), c::TEXT_3);
            });
        })
        .response;
    let resp = ui.interact(r.rect, ui.scope_id().with(("device-tab", name)), egui::Sense::click());
    dv::mark(&format!("device:{name}"), resp.clone());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, name));
    theme::focus_ring(ui, &resp, CornerRadius::same(10));
    resp.clicked()
}

fn local_detail(app: &mut App, ui: &mut egui::Ui) {
    card(ui, |ui| {
        let name = app.hub.local_info().map(|i| i.device_name).unwrap_or_else(|| "이 컴퓨터".into());
        card_head(ui, &name, "지금 쓰는 컴퓨터예요. 등록 없이 바로 써요.");
        tools_section(app, ui, HostKey::Local, "이 컴퓨터의 로그인과 설정이에요.", "이 컴퓨터");
        mcp_section(app, ui, HostKey::Local, "이 컴퓨터");
        let env = app.hub.query::<EnvList>(HostKey::Local, &()).data;
        card_head(ui, "공통 환경변수", "값 목록은 기기마다 따로예요. 이 컴퓨터의 값은 ‘공통 환경변수’에서 바꿔요.");
        match env {
            Some(e) => check_row(
                ui,
                Mark::Ok,
                &format!("v{} · 변수 {}개", e.version, e.vars.len()),
                &format!("비밀값 {}개는 이 컴퓨터의 보안 저장소에 있어요.", e.vars.iter().filter(|v| v.secret).count()),
                |_| {},
            ),
            None => check_row(ui, Mark::Busy, "확인하는 중…", "", |_| {}),
        }
        ui.add_space(6.0);
    });
}

fn device_detail(app: &mut App, ui: &mut egui::Ui, d: &Device, workspaces: &[Workspace], acts: &mut Vec<Act>) {
    let host = remote::key(d.id);
    let st = remote::status(app, d.id);
    let live = st.is_connected();
    let linked: Vec<Workspace> =
        workspaces.iter().filter(|w| w.remote.as_ref().is_some_and(|r| r.device_id == d.id)).cloned().collect();
    let ping = app.feat.settings.devices.pings.get(&d.id).map(|p| match p {
        Ping::Waiting => "확인하는 중…".to_string(),
        Ping::Done(at, Ok(rtt)) if at.elapsed() < Duration::from_secs(60) => {
            format!("방금 확인함 · {}ms", rtt.as_millis())
        }
        Ping::Done(at, Err(_)) if at.elapsed() < Duration::from_secs(60) => "다시 확인함 · 연결 안 됨".to_string(),
        Ping::Done(..) => "연결 확인".to_string(),
    });
    let unpair = app.feat.settings.devices.unpair == Some(d.id);
    let renaming = app.feat.settings.devices.renaming.clone().filter(|(id, _)| *id == d.id);
    card(ui, |ui| {
        // Title and address.
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 12 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if small_button(ui, "등록 해제…").clicked() {
                        acts.push(Act::AskUnpair(d.id));
                    }
                    if live
                        && let Some(w) = linked.first()
                        && small_button(ui, "공간 열기").clicked()
                    {
                        acts.push(Act::OpenSpace(w.clone()));
                    }
                    if small_button(ui, ping.as_deref().unwrap_or("연결 확인")).clicked() {
                        acts.push(Act::Check(d.id));
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        match renaming {
                            Some((_, mut text)) => {
                                ui.horizontal(|ui| {
                                    let r = ui.add(
                                        egui::TextEdit::singleline(&mut text).font(semibold(15.0)).desired_width(200.0),
                                    );
                                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    if theme::link_button(ui, "저장").clicked() || enter {
                                        acts.push(Act::Rename(d.id, text.clone()));
                                    } else if theme::link_button(ui, "취소").clicked() {
                                        acts.push(Act::CancelRename);
                                    } else {
                                        acts.push(Act::StartRename(d.id, text));
                                    }
                                });
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    theme::truncated(ui, &d.name, semibold(15.0), c::TEXT_STRONG);
                                    if theme::icon_button(ui, icons::PENCIL, "이름 바꾸기").clicked() {
                                        acts.push(Act::StartRename(d.id, d.name.clone()));
                                    }
                                });
                            }
                        }
                        let mut addr = d.target.clone();
                        if let Some(p) = d.port {
                            addr.push_str(&format!(":{p}"));
                        }
                        if let Some(v) = &d.via {
                            addr.push_str(&format!(" · {v}"));
                        }
                        if !d.target.starts_with(runwick_core::api::devices::SIMULATED_PREFIX) {
                            addr.push_str(" · SSH 키");
                        }
                        if let Some(f) = &d.fingerprint {
                            addr.push_str(&format!(" · 지문 {f}"));
                        }
                        theme::truncated(ui, addr, font(12.5), c::TEXT_3);
                    });
                });
            });
        });
        if unpair {
            egui::Frame::new()
                .fill(c::ROSE_BG)
                .inner_margin(egui::Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if dv::mark("등록 해제", theme::danger_button(ui, "등록 해제")).clicked() {
                                acts.push(Act::Unpair(d.clone()));
                            }
                            if dv::mark("취소", theme::secondary_button(ui, "취소")).clicked() {
                                acts.push(Act::CancelUnpair);
                            }
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.label(RichText::new(format!("{} 등록을 해제할까요?", d.name)).font(semibold(ty::BODY)).color(c::TEXT_STRONG));
                                let more = if linked.is_empty() {
                                    String::new()
                                } else {
                                    format!(" 사이드바에서 공간 {}개가 빠져요.", linked.len())
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "이 컴퓨터는 다시 추가하기 전까지 접속할 수 없어요. {}의 실행과 기록은 그쪽에 그대로 남아요.{more}",
                                            d.name
                                        ))
                                        .font(font(12.5))
                                        .color(c::TEXT_2),
                                    )
                                    .wrap(),
                                );
                            });
                        });
                    });
                });
        }
        // Problem.
        if let Some((cause, text)) = remote::explain(&st, &d.name) {
            let title = match &st {
                Status::Refused(remote::Refusal::Version { .. }) => "Runwick 버전이 달라 연결하지 않았어요".to_string(),
                Status::Refused(_) | Status::Setup(_) => format!("{}에 연결하지 않았어요", d.name),
                _ => format!("{}에 연결되어 있지 않아요 · {cause}", d.name),
            };
            let seen = d
                .last_seen_at
                .map(|t| format!("마지막 확인 {} ({})", remote::clock(t), data::ago(t)))
                .unwrap_or_else(|| "아직 연결한 적이 없어요".into());
            egui::Frame::new().fill(c::YELLOW_BG).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).font(semibold(ty::BODY)).color(c::TEXT_STRONG));
                ui.add(egui::Label::new(RichText::new(text).font(font(12.5)).color(c::TEXT_2)).wrap());
                ui.label(RichText::new(seen).font(font(12.5)).color(c::TEXT_3));
            });
        } else if matches!(st, Status::Off) {
            egui::Frame::new().fill(c::GREY_BG).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("이 컴퓨터에서 연결을 끊었어요").font(semibold(ty::BODY)).color(c::TEXT_STRONG));
                ui.label(
                    RichText::new(format!("{}의 실행은 계속돼요. ‘연결 확인’으로 다시 연결해요.", d.name))
                        .font(font(12.5))
                        .color(c::TEXT_2),
                );
            });
        }
        if live {
            tools_section(
                app,
                ui,
                host,
                &format!("{}에서 실행할 때 쓰는 로그인과 설정이에요. 이 컴퓨터에서 옮기지 않아요.", d.name),
                &d.name,
            );
            mcp_section(app, ui, host, &d.name);
            env_section(app, ui, d, acts);
        }
        spaces_section(ui, d, &linked, live, acts);
        ui.add_space(6.0);
    });
}

fn tools_section(app: &mut App, ui: &mut egui::Ui, host: HostKey, note: &str, name: &str) {
    card_head(ui, "도구와 로그인", note);
    match data::tools(app, host).data {
        None => check_row(ui, Mark::Busy, "확인하는 중…", "", |_| {}),
        Some(list) => {
            for t in list.iter() {
                let v = t.version.clone().map(|v| format!(" {v}")).unwrap_or_default();
                let (mark, title) = match (t.tool, t.installed, t.auth) {
                    (_, false, _) => (Mark::Missing, format!("{} · {}", t.tool.label(), data::NOT_INSTALLED)),
                    (Tool::Shell, true, _) => (Mark::Ok, data::shell_name(t)),
                    (_, true, AuthState::LoggedIn) => (Mark::Ok, format!("{}{v} · 로그인됨", t.tool.label())),
                    (_, true, AuthState::NeedsLogin) => (Mark::Wait, format!("{}{v} · 로그인 필요", t.tool.label())),
                    (_, true, AuthState::Unknown) => (Mark::Wait, format!("{}{v} · 로그인 확인 불가", t.tool.label())),
                };
                let msg = match (t.tool, t.installed, t.auth) {
                    (_, false, _) => {
                        format!("{name}에서 찾지 못했어요. 설치 방법은 ‘CLI 도구’에 있어요.")
                    }
                    (Tool::Shell, true, _) => format!("{name}의 기본 셸"),
                    (_, true, AuthState::NeedsLogin) => format!("{name}에서 로그인해야 해요."),
                    (_, true, AuthState::Unknown) => format!("{name}에서 처음 실행할 때 확인돼요."),
                    (_, true, AuthState::LoggedIn) => format!("{name}의 로그인을 써요."),
                };
                check_row(ui, mark, &title, &msg, |_| {});
            }
        }
    }
}

fn mcp_section(app: &mut App, ui: &mut egui::Ui, host: HostKey, name: &str) {
    card_head(ui, "MCP", &format!("{name}에 설정된 서버예요. 기기마다 따로 설정해요."));
    let q = app.hub.query::<McpList>(host, &McpQuery { cwd: None });
    match (q.data, q.error) {
        (Some(cfg), _) if cfg.servers.is_empty() => check_row(ui, Mark::Wait, "설정된 MCP 서버가 없어요", "", |_| {}),
        (Some(cfg), _) => {
            for s in &cfg.servers {
                let mark = if s.enabled { Mark::Ok } else { Mark::Wait };
                let title = format!("{} · {}", s.name, s.provider.label());
                let msg = format!("{} 범위{}", s.scope.label(), if s.enabled { "" } else { " · 꺼짐" });
                check_row(ui, mark, &title, &msg, |_| {});
            }
        }
        (None, Some(e)) => check_row(ui, Mark::Bad, "MCP 설정을 읽지 못했어요", &e.message, |_| {}),
        (None, None) => check_row(ui, Mark::Busy, "확인하는 중…", "", |_| {}),
    }
}

fn env_section(app: &mut App, ui: &mut egui::Ui, d: &Device, acts: &mut Vec<Act>) {
    let host = remote::key(d.id);
    card_head(ui, "공통 환경변수", "값 목록은 기기마다 따로예요. 이 컴퓨터의 일반 값만 요청할 때 보내요.");
    let local = app.hub.query::<EnvList>(HostKey::Local, &()).data;
    let there = app.hub.query::<EnvList>(host, &()).data;
    let applying = app.feat.settings.devices.applying.contains(&d.id);
    let (Some(l), Some(t)) = (local, there) else {
        check_row(ui, Mark::Busy, "확인하는 중…", "", |_| {});
        return;
    };
    let diff = envsync::diff(&l, &t);
    if diff.in_sync() {
        check_row(ui, Mark::Ok, &format!("v{} 적용 · 이 컴퓨터와 같은 값이에요", t.version), "", |_| {});
    } else {
        let keys: Vec<&str> = diff.to_send.iter().map(|v| v.key.as_str()).collect();
        check_row(
            ui,
            Mark::Ask,
            &format!("v{} 적용 · 이 컴퓨터와 다른 값 {}개", t.version, keys.len()),
            &format!("{} · 보내기 전에는 {}의 새 세션도 지금 값을 써요.", keys.join(", "), d.name),
            |ui| {
                let label = if applying { "보내는 중…".to_string() } else { format!("{}에 적용", d.name) };
                if ui.add_enabled_ui(!applying, |ui| small_button(ui, &label)).inner.clicked() {
                    acts.push(Act::ApplyEnv(d.id));
                }
            },
        );
    }
    for k in &diff.local_secrets {
        let there_too = t.vars.iter().any(|v| v.key == *k && v.secret);
        let msg = if there_too {
            format!("{}의 보안 저장소에도 있어요. 값은 기기마다 따로예요.", d.name)
        } else {
            format!("{}에는 보내지 않아요. 비밀값은 그 기기에서 직접 입력해요.", d.name)
        };
        check_row(ui, if there_too { Mark::Ok } else { Mark::Wait }, &format!("{k} · 비밀값"), &msg, |_| {});
    }
    for k in &diff.secret_there {
        check_row(ui, Mark::Wait, &format!("{k} · {}에서는 비밀값", d.name), "그쪽 값을 덮어쓰지 않아요.", |_| {});
    }
    if diff.only_there > 0 {
        check_row(
            ui,
            Mark::Wait,
            &format!("{}에만 있는 변수 {}개", d.name, diff.only_there),
            "보내도 지우지 않아요.",
            |_| {},
        );
    }
    let refresh = data::all_sessions(app, host)
        .data
        .map(|list| {
            list.iter().filter(|s| remote::is_running(app, host, s) && s.session.env_version < t.version).count()
        })
        .unwrap_or(0);
    if refresh > 0 {
        check_row(
            ui,
            Mark::Wait,
            &format!("실행 중인 세션 {refresh}개는 새로고침해야 새 값을 써요"),
            "세션 바의 새로고침으로 같은 대화를 이어서 적용해요.",
            |_| {},
        );
    }
}

fn spaces_section(ui: &mut egui::Ui, d: &Device, linked: &[Workspace], live: bool, acts: &mut Vec<Act>) {
    card_head(
        ui,
        "이 컴퓨터에 보이는 공간",
        &format!("{}의 폴더를 사이드바에 보여 줘요. 빼도 그 기기의 기록은 남아요.", d.name),
    );
    if linked.is_empty() {
        check_row(ui, Mark::Wait, "아직 보이는 공간이 없어요", "", |_| {});
    }
    for w in linked {
        check_row(ui, Mark::Ok, &w.name, &data::tilde(&w.path), |ui| {
            if small_button(ui, "사이드바에서 빼기").clicked() {
                acts.push(Act::Unlink(w.clone()));
            }
        });
    }
    egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
        let r = ui.add_enabled_ui(live, |ui| small_button(ui, "폴더 추가")).inner;
        if r.on_disabled_hover_text("연결돼야 폴더를 고를 수 있어요").clicked() {
            acts.push(Act::AddFolder(d.clone()));
        }
    });
}

fn apply(app: &mut App, a: Act) {
    match a {
        Act::Add => dv::start_add(app),
        Act::Select(h) => {
            let st = &mut app.feat.settings.devices;
            st.selected = Some(h);
            st.unpair = None;
            st.renaming = None;
        }
        Act::Check(id) => {
            if remote::status(app, id).is_connected() {
                app.feat.settings.devices.pings.insert(id, Ping::Waiting);
                let started = Instant::now();
                app.hub.call::<HostPing>(remote::key(id), (), move |app, res| {
                    let r = res.map(|_| started.elapsed()).map_err(|e| e.message);
                    app.feat.settings.devices.pings.insert(id, Ping::Done(Instant::now(), r));
                });
            } else {
                app.feat.settings.devices.pings.remove(&id);
                remote::reconnect(app, id);
            }
        }
        Act::OpenSpace(w) => {
            if let Some(r) = &w.remote {
                let key = crate::state::WsKey { host: remote::key(r.device_id), id: r.remote_workspace_id };
                crate::shell::actions::select_workspace(app, w.project_id, key);
            }
        }
        Act::AskUnpair(id) => app.feat.settings.devices.unpair = Some(id),
        Act::CancelUnpair => app.feat.settings.devices.unpair = None,
        Act::Unpair(d) => {
            app.feat.settings.devices.unpair = None;
            app.feat.settings.devices.selected = Some(HostKey::Local);
            let name = d.name.clone();
            app.hub.call::<DevicesRemove>(HostKey::Local, ById { id: d.id }, move |app, res| match res {
                Ok(r) => {
                    remote::forget(app, d.id);
                    let more = if r.unlinked > 0 {
                        format!(" · 사이드바에서 공간 {}개를 뺐어요", r.unlinked)
                    } else {
                        String::new()
                    };
                    app.toast_info(&format!("{name} 등록을 해제했어요{more}"));
                }
                Err(e) => app.toast_error(&format!("등록을 해제하지 못했어요: {}", e.message)),
            });
        }
        Act::StartRename(id, text) => app.feat.settings.devices.renaming = Some((id, text)),
        Act::CancelRename => app.feat.settings.devices.renaming = None,
        Act::Rename(id, name) => {
            app.feat.settings.devices.renaming = None;
            let p = DeviceUpdate { id, name: Some(name), ..Default::default() };
            app.hub.call::<DevicesUpdate>(HostKey::Local, p, |app, res| {
                if let Err(e) = res {
                    app.toast_error(&format!("이름을 바꾸지 못했어요: {}", e.message));
                }
            });
        }
        Act::ApplyEnv(id) => apply_env(app, id),
        Act::Unlink(w) => {
            let name = w.name.clone();
            app.hub.call::<WorkspacesUnlinkRemote>(HostKey::Local, ById { id: w.id }, move |app, res| match res {
                Ok(()) => app.toast_info(&format!("‘{name}’을 사이드바에서 뺐어요. 그 기기의 기록은 그대로예요")),
                Err(e) => app.toast_error(&format!("빼지 못했어요: {}", e.message)),
            });
        }
        Act::AddFolder(d) => dv::start_add_folder(app, d),
    }
}

/// Send this computer's non-secret values that differ to the device (never secrets, never
/// removing anything there).
fn apply_env(app: &mut App, id: DeviceId) {
    let host = remote::key(id);
    let local = app.hub.query::<EnvList>(HostKey::Local, &()).data;
    let there = app.hub.query::<EnvList>(host, &()).data;
    let (Some(l), Some(t)) = (local, there) else {
        return;
    };
    let reqs = envsync::requests(&envsync::diff(&l, &t));
    if reqs.is_empty() {
        return;
    }
    app.feat.settings.devices.applying.insert(id);
    let total = reqs.len();
    let left = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(total));
    let failed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    for r in reqs {
        let key = r.key.clone();
        let (left, failed) = (left.clone(), failed.clone());
        app.hub.call::<EnvSet>(host, r, move |app, res| {
            if let Err(e) = res
                && let Ok(mut f) = failed.lock()
            {
                f.push(format!("{key}: {}", e.message));
            }
            if left.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                app.feat.settings.devices.applying.remove(&id);
                let failed = failed.lock().map(|f| f.clone()).unwrap_or_default();
                if failed.is_empty() {
                    app.toast_info(&format!(
                        "일반 값 {total}개를 보냈어요. 실행 중인 세션은 새로고침해야 새 값을 써요"
                    ));
                } else {
                    app.toast_error(&format!("일부를 보내지 못했어요 · {}", failed.join(" · ")));
                }
            }
        });
    }
}
