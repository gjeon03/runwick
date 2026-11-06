//! 기기 추가 (DeviceAdd.dc.html): 기기 고르기 → 연결 확인 → 쓸 준비 → 폴더 고르기 → 완료.
//! A failed check stays on its step with `다시 확인`; nothing is run on this computer instead.
//! The same repository on two computers is never merged by itself: the person picks where the
//! folder shows up.

use super::{Mark, background, card, card_head, check_row, mark, small_button};
use crate::App;
use crate::client::{ConnState, HostKey};
use crate::remote::probe::{self, HostKey as SshKey, Installed, Platform, Resolved};
use crate::remote::ssh::{self, Failure, Route};
use crate::remote::tailscale::{self, Found, Peer};
use crate::remote::{self};
use crate::shell::data;
use crate::state::{Page, WsKey};
use crate::theme::{self, c, font, medium, mono, semibold, ty};
use egui::{CornerRadius, RichText};
use runwick_core::api::devices::*;
use runwick_core::api::projects::{OpenFolder, Opened, ProjectId, ProjectsOpen, Workspace};
use runwick_core::api::sessions::Tool;
use runwick_core::api::tools::{AuthState, ToolsList, ToolsQuery};
use runwick_core::josa;
use std::time::Duration;
use uuid::Uuid;

const STEPS: [&str; 5] = ["기기 고르기", "연결 확인", "쓸 준비", "폴더 고르기", "완료"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Pick = 1,
    Connect = 2,
    Prepare = 3,
    Folder = 4,
    Done = 5,
}

#[derive(Debug, Clone, PartialEq)]
enum Check<T> {
    Idle,
    Running,
    Ok(T),
    Failed(Failure),
}

#[derive(Debug, Clone, PartialEq)]
enum KeyCheck {
    Idle,
    Running,
    /// Already in the person's known_hosts.
    Known(SshKey),
    /// First contact: the person compares the fingerprint.
    Ask(SshKey),
    Accepted(SshKey),
    /// Not applicable (simulated device or a proxy in the SSH config).
    Skipped(String),
    Failed(Failure),
}

#[derive(Debug, Clone, PartialEq)]
enum Ready {
    Idle,
    Running,
    Found(Installed),
    Missing,
    Installing,
    Failed(Failure),
}

/// Where the chosen folder shows up in this computer's sidebar.
#[derive(Debug, Clone, PartialEq)]
enum Join {
    Existing(ProjectId),
    New,
}

pub struct Flow {
    /// Device id chosen up front (the device is connected under it during setup).
    id: DeviceId,
    /// Adding another folder of a registered device (steps 4–5 only).
    existing: Option<Device>,
    step: Step,
    // 1
    searched: bool,
    found: Option<Found>,
    peer: Option<String>,
    name: String,
    name_edited: bool,
    address: String,
    sim_home: String,
    use_sim: bool,
    pick_error: Option<String>,
    // chosen
    target: String,
    port: Option<u16>,
    via: Option<String>,
    // 2
    resolved: Option<Resolved>,
    reach: Check<Option<Duration>>,
    key: KeyCheck,
    login: Check<Platform>,
    // 3
    ready: Ready,
    connected_at: Option<std::time::Instant>,
    show_key_help: bool,
    // 4
    dir: Option<String>,
    chosen: Option<DirInfo>,
    join: Option<Join>,
    project_name: String,
    // 5
    done: Option<(Workspace, String, bool)>,
    busy: bool,
    error: Option<String>,
}

impl Flow {
    pub fn new() -> Self {
        Flow {
            id: Uuid::now_v7(),
            existing: None,
            step: Step::Pick,
            searched: false,
            found: None,
            peer: None,
            name: String::new(),
            name_edited: false,
            address: String::new(),
            sim_home: String::new(),
            use_sim: false,
            pick_error: None,
            target: String::new(),
            port: None,
            via: None,
            resolved: None,
            reach: Check::Idle,
            key: KeyCheck::Idle,
            login: Check::Idle,
            ready: Ready::Idle,
            connected_at: None,
            show_key_help: false,
            dir: None,
            chosen: None,
            join: None,
            project_name: String::new(),
            done: None,
            busy: false,
            error: None,
        }
    }

    pub fn for_device(d: Device) -> Self {
        let mut f = Flow::new();
        f.id = d.id;
        f.name = d.name.clone();
        f.target = d.target.clone();
        f.port = d.port;
        f.step = Step::Folder;
        f.existing = Some(d);
        f
    }

    fn sim(&self) -> bool {
        self.target.starts_with(SIMULATED_PREFIX)
    }
}

impl Default for Flow {
    fn default() -> Self {
        Self::new()
    }
}

fn flow(app: &mut App) -> Option<&mut Flow> {
    app.feat.devices.add.as_mut()
}

/// Run `work` off the UI thread and apply it to the flow that started it.
fn bg<T: Send + 'static>(
    app: &App,
    ctx: &egui::Context,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut App, T) + Send + 'static,
) {
    let id = app.feat.devices.add.as_ref().map(|f| f.id);
    background(app, ctx, work, move |app, out| {
        if app.feat.devices.add.as_ref().map(|f| f.id) == id {
            done(app, out);
        }
    });
}

/// Leave the flow; a connection made only for it is closed.
pub fn cancel(app: &mut App) {
    if let Some(f) = app.feat.devices.add.take()
        && f.existing.is_none()
        && f.done.is_none()
    {
        remote::forget(app, f.id);
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    super::pump(app);
    let Some(step) = app.feat.devices.add.as_ref().map(|f| f.step) else {
        return;
    };
    ui.spacing_mut().item_spacing.y = 4.0;
    if ui.add(egui::Link::new(RichText::new("‹ 기기").font(font(12.5)).color(c::TEXT_2))).clicked() {
        cancel(app);
        return;
    }
    let existing = app.feat.devices.add.as_ref().and_then(|f| f.existing.clone());
    let title = match &existing {
        Some(d) => format!("{}의 폴더 추가", d.name),
        None => "기기 추가".into(),
    };
    ui.label(RichText::new(title).font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
    ui.add(
        egui::Label::new(
            RichText::new(
                "다른 컴퓨터를 한 번 등록하면 그 컴퓨터의 작업 공간을 이 사이드바에서 열 수 있어요. 실행은 그 컴퓨터에서 계속돼요.",
            )
            .font(font(ty::BODY))
            .color(c::TEXT_2),
        )
        .wrap(),
    );
    ui.add_space(14.0);
    steps(ui, step);
    ui.add_space(18.0);
    let ctx = ui.ctx().clone();
    card(ui, |ui| match step {
        Step::Pick => pick(app, ui, &ctx),
        Step::Connect => connect(app, ui, &ctx),
        Step::Prepare => prepare(app, ui, &ctx),
        Step::Folder => folder(app, ui, &ctx),
        Step::Done => done(app, ui),
    });
}

fn steps(ui: &mut egui::Ui, step: Step) {
    let w = ui.available_width();
    let gap = 6.0;
    let each = (w - gap * 4.0) / 5.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 30.0), egui::Sense::hover());
    let p = ui.painter();
    for (i, name) in STEPS.iter().enumerate() {
        let n = i + 1;
        let cur = n == step as usize;
        let done = n < step as usize;
        let x = rect.left() + i as f32 * (each + gap);
        let bar = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(each, 3.0));
        let bar_c = if done {
            c::MINT
        } else if cur {
            c::TEXT
        } else {
            c::CONTROL
        };
        p.rect_filled(bar, CornerRadius::same(2), bar_c);
        let cy = rect.top() + 3.0 + 6.0 + 9.0;
        let center = egui::pos2(x + 9.0, cy);
        let (dot_bg, dot_fg) = if done {
            (egui::Color32::from_rgb(0x17, 0x3A, 0x2C), c::MINT)
        } else if cur {
            (c::TEXT_STRONG, c::DOC)
        } else {
            (c::CONTROL, c::TEXT_3)
        };
        p.circle_filled(center, 9.0, dot_bg);
        if done {
            p.text(center, egui::Align2::CENTER_CENTER, crate::icons::CHECK, theme::icon_font(10.0), dot_fg);
        } else {
            p.text(center, egui::Align2::CENTER_CENTER, n.to_string(), semibold(10.5), dot_fg);
        }
        let fg = if cur {
            c::TEXT_STRONG
        } else if done {
            c::TEXT_2
        } else {
            c::TEXT_OFF
        };
        let label_rect = egui::Rect::from_min_max(egui::pos2(x + 24.0, cy - 9.0), egui::pos2(x + each, cy + 9.0));
        crate::shell::widgets::paint_clipped(ui, label_rect, name, font(ty::SMALL), fg, true);
    }
}

/// Footer with a note, `이전` and the one primary action.
fn footer(ui: &mut egui::Ui, note: &str, prev: bool, next: &str, can_next: bool) -> (bool, bool) {
    let mut out = (false, false);
    let r = ui.cursor();
    ui.painter().hline(ui.max_rect().x_range(), r.top(), egui::Stroke::new(1.0, c::LINE));
    egui::Frame::new()
        .fill(c::PANEL_2)
        .corner_radius(CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 })
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let b = if can_next {
                        mark(next, theme::primary_button(ui, next))
                    } else {
                        ui.add(
                            egui::Button::new(RichText::new(next).font(medium(ty::BODY)).color(c::TEXT_OFF))
                                .fill(c::CONTROL)
                                .corner_radius(CornerRadius::same(7))
                                .min_size(egui::vec2(0.0, 28.0)),
                        )
                    };
                    out.1 = b.clicked() && can_next;
                    if prev && mark("이전", theme::secondary_button(ui, "이전")).clicked() {
                        out.0 = true;
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(note).font(font(ty::SMALL)).color(c::TEXT_3)).wrap());
                    });
                });
            });
        });
    out
}

// ── 1. 기기 고르기 ──────────────────────────────────────────────────────────

fn pick(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    if let Some(f) = flow(app)
        && !f.searched
    {
        f.searched = true;
        bg(app, ctx, tailscale::discover, |app, found| {
            if let Some(f) = flow(app) {
                f.found = Some(found);
            }
        });
    }
    let registered = remote::devices(app);
    let Some(f) = flow(app) else { return };
    card_head(
        ui,
        "어떤 컴퓨터를 추가할까요?",
        "Tailscale에서 찾은 내 기기예요. Tailscale 없이 SSH 주소로도 추가할 수 있어요.",
    );
    egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 8 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match f.found.clone() {
            Some(Found::NotInstalled) => {
                note_line(ui, "이 컴퓨터에서 Tailscale을 찾지 못했어요. 아래에 SSH 주소를 입력하세요.")
            }
            Some(Found::Stopped(s)) => note_line(
                ui,
                &format!(
                    "Tailscale이 연결돼 있지 않아요{}. 아래에 SSH 주소를 입력하세요.",
                    if s.is_empty() { String::new() } else { format!(" ({s})") }
                ),
            ),
            Some(Found::Failed(e)) => note_line(ui, &format!("Tailscale 기기 목록을 읽지 못했어요 · {e}")),
            Some(Found::Peers(peers)) => {
                if peers.iter().all(|p| p.is_self) {
                    note_line(ui, "Tailscale에 다른 기기가 없어요. 아래에 SSH 주소를 입력하세요.");
                }
                for p in &peers {
                    let taken = registered
                        .iter()
                        .find(|d| d.target == p.address() || d.target.ends_with(&format!("@{}", p.address())));
                    peer_row(ui, f, p, taken.map(|d| d.name.as_str()));
                }
            }
            None => note_line(ui, "Tailscale에서 기기를 찾는 중…"),
        }
    });
    let r = ui.cursor();
    ui.painter().hline(ui.max_rect().x_range(), r.top(), egui::Stroke::new(1.0, c::LINE));
    egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 14 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.columns(2, |cols| {
            cols[0].label(RichText::new("이 기기를 부를 이름").font(font(ty::SMALL)).color(c::TEXT_2));
            let r = cols[0].add(
                egui::TextEdit::singleline(&mut f.name)
                    .hint_text("예: 집 Mac mini")
                    .font(font(ty::BODY))
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, 32.0))
                    .align(egui::Align2::LEFT_CENTER),
            );
            if r.changed() {
                f.name_edited = true;
            }
            cols[1].label(RichText::new("또는 SSH 주소로 추가").font(font(ty::SMALL)).color(c::TEXT_2));
            let r = cols[1].add(
                egui::TextEdit::singleline(&mut f.address)
                    .hint_text("user@host 또는 192.168.0.12")
                    .font(mono(ty::MONO))
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, 32.0))
                    .align(egui::Align2::LEFT_CENTER),
            );
            if r.changed() {
                f.peer = None;
                f.use_sim = false;
                f.pick_error = None;
                if !f.name_edited {
                    f.name = default_name(&f.address);
                }
            }
        });
        if ssh::simulated_allowed() {
            ui.add_space(10.0);
            ui.label(
                RichText::new("개발용 시뮬레이션 기기 · 이 컴퓨터의 다른 데이터 폴더")
                    .font(font(ty::SMALL))
                    .color(c::YELLOW),
            );
            let r = ui.add(
                egui::TextEdit::singleline(&mut f.sim_home)
                    .hint_text("/tmp/…/home")
                    .font(mono(ty::MONO))
                    .desired_width(f32::INFINITY),
            );
            mark("sim-home", r.clone());
            if r.changed() {
                f.use_sim = !f.sim_home.trim().is_empty();
                f.peer = None;
                f.pick_error = None;
                if !f.name_edited {
                    f.name = "시뮬레이션 기기".into();
                }
            }
        }
        if let Some(e) = &f.pick_error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).font(font(ty::SMALL)).color(c::ROSE));
        }
    });
    let chosen = f.peer.is_some() || !f.address.trim().is_empty() || f.use_sim;
    let can = chosen && !f.name.trim().is_empty();
    let note = if !chosen {
        "기기를 고르거나 SSH 주소를 입력하세요."
    } else if f.name.trim().is_empty() {
        "이 기기를 부를 이름을 입력하세요."
    } else {
        ""
    };
    let (_, next) = footer(ui, note, false, "다음", can);
    if next {
        choose_target(app, ctx);
    }
}

fn default_name(address: &str) -> String {
    let host = address.trim().rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or(host);
    host.split('.').next().unwrap_or(host).to_string()
}

fn note_line(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).font(font(12.5)).color(c::TEXT_3)).wrap());
    });
}

fn peer_row(ui: &mut egui::Ui, f: &mut Flow, p: &Peer, taken: Option<&str>) {
    let disabled = p.is_self || p.is_phone() || taken.is_some();
    let on = f.peer.as_deref() == Some(p.address().as_str()) && !disabled;
    let meta = if p.is_self {
        format!("{} · 지금 쓰는 컴퓨터 · 등록 없이 이미 쓰고 있어요", p.os_label())
    } else if p.is_phone() {
        format!("{} · 휴대폰은 실행 기기로 쓸 수 없어요", p.os_label())
    } else if let Some(n) = taken {
        format!("{} · 이미 {} 등록했어요", p.os_label(), josa::euro_ro(&format!("‘{n}’")))
    } else {
        format!("{} · Tailscale {}", p.os_label(), if p.direct { "직접 연결" } else { "중계 연결" })
    };
    let state = if disabled {
        ""
    } else if p.online {
        "온라인"
    } else {
        "오프라인"
    };
    let w = ui.available_width();
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(w, 46.0), if disabled { egui::Sense::hover() } else { egui::Sense::click() });
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, !disabled, on, &p.host));
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, CornerRadius::same(8), c::CONTROL);
    } else if resp.hovered() && !disabled {
        painter.rect_filled(rect, CornerRadius::same(8), c::ROW);
    }
    let ring_c = egui::pos2(rect.left() + 17.0, rect.center().y);
    if on {
        painter.circle_stroke(ring_c, 5.0, egui::Stroke::new(4.5, c::TEXT_STRONG));
    } else {
        painter.circle_stroke(
            ring_c,
            6.25,
            egui::Stroke::new(1.5, if disabled { c::LINE_STRONG } else { c::TEXT_OFF }),
        );
    }
    let fg = if disabled { c::TEXT_OFF } else { c::TEXT_STRONG };
    let text_left = rect.left() + 34.0;
    let state_w = if state.is_empty() { 0.0 } else { 60.0 };
    let tr = egui::Rect::from_min_max(
        egui::pos2(text_left, rect.top() + 6.0),
        egui::pos2(rect.right() - 10.0 - state_w, rect.center().y + 1.0),
    );
    crate::shell::widgets::paint_clipped(ui, tr, &p.address(), mono(12.5), fg, true);
    let mr = egui::Rect::from_min_max(
        egui::pos2(text_left, rect.center().y + 1.0),
        egui::pos2(rect.right() - 10.0 - state_w, rect.bottom() - 4.0),
    );
    crate::shell::widgets::paint_clipped(ui, mr, &meta, font(ty::SMALL), c::TEXT_3, true);
    if !state.is_empty() {
        let col = if p.online { c::MINT } else { c::TEXT_3 };
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            state,
            font(ty::SMALL),
            col,
        );
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(8));
    mark(&format!("peer:{}", p.host), resp.clone());
    if resp.clicked() && !disabled {
        f.peer = Some(p.address());
        f.address.clear();
        f.use_sim = false;
        f.pick_error = None;
        if !f.name_edited {
            f.name = p.host.clone();
        }
    }
}

fn choose_target(app: &mut App, ctx: &egui::Context) {
    let taken: Vec<String> = remote::devices(app).into_iter().map(|d| d.name).collect();
    let Some(f) = flow(app) else { return };
    if let Some(other) = taken.iter().find(|n| n.trim().to_lowercase() == f.name.trim().to_lowercase()) {
        f.pick_error = Some(format!("‘{other}’ 이름의 기기가 이미 있어요. 다른 이름을 쓰세요"));
        return;
    }
    let (target, port, via) = if f.use_sim {
        let home = f.sim_home.trim();
        if !home.starts_with('/') {
            f.pick_error = Some("시뮬레이션 기기의 데이터 폴더는 절대 경로로 입력하세요".into());
            return;
        }
        (format!("{SIMULATED_PREFIX}{home}"), None, Some("시뮬레이션".to_string()))
    } else {
        let raw = f.peer.clone().unwrap_or_else(|| f.address.clone());
        match parse_target(&raw) {
            Ok((t, p)) => (t, p, f.peer.as_ref().map(|_| "Tailscale".to_string())),
            Err(e) => {
                f.pick_error = Some(e);
                return;
            }
        }
    };
    if f.target != target || f.port != port {
        f.reach = Check::Idle;
        f.key = KeyCheck::Idle;
        f.login = Check::Idle;
        f.ready = Ready::Idle;
        f.resolved = None;
    }
    f.target = target;
    f.port = port;
    f.via = via;
    f.name = f.name.trim().to_string();
    f.step = Step::Connect;
    run_checks(app, ctx);
}

// ── 2. 연결 확인 ────────────────────────────────────────────────────────────

/// Run the connection checks from the first one that is not done.
fn run_checks(app: &mut App, ctx: &egui::Context) {
    let Some(f) = flow(app) else { return };
    if f.sim() {
        f.reach = Check::Ok(None);
        f.key = KeyCheck::Skipped("이 컴퓨터의 다른 데이터 폴더라 호스트 키가 없어요 (개발용)".into());
        f.login = Check::Ok(Platform::local());
        return;
    }
    let (target, port) = (f.target.clone(), f.port);
    if matches!(f.reach, Check::Running) || matches!(f.key, KeyCheck::Running) || matches!(f.login, Check::Running) {
        return;
    }
    f.reach = Check::Running;
    if !matches!(f.key, KeyCheck::Accepted(_)) {
        f.key = KeyCheck::Idle;
    }
    f.login = Check::Idle;
    bg(
        app,
        ctx,
        move || {
            let r = probe::resolve(&target, port);
            let reach = r.as_ref().ok().map(|r| if r.proxied { Ok(None) } else { probe::reach(r).map(Some) });
            (r, reach)
        },
        {
            let ctx = ctx.clone();
            move |app, (r, reach)| {
                let Some(f) = flow(app) else { return };
                match (r, reach) {
                    (Err(e), _) => f.reach = Check::Failed(Failure::new(ssh::Kind::Other, e)),
                    (Ok(r), Some(Err(e))) => {
                        f.resolved = Some(r);
                        f.reach = Check::Failed(e);
                    }
                    (Ok(r), reach) => {
                        f.resolved = Some(r);
                        f.reach = Check::Ok(reach.and_then(|x| x.ok()).flatten());
                        check_key(app, &ctx);
                    }
                }
            }
        },
    );
}

fn check_key(app: &mut App, ctx: &egui::Context) {
    let Some(f) = flow(app) else { return };
    let Some(r) = f.resolved.clone() else { return };
    let accepted = match &f.key {
        KeyCheck::Accepted(k) => Some(k.clone()),
        _ => None,
    };
    f.key = KeyCheck::Running;
    bg(
        app,
        ctx,
        move || {
            let known = probe::known_keys(&r);
            if let Some(k) = known.into_iter().next() {
                return Ok((k, true));
            }
            let first = probe::scan(&r)?.into_iter().next();
            first.map(|k| (k, false)).ok_or_else(|| Failure::new(ssh::Kind::Other, "호스트 키를 읽지 못했어요"))
        },
        {
            let ctx = ctx.clone();
            move |app, res| {
                let Some(f) = flow(app) else { return };
                match res {
                    Ok((k, true)) => {
                        f.key = KeyCheck::Known(k);
                        login(app, &ctx, false);
                    }
                    // The same key the person already confirmed in this flow.
                    Ok((k, false)) if accepted.as_ref() == Some(&k) => {
                        f.key = KeyCheck::Accepted(k);
                        login(app, &ctx, true);
                    }
                    Ok((k, false)) => f.key = KeyCheck::Ask(k),
                    Err(e) => f.key = KeyCheck::Failed(e),
                }
            }
        },
    );
}

fn login(app: &mut App, ctx: &egui::Context, accept_new: bool) {
    let Some(f) = flow(app) else { return };
    let (target, port) = (f.target.clone(), f.port);
    let resolved = f.resolved.clone();
    let expected = match &f.key {
        KeyCheck::Accepted(k) => Some(k.clone()),
        _ => None,
    };
    f.login = Check::Running;
    bg(
        app,
        ctx,
        move || {
            let p = probe::login(&target, port, accept_new)?;
            // The key ssh just remembered must be the one the person compared.
            if let (Some(want), Some(r)) = (expected, resolved)
                && !probe::known_keys(&r).contains(&want)
            {
                return Err(Failure::new(ssh::Kind::ChangedKey, "확인한 지문과 다른 키가 저장됐어요"));
            }
            Ok(p)
        },
        |app, res| {
            if let Some(f) = flow(app) {
                f.login = match res {
                    Ok(p) => Check::Ok(p),
                    Err(e) => Check::Failed(e),
                };
            }
        },
    );
}

fn connect(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    let Some(f) = flow(app) else { return };
    let name = f.name.clone();
    card_head(
        ui,
        &format!("{name}에 연결할 수 있는지 확인해요"),
        "실패하면 이 단계에서 다시 확인해요. 이 컴퓨터에서 대신 실행하지 않아요.",
    );
    let mut retry = false;
    let mut accept = false;
    let addr = match &f.resolved {
        Some(r) if r.port != 22 => format!("{}:{}", r.host, r.port),
        Some(r) => r.host.clone(),
        None => f.target.clone(),
    };
    let via = f.via.clone().map(|v| format!(" · {v}")).unwrap_or_default();
    match &f.reach {
        Check::Idle | Check::Running => check_row(ui, Mark::Busy, "닿는지 확인하는 중…", &addr, |_| {}),
        Check::Ok(Some(d)) => check_row(ui, Mark::Ok, "닿아요", &format!("{addr}{via} · {}ms", d.as_millis()), |_| {}),
        Check::Ok(None) if f.sim() => {
            check_row(ui, Mark::Ok, "닿아요", "이 컴퓨터의 다른 데이터 폴더예요 (개발용)", |_| {})
        }
        Check::Ok(None) => {
            check_row(ui, Mark::Ok, "SSH 설정의 프록시를 거쳐 연결해요", &format!("{addr}{via}"), |_| {})
        }
        Check::Failed(e) => {
            check_row(ui, Mark::Bad, &format!("닿지 않아요 · {}", e.cause.label()), &e.explain(&name), |ui| {
                retry |= small_button(ui, "다시 확인").clicked();
            })
        }
    }
    match &f.key {
        KeyCheck::Idle => check_row(ui, Mark::Wait, "기기 확인", "닿은 뒤에 확인해요.", |_| {}),
        KeyCheck::Running => check_row(ui, Mark::Busy, "기기를 확인하는 중…", "", |_| {}),
        KeyCheck::Known(k) => {
            check_row(ui, Mark::Ok, "알고 있는 기기예요", &format!("{} · {}", k.algo, k.fingerprint), |_| {})
        }
        KeyCheck::Ask(k) => check_row(
            ui,
            Mark::Ask,
            "처음 보는 기기예요",
            &format!("지문이 {name}에서 보이는 값과 같은지 확인하세요 · {} {}", k.algo, k.fingerprint),
            |ui| {
                accept |= small_button(ui, "같아요")
                    .on_hover_text(format!(
                        "{name}에서 확인하는 방법: ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub"
                    ))
                    .clicked();
            },
        ),
        KeyCheck::Accepted(k) => check_row(
            ui,
            Mark::Ok,
            "처음 보는 기기를 확인했어요",
            &format!("{} · 다음부터는 이 지문이 다르면 연결하지 않아요", k.fingerprint),
            |_| {},
        ),
        KeyCheck::Skipped(m) => check_row(ui, Mark::Ok, "기기 확인 생략", m, |_| {}),
        KeyCheck::Failed(e) => {
            check_row(ui, Mark::Bad, "호스트 키를 받지 못했어요", &e.explain(&name), |ui| {
                retry |= small_button(ui, "다시 확인").clicked();
            })
        }
    }
    let mut help = false;
    match &f.login {
        Check::Idle => check_row(ui, Mark::Wait, "로그인", "기기를 확인한 뒤에 해요.", |_| {}),
        Check::Running => check_row(ui, Mark::Busy, "로그인하는 중…", "", |_| {}),
        Check::Ok(p) => {
            let user = f.resolved.as_ref().map(|r| format!("{} 계정 · ", r.user)).unwrap_or_default();
            let how = if f.sim() { "개발용".to_string() } else { format!("{user}SSH 키") };
            check_row(ui, Mark::Ok, "로그인됨", &format!("{how} · {}", p.label()), |_| {})
        }
        Check::Failed(e) => {
            let title = match e.cause {
                ssh::Cause::Auth => "SSH 로그인 실패".to_string(),
                c => format!("로그인하지 못했어요 · {}", c.label()),
            };
            let auth = e.cause == ssh::Cause::Auth;
            check_row(ui, Mark::Bad, &title, &e.explain(&name), |ui| {
                retry |= small_button(ui, "다시 확인").clicked();
                if auth {
                    help |= theme::link_button(ui, "등록 방법 보기").clicked();
                }
            });
        }
    }
    if help {
        f.show_key_help = !f.show_key_help;
    }
    if f.show_key_help {
        key_help(ui, f);
    }
    let ok = matches!(f.login, Check::Ok(_));
    let note = if ok { "" } else { "모든 확인이 끝나야 다음으로 갈 수 있어요." };
    let (prev, next) = footer(ui, note, true, "다음", ok);
    if accept && let KeyCheck::Ask(k) = f.key.clone() {
        f.key = KeyCheck::Accepted(k);
        login(app, ctx, true);
    } else if retry {
        run_checks(app, ctx);
    } else if prev {
        if let Some(f) = flow(app) {
            f.step = Step::Pick;
        }
    } else if next {
        if let Some(f) = flow(app) {
            f.step = Step::Prepare;
        }
        find_runwick(app, ctx);
    }
}

fn key_help(ui: &mut egui::Ui, f: &Flow) {
    let dest = if f.port.is_some_and(|p| p != 22) {
        format!("-p {} {}", f.port.unwrap_or(22), f.target)
    } else {
        f.target.clone()
    };
    egui::Frame::new()
        .fill(c::ROW)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .outer_margin(egui::Margin { left: 16, right: 16, top: 0, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(
                RichText::new("이 컴퓨터의 공개 키를 기기에 등록하는 방법").font(medium(12.5)).color(c::TEXT_STRONG),
            );
            let lines = [
                ("1. 키가 없으면 만들어요", "ssh-keygen -t ed25519".to_string()),
                ("2. 기기에 공개 키를 등록해요 (그 기기의 비밀번호를 한 번 물어요)", format!("ssh-copy-id {dest}")),
            ];
            for (what, cmd) in lines {
                ui.label(RichText::new(what).font(font(ty::SMALL)).color(c::TEXT_2));
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&cmd).font(mono(12.0)).color(c::TEXT)).selectable(true));
                    if theme::icon_button(ui, crate::icons::COPY, &format!("복사: {cmd}")).clicked() {
                        ui.ctx().copy_text(cmd.clone());
                    }
                });
            }
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "터미널에서 직접 실행한 뒤 ‘다시 확인’을 누르세요. Runwick은 SSH 설정을 바꾸지 않아요.",
                    )
                    .font(font(ty::SMALL))
                    .color(c::TEXT_3),
                )
                .wrap(),
            );
        });
}

// ── 3. 쓸 준비 ──────────────────────────────────────────────────────────────

fn find_runwick(app: &mut App, ctx: &egui::Context) {
    let exe = app.exe.clone();
    let Some(f) = flow(app) else { return };
    if f.sim() {
        f.ready = Ready::Found(Installed { path: exe.display().to_string(), version: runwick_core::VERSION.into() });
        start_connection(app);
        return;
    }
    let (target, port) = (f.target.clone(), f.port);
    f.ready = Ready::Running;
    bg(
        app,
        ctx,
        move || probe::find_runwick(&target, port),
        |app, res| {
            let Some(f) = flow(app) else { return };
            f.ready = match res {
                Ok(Some(i)) => Ready::Found(i),
                Ok(None) => Ready::Missing,
                Err(e) => Ready::Failed(e),
            };
            if matches!(&f.ready, Ready::Found(i) if i.version == runwick_core::VERSION) {
                start_connection(app);
            }
        },
    );
}

fn install(app: &mut App, ctx: &egui::Context) {
    let exe = app.exe.clone();
    let Some(f) = flow(app) else { return };
    let (target, port, id) = (f.target.clone(), f.port, f.id);
    remote::forget(app, id);
    if let Some(f) = flow(app) {
        f.ready = Ready::Installing;
        f.connected_at = None;
    }
    bg(
        app,
        ctx,
        move || probe::install(&target, port, &exe),
        |app, res| {
            let Some(f) = flow(app) else { return };
            f.ready = match res {
                Ok(i) => Ready::Found(i),
                Err(e) => Ready::Failed(e),
            };
            if matches!(&f.ready, Ready::Found(i) if i.version == runwick_core::VERSION) {
                start_connection(app);
            }
        },
    );
}

/// Connect to the device's host under the flow's id.
fn start_connection(app: &mut App) {
    let exe = app.exe.clone();
    let Some(f) = flow(app) else { return };
    let route = if let Some(home) = f.target.strip_prefix(SIMULATED_PREFIX) {
        Route::Simulated { home: home.into(), exe }
    } else if let Ready::Found(i) = &f.ready {
        Route::Ssh { target: f.target.clone(), port: f.port, exe: i.path.clone() }
    } else {
        return;
    };
    f.connected_at = None;
    let id = f.id;
    remote::connect_route(app, id, route);
}

/// Why the connected host cannot be added (another version still running there, this
/// computer, or a device registered already).
fn identity_problem(app: &mut App, id: DeviceId) -> Option<String> {
    let info = app.hub.state(HostKey::Device(id)).info().cloned()?;
    if info.version != runwick_core::VERSION {
        return Some(format!(
            "그 기기에서 Runwick {} 이미 실행 중이라 이 컴퓨터의 {} 맞지 않아요. 그 기기에서 실행 중인 작업을 정리하고 Runwick을 다시 시작한 뒤 다시 확인하세요.",
            josa::i_ga(&info.version),
            josa::gwa_wa(runwick_core::VERSION)
        ));
    }
    if app.hub.local_info().is_some_and(|l| l.host_id == info.host_id) {
        return Some("지금 쓰는 컴퓨터예요. 등록하지 않아도 이미 쓰고 있어요.".into());
    }
    let dup = remote::devices(app).into_iter().find(|d| d.remote_host_id == Some(info.host_id) && d.id != id);
    dup.map(|d| format!("이미 {} 등록된 기기예요.", josa::euro_ro(&format!("‘{}’", d.name))))
}

fn prepare(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    let Some(id) = flow(app).map(|f| f.id) else {
        return;
    };
    let host = HostKey::Device(id);
    let conn = app.hub.state(host);
    let failure = remote::status(app, id);
    let problem = identity_problem(app, id);
    let connected = conn.is_connected() && problem.is_none();
    let tools = if connected { app.hub.query::<ToolsList>(host, &ToolsQuery::default()).data } else { None };
    let platform_ok = flow(app).is_some_and(|f| matches!(&f.login, Check::Ok(p) if *p == Platform::local()));
    let Some(f) = flow(app) else { return };
    if connected && f.connected_at.is_none() {
        f.connected_at = Some(std::time::Instant::now());
    }
    let name = f.name.clone();
    card_head(
        ui,
        &format!("{name}에서 쓸 준비"),
        &format!("도구와 로그인은 {name} 기준이에요. 이 컴퓨터의 로그인과 설정을 옮기지 않아요."),
    );
    let mut act_install = false;
    let mut retry = false;
    let ours = runwick_core::VERSION;
    let local = Platform::local();
    let theirs = match &f.login {
        Check::Ok(p) => p.label(),
        _ => "확인 불가".into(),
    };
    match f.ready.clone() {
        Ready::Idle | Ready::Running => check_row(ui, Mark::Busy, "Runwick을 찾는 중…", "", |_| {}),
        Ready::Installing => check_row(
            ui,
            Mark::Busy,
            "Runwick 설치 중",
            &format!("{name}에 실행 유지용 Runwick만 설치해요. 화면 앱은 설치하지 않아요."),
            |_| {},
        ),
        Ready::Missing if platform_ok => check_row(
            ui,
            Mark::Ask,
            "Runwick이 없어요",
            &format!(
                "실행을 유지하려면 {name}에 Runwick이 필요해요. 이 컴퓨터의 Runwick {ours}을 ~/.runwick/bin에 복사해요."
            ),
            |ui| {
                act_install |= small_button(ui, "설치").clicked();
            },
        ),
        Ready::Missing => check_row(
            ui,
            Mark::Bad,
            "Runwick이 없어요",
            &format!(
                "이 컴퓨터는 {}, {name}은 {theirs}라 그대로 복사할 수 없어요. {name}에 Runwick {ours}을 설치한 뒤 다시 확인하세요.",
                local.label()
            ),
            |ui| {
                retry |= small_button(ui, "다시 확인").clicked();
            },
        ),
        Ready::Found(i) if i.version != ours => check_row(
            ui,
            Mark::Ask,
            &format!("Runwick 버전이 달라요 · {name} {} · 이 컴퓨터 {ours}", i.version),
            &if platform_ok {
                format!("같은 버전이어야 연결해요. 이 컴퓨터의 Runwick {ours}을 ~/.runwick/bin에 복사할 수 있어요.")
            } else {
                format!("같은 버전이어야 연결해요. {name}의 Runwick을 {ours}로 바꾼 뒤 다시 확인하세요.")
            },
            |ui| {
                if platform_ok {
                    act_install |= small_button(ui, &format!("{ours}로 바꾸기")).clicked();
                } else {
                    retry |= small_button(ui, "다시 확인").clicked();
                }
            },
        ),
        Ready::Found(i) => {
            let (mark, title, msg) = match (&conn, &problem) {
                (_, Some(p)) => (Mark::Bad, format!("Runwick {} · 연결할 수 없어요", i.version), p.clone()),
                (ConnState::Connected(_), None) => (
                    Mark::Ok,
                    format!("Runwick {} 설치됨", i.version),
                    "이 컴퓨터와 같은 버전이에요. 창을 닫거나 연결이 끊겨도 실행을 유지해요.".to_string(),
                ),
                (ConnState::Connecting, None) => {
                    (Mark::Busy, format!("Runwick {} · 연결하는 중…", i.version), i.path.clone())
                }
                (ConnState::Disconnected { .. }, None) => {
                    let why =
                        remote::explain(&failure, &name).map(|x| x.1).unwrap_or_else(|| "연결하지 못했어요".into());
                    (Mark::Bad, format!("Runwick {} · 연결하지 못했어요", i.version), why)
                }
            };
            let show_retry = mark == Mark::Bad;
            check_row(ui, mark, &title, &msg, |ui| {
                if show_retry {
                    retry |= small_button(ui, "다시 확인").clicked();
                }
            });
        }
        Ready::Failed(e) => check_row(
            ui,
            Mark::Bad,
            &format!("확인하지 못했어요 · {}", e.cause.label()),
            &e.explain(&name),
            |ui| {
                retry |= small_button(ui, "다시 확인").clicked();
            },
        ),
    }
    let mut recheck_tools = false;
    if connected {
        match &tools {
            None => check_row(ui, Mark::Busy, "도구를 확인하는 중…", "", |_| {}),
            Some(list) => {
                for t in list.iter() {
                    let (mark, title, msg) = tool_line(t, &name);
                    let needs = t.tool != Tool::Shell && t.installed && t.auth != AuthState::LoggedIn;
                    check_row(ui, mark, &title, &msg, |ui| {
                        if needs {
                            recheck_tools |= small_button(ui, "다시 확인").clicked();
                        }
                    });
                }
            }
        }
    }
    let ready = connected;
    let note = if ready { "" } else { "Runwick이 준비돼야 실행을 유지할 수 있어요." };
    let (prev, next) = footer(ui, note, true, "다음", ready);
    if act_install {
        install(app, ctx);
    } else if retry {
        find_runwick(app, ctx);
    } else if recheck_tools {
        app.hub.send::<ToolsList>(host, ToolsQuery { refresh: true });
    } else if prev {
        if let Some(f) = flow(app) {
            f.step = Step::Connect;
        }
    } else if next && let Some(f) = flow(app) {
        f.step = Step::Folder;
    }
}

fn tool_line(t: &runwick_core::api::tools::ToolInfo, name: &str) -> (Mark, String, String) {
    let v = t.version.clone().map(|v| format!(" {v}")).unwrap_or_default();
    match t.tool {
        Tool::Shell => {
            let shell = data::shell_name(t);
            if t.installed {
                (Mark::Ok, shell, "기본 셸을 확인했어요.".into())
            } else {
                (Mark::Bad, "셸을 찾지 못했어요".into(), String::new())
            }
        }
        tool if !t.installed => (
            Mark::Missing,
            format!("{} · {}", tool.label(), data::NOT_INSTALLED),
            format!("{name}에 설치하면 쓸 수 있어요. 지금 안 해도 추가할 수 있어요."),
        ),
        tool => match t.auth {
            AuthState::LoggedIn => {
                (Mark::Ok, format!("{}{v} · 로그인됨", tool.label()), format!("{name}의 로그인을 써요."))
            }
            AuthState::NeedsLogin => (
                Mark::Wait,
                format!("{}{v} · 로그인 필요", tool.label()),
                format!("{name}에서 로그인해야 해요. 지금 안 해도 추가할 수 있어요."),
            ),
            AuthState::Unknown => (
                Mark::Wait,
                format!("{}{v} · 로그인 확인 불가", tool.label()),
                format!("{name}에서 처음 실행할 때 확인돼요."),
            ),
        },
    }
}

// ── 4. 폴더 고르기 ──────────────────────────────────────────────────────────

fn folder(app: &mut App, ui: &mut egui::Ui, _ctx: &egui::Context) {
    let Some(id) = flow(app).map(|f| f.id) else {
        return;
    };
    let host = HostKey::Device(id);
    let connected = app.hub.state(host).is_connected();
    let dir = flow(app).and_then(|f| f.dir.clone());
    let listing = connected
        .then(|| app.hub.query::<DevicesBrowseDirs>(host, &BrowseDirs { path: dir.clone(), show_hidden: false }));
    let projects = data::projects(app).data;
    let name = flow(app).map(|f| f.name.clone()).unwrap_or_default();
    let Some(f) = flow(app) else { return };
    card_head(ui, &format!("{name}의 폴더를 골라요"), "고른 폴더가 작업 공간이 돼요.");
    let mut go: Option<Option<String>> = None;
    egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(q) = listing else {
            note_line(ui, &format!("{name}에 연결돼야 폴더를 고를 수 있어요."));
            return;
        };
        let Some(l) = q.data else {
            match q.error {
                Some(e) => note_line(ui, &format!("폴더를 읽지 못했어요 · {}", e.message)),
                None => note_line(ui, "폴더를 읽는 중…"),
            }
            return;
        };
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if let Some(parent) = &l.parent
                && theme::icon_button(ui, crate::icons::ARROW_UP, "위 폴더").clicked()
            {
                go = Some(Some(parent.clone()));
            }
            theme::truncated(ui, &l.display, mono(12.5), c::TEXT_2);
        });
        dir_row(ui, f, &l.dir, &l.display, true, &mut go);
        if l.dirs.is_empty() {
            note_line(ui, "하위 폴더가 없어요.");
        }
        let mut rows = |ui: &mut egui::Ui| {
            for d in &l.dirs {
                let label =
                    if l.display == "~" { format!("~/{}", d.name) } else { format!("{}/{}", l.display, d.name) };
                dir_row(ui, f, d, &label, false, &mut go);
            }
            if l.more > 0 {
                note_line(
                    ui,
                    &format!("폴더가 많아 {}개는 보여 주지 않았어요. 더 안쪽 폴더로 들어가 고르세요.", l.more),
                );
            }
        };
        if l.dirs.len() > 7 {
            egui::ScrollArea::vertical()
                .id_salt(("device-dirs", &l.dir.path))
                .min_scrolled_height(322.0)
                .max_height(322.0)
                .show(ui, |ui| rows(ui));
        } else {
            rows(ui);
        }
    });
    if let Some(to) = go {
        f.dir = to;
    }
    // Where it shows up here.
    let mut can = false;
    if let Some(ch) = f.chosen.clone() {
        let key = ch.remote_url.as_deref().and_then(probe::repo_key);
        let list: Vec<_> = projects.as_ref().map(|l| data::visible_projects(l).cloned().collect()).unwrap_or_default();
        let same: Vec<_> = list
            .iter()
            .filter(|p| key.is_some() && p.remote_url.as_deref().and_then(probe::repo_key) == key)
            .cloned()
            .collect();
        if f.project_name.is_empty() {
            f.project_name = format!("{} ({name})", ch.name);
        }
        let r = ui.cursor();
        ui.painter().hline(ui.max_rect().x_range(), r.top(), egui::Stroke::new(1.0, c::LINE));
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 14 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            if let Some(p) = same.first() {
                ui.label(
                    RichText::new(format!("이 컴퓨터의 {}와 같은 저장소예요", p.name))
                        .font(medium(ty::BODY))
                        .color(c::TEXT_STRONG),
                );
                ui.label(
                    RichText::new("주소가 같아도 자동으로 합치지 않아요. 고른 대로만 연결해요.")
                        .font(font(12.5))
                        .color(c::TEXT_3),
                );
            } else {
                ui.label(RichText::new("사이드바 어디에 보일지 골라요").font(medium(ty::BODY)).color(c::TEXT_STRONG));
            }
            ui.columns(2, |cols| {
                if let Some(p) = same.first() {
                    if join_option(
                        &mut cols[0],
                        f.join == Some(Join::Existing(p.id)),
                        &format!("{}의 공간으로 추가", p.name),
                        &format!("사이드바의 {} 아래에 함께 보여요.", p.name),
                    ) {
                        f.join = Some(Join::Existing(p.id));
                    }
                } else {
                    let on = matches!(f.join, Some(Join::Existing(_)));
                    if join_option(
                        &mut cols[0],
                        on,
                        "기존 프로젝트의 공간으로 추가",
                        "고른 프로젝트 아래에 함께 보여요.",
                    ) && !on
                    {
                        f.join = list.first().map(|p| Join::Existing(p.id));
                    }
                    if let Some(Join::Existing(pid)) = f.join.clone() {
                        let current = list.iter().find(|p| p.id == pid).map(|p| p.name.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt("join-project")
                            .selected_text(current)
                            .width(cols[0].available_width())
                            .show_ui(&mut cols[0], |ui| {
                                for p in &list {
                                    ui.selectable_value(&mut f.join, Some(Join::Existing(p.id)), &p.name);
                                }
                            });
                    }
                }
                if join_option(
                    &mut cols[1],
                    f.join == Some(Join::New),
                    "별도 프로젝트로 추가",
                    "이름이 같아도 다른 프로젝트로 다뤄요.",
                ) {
                    f.join = Some(Join::New);
                }
                if f.join == Some(Join::New) {
                    cols[1].add(
                        egui::TextEdit::singleline(&mut f.project_name)
                            .font(font(ty::BODY))
                            .desired_width(f32::INFINITY),
                    );
                }
            });
        });
        can = match &f.join {
            Some(Join::New) => !f.project_name.trim().is_empty(),
            Some(Join::Existing(_)) => true,
            None => false,
        };
    }
    if let Some(e) = &f.error {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 6)).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(e).font(font(ty::SMALL)).color(c::ROSE)).wrap());
        });
    }
    let note = if f.busy {
        "추가하는 중…"
    } else if f.chosen.is_none() {
        "작업 공간으로 쓸 폴더를 고르세요."
    } else if f.join.is_none() {
        "어디에 보일지 고르세요."
    } else {
        "추가해도 기존 공간과 실행은 바뀌지 않아요."
    };
    let has_prev = f.existing.is_none();
    let (prev, next) = footer(ui, note, has_prev, "추가", can && !f.busy && connected);
    if prev {
        f.step = Step::Prepare;
    } else if next {
        submit(app);
    }
}

fn dir_row(
    ui: &mut egui::Ui,
    f: &mut Flow,
    d: &DirInfo,
    label: &str,
    is_current: bool,
    go: &mut Option<Option<String>>,
) {
    let on = f.chosen.as_ref().is_some_and(|c| c.path == d.path);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 46.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, label));
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, CornerRadius::same(8), c::CONTROL);
    } else if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), c::ROW);
    }
    let ring = egui::pos2(rect.left() + 17.0, rect.center().y);
    if on {
        painter.circle_stroke(ring, 5.0, egui::Stroke::new(4.5, c::TEXT_STRONG));
    } else {
        painter.circle_stroke(ring, 6.25, egui::Stroke::new(1.5, c::TEXT_OFF));
    }
    let meta = match (d.is_git, &d.branch, &d.remote_url) {
        (true, b, u) => {
            let mut s = "Git".to_string();
            if let Some(b) = b {
                s.push_str(&format!(" · {b}"));
            }
            if let Some(k) = u.as_deref().and_then(probe::repo_key) {
                s.push_str(&format!(" · {k}"));
            }
            s
        }
        _ => "폴더".into(),
    };
    let meta = if is_current { format!("지금 보고 있는 폴더 · {meta}") } else { meta };
    let right = if is_current { rect.right() - 10.0 } else { rect.right() - 40.0 };
    let tr = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 34.0, rect.top() + 6.0),
        egui::pos2(right, rect.center().y + 1.0),
    );
    crate::shell::widgets::paint_clipped(ui, tr, label, mono(12.5), c::TEXT_STRONG, true);
    let mr = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 34.0, rect.center().y + 1.0),
        egui::pos2(right, rect.bottom() - 4.0),
    );
    crate::shell::widgets::paint_clipped(ui, mr, &meta, font(ty::SMALL), c::TEXT_3, true);
    let mut entered = false;
    if !is_current {
        let br = egui::Rect::from_center_size(egui::pos2(rect.right() - 20.0, rect.center().y), egui::vec2(26.0, 26.0));
        let b =
            ui.interact(br, ui.scope_id().with(("enter", &d.path)), egui::Sense::click()).on_hover_text("폴더 안으로");
        mark(&format!("enter:{label}"), b.clone());
        let fg = if b.hovered() { c::TEXT_STRONG } else { c::TEXT_3 };
        ui.painter().text(
            br.center(),
            egui::Align2::CENTER_CENTER,
            crate::icons::CHEVRON_RIGHT,
            theme::icon_font(13.0),
            fg,
        );
        if b.clicked() || resp.double_clicked() {
            *go = Some(Some(d.path.clone()));
            entered = true;
        }
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(8));
    mark(&format!("dir:{label}"), resp.clone());
    if resp.clicked() && !entered {
        if f.chosen.as_ref().is_none_or(|c| c.path != d.path) {
            f.join = None;
            f.project_name.clear();
        }
        f.chosen = Some(d.clone());
    }
}

fn join_option(ui: &mut egui::Ui, on: bool, title: &str, desc: &str) -> bool {
    let fill = if on { c::CONTROL } else { c::ROW };
    let r = egui::Frame::new()
        .fill(fill)
        .stroke(if on { egui::Stroke::new(1.5, c::TEXT) } else { egui::Stroke::NONE })
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
            ui.add(egui::Label::new(RichText::new(desc).font(font(ty::SMALL)).color(c::TEXT_2)).wrap());
        })
        .response;
    let resp = ui.interact(r.rect, ui.scope_id().with(("join", title)), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, title));
    theme::focus_ring(ui, &resp, CornerRadius::same(9));
    mark(&format!("join:{title}"), resp.clone());
    resp.clicked()
}

/// Open the folder on the device, register the device (first time) and link the workspace.
fn submit(app: &mut App) {
    let Some(f) = flow(app) else { return };
    let Some(ch) = f.chosen.clone() else { return };
    let Some(join) = f.join.clone() else { return };
    f.busy = true;
    f.error = None;
    let id = f.id;
    app.hub.call::<ProjectsOpen>(
        HostKey::Device(id),
        OpenFolder { path: ch.path.clone() },
        move |app, res| match res {
            Ok(opened) => register(app, opened, ch, join),
            Err(e) => {
                let name = flow_name(app);
                fail(app, format!("{name}에서 폴더를 열지 못했어요: {}", e.message))
            }
        },
    );
}

fn flow_name(app: &mut App) -> String {
    flow(app).map(|f| f.name.clone()).unwrap_or_default()
}

fn fail(app: &mut App, msg: String) {
    if let Some(f) = flow(app) {
        f.busy = false;
        f.error = Some(msg);
    }
}

fn register(app: &mut App, opened: Opened, ch: DirInfo, join: Join) {
    let Some(f) = flow(app) else { return };
    let (existing, id) = (f.existing.is_some(), f.id);
    if existing {
        link(app, opened, ch, join);
        return;
    }
    let info = app.hub.state(HostKey::Device(id)).info().cloned();
    let Some(f) = flow(app) else { return };
    let (os, arch) = match &f.login {
        Check::Ok(p) => (Some(p.os.clone()), Some(p.arch.clone())),
        _ => (None, None),
    };
    let fingerprint = match &f.key {
        KeyCheck::Known(k) | KeyCheck::Accepted(k) => Some(k.fingerprint.clone()),
        _ => None,
    };
    let remote_exe = match &f.ready {
        Ready::Found(i) if !f.sim() => Some(i.path.clone()),
        _ => None,
    };
    let p = AddDevice {
        id: Some(f.id),
        name: f.name.clone(),
        target: f.target.clone(),
        port: f.port,
        os: info.as_ref().map(|i| i.os.clone()).or(os),
        arch: info.as_ref().map(|i| i.arch.clone()).or(arch),
        remote_host_id: info.as_ref().map(|i| i.host_id),
        remote_version: info.as_ref().map(|i| i.version.clone()),
        remote_exe,
        fingerprint,
        via: f.via.clone(),
    };
    app.hub.call::<DevicesAdd>(HostKey::Local, p, move |app, res| match res {
        Ok(_) => link(app, opened, ch, join),
        Err(e) => fail(app, format!("기기를 등록하지 못했어요: {}", e.message)),
    });
}

fn link(app: &mut App, opened: Opened, ch: DirInfo, join: Join) {
    let Some(f) = flow(app) else { return };
    let project = match &join {
        Join::Existing(pid) => LinkProject::Existing { project_id: *pid },
        Join::New => LinkProject::New { name: f.project_name.trim().to_string() },
    };
    let p = LinkRemote {
        device_id: f.id,
        remote_workspace_id: opened.workspace.id,
        name: opened.workspace.name.clone(),
        path: ch.path.clone(),
        project,
        remote_url: ch.remote_url.clone(),
    };
    let new_project = matches!(join, Join::New).then(|| f.project_name.trim().to_string());
    app.hub.call::<WorkspacesLinkRemote>(HostKey::Local, p, move |app, res| match res {
        Ok(w) => {
            let project_name = new_project.clone().unwrap_or_else(|| {
                data::projects(app)
                    .data
                    .and_then(|l| data::project_of(&l, w.project_id).map(|p| p.name.clone()))
                    .unwrap_or_default()
            });
            if let Some(f) = flow(app) {
                f.busy = false;
                f.step = Step::Done;
                f.done = Some((w, project_name, new_project.is_some()));
            }
        }
        Err(e) => fail(app, format!("사이드바에 추가하지 못했어요: {}", e.message)),
    });
}

// ── 5. 완료 ─────────────────────────────────────────────────────────────────

fn done(app: &mut App, ui: &mut egui::Ui) {
    let Some(f) = flow(app) else { return };
    let Some((w, project, separate)) = f.done.clone() else {
        return;
    };
    let (name, id, existing) = (f.name.clone(), f.id, f.existing.is_some());
    let host = HostKey::Device(id);
    let tools = app.hub.query::<ToolsList>(host, &ToolsQuery::default()).data;
    // Workspaces already under that project (this computer's or other devices').
    let others: Vec<String> = data::workspaces(app)
        .data
        .map(|l| {
            l.iter()
                .filter(|x| x.project_id == w.project_id && x.id != w.id)
                .map(|x| match &x.remote {
                    Some(_) => format!("{} · 다른 기기", x.name),
                    None => x.name.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut open = false;
    let mut again = false;
    egui::Frame::new().inner_margin(egui::Margin::same(16)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            let side = 250.0_f32.min(ui.available_width() * 0.4);
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width() - side - 24.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.horizontal(|ui| {
                    ui.label(theme::icon(crate::icons::CHECK, 16.0, c::MINT));
                    ui.label(RichText::new(format!("{} 추가했어요", josa::eul_reul(&name))).font(semibold(15.0)).color(c::TEXT_STRONG));
                });
                let text = if separate {
                    format!("사이드바에 ‘{project}’ 프로젝트가 생겼어요.")
                } else {
                    format!("사이드바 {project} 아래에 {} 생겼어요.", josa::i_ga(&format!("‘{} · {name}’", w.name)))
                };
                ui.add(egui::Label::new(RichText::new(text).font(font(ty::BODY)).color(c::TEXT)).wrap());
                ui.add(
                    egui::Label::new(
                        RichText::new("이 컴퓨터의 기존 공간과 실행은 그대로예요. 기존 프로젝트의 실행 위치도 바꾸지 않았어요.")
                            .font(font(12.5))
                            .color(c::TEXT_2),
                    )
                    .wrap(),
                );
                if let Some(list) = &tools {
                    let left: Vec<&str> = list
                        .iter()
                        .filter(|t| t.tool.is_agent() && t.installed && t.auth == AuthState::NeedsLogin)
                        .map(|t| t.tool.label())
                        .collect();
                    if !left.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} 로그인이 남아 있어요. {name}에서 로그인하면 바로 쓸 수 있어요.", left.join(", ")))
                                    .font(font(12.5))
                                    .color(c::TEXT_2),
                            )
                            .wrap(),
                        );
                    }
                }
                ui.horizontal(|ui| {
                    open = mark("공간 열기", theme::primary_button(ui, "공간 열기")).clicked();
                    if !existing {
                        again = theme::secondary_button(ui, "다른 기기 추가").clicked();
                    }
                });
            });
            ui.add_space(24.0);
            preview(ui, side, &project, &others, &w.name, &name);
        });
    });
    if open {
        let key = WsKey { host, id: w.remote.as_ref().map(|r| r.remote_workspace_id).unwrap_or(w.id) };
        app.feat.devices.add = None;
        crate::shell::actions::select_workspace(app, w.project_id, key);
        app.ui.page = Page::Work;
    } else if again {
        app.feat.devices.add = Some(Flow::new());
    }
}

fn preview(ui: &mut egui::Ui, w: f32, project: &str, others: &[String], ws: &str, device: &str) {
    egui::Frame::new()
        .fill(c::NAV)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(6, 8))
        .show(ui, |ui| {
            ui.set_width(w);
            ui.vertical(|ui| preview_rows(ui, project, others, ws, device));
        });
}

fn preview_rows(ui: &mut egui::Ui, project: &str, others: &[String], ws: &str, device: &str) {
    {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.horizontal(|ui| {
            ui.label(theme::icon(crate::icons::CHEVRON_DOWN, 11.0, c::TEXT_3));
            theme::truncated(ui, project, medium(12.5), c::TEXT_STRONG);
        });
        for o in others {
            egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 8, top: 4, bottom: 4 }).show(ui, |ui| {
                theme::truncated(ui, o, font(12.5), c::TEXT_2);
            });
        }
        egui::Frame::new()
            .fill(c::CONTROL)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin { left: 20, right: 8, top: 4, bottom: 4 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ws).font(font(12.5)).color(c::TEXT_STRONG));
                    egui::Frame::new()
                        .fill(c::ROW)
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(egui::Margin::symmetric(5, 1))
                        .show(ui, |ui| {
                            ui.label(RichText::new(device).font(font(10.5)).color(c::TEXT));
                        });
                });
            });
    }
}
