//! Devices in the workroom (design rows 8–9): adding a device (DeviceAdd), a device's
//! workspace as seen from here (RemoteSpace panel), and what a remote terminal shows while its
//! device is unreachable or another screen holds the input (RemoteLost, RemoteInput).
//! Connections themselves live in `crate::remote`; Settings › 기기 is `views::settings::devices`.

pub mod add;
mod banner;
mod space;

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::state::{Modal, Page, RightPanel, SettingsSection, WsKey};
use crate::theme::{self, c, font, icon, medium, ty};
use egui::{CornerRadius, RichText};
use runwick_core::api::sessions::SessionId;
use serde_json::Value;
use std::sync::mpsc::{Receiver, Sender, channel};

type Job = Box<dyn FnOnce(&mut App) + Send>;

pub struct State {
    pub remote: crate::remote::State,
    /// The add-device flow while it is open (shown in Settings › 기기).
    pub add: Option<add::Flow>,
    pub space: space::State,
    jobs: (Sender<Job>, Receiver<Job>),
}

impl Default for State {
    fn default() -> Self {
        Self { remote: Default::default(), add: None, space: Default::default(), jobs: channel() }
    }
}

/// Run blocking work (ssh, scp, tailscale) off the UI thread; `done` runs on the UI thread the
/// next time a devices screen is drawn.
pub fn background<T: Send + 'static>(
    app: &App,
    ctx: &egui::Context,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut App, T) + Send + 'static,
) {
    let tx = app.feat.devices.jobs.0.clone();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("runwick-device-check".into())
        .spawn(move || {
            let out = work();
            let _ = tx.send(Box::new(move |app: &mut App| done(app, out)));
            ctx.request_repaint();
        })
        .ok();
}

/// Apply finished background work.
pub fn pump(app: &mut App) {
    let jobs: Vec<Job> = app.feat.devices.jobs.1.try_iter().collect();
    for j in jobs {
        j(app);
    }
}

pub fn on_event(app: &mut App, host: HostKey, topic: &str, data: &Value) {
    crate::remote::on_event(app, host, topic, data);
}

/// `Modal::AddDevice` opens the add flow inside Settings › 기기 (as in the design).
pub fn show_add_modal(app: &mut App, _ctx: &egui::Context) -> bool {
    start_add(app);
    false
}

pub fn start_add(app: &mut App) {
    if app.feat.devices.add.is_none() {
        app.feat.devices.add = Some(add::Flow::new());
    }
    app.ui.page = Page::Settings;
    app.ui.settings_section = SettingsSection::Devices;
    if app.ui.modal == Some(Modal::AddDevice) {
        app.ui.modal = None;
    }
}

/// Pick another folder of an already registered device.
pub fn start_add_folder(app: &mut App, device: runwick_core::api::devices::Device) {
    app.feat.devices.add = Some(add::Flow::for_device(device));
    app.ui.page = Page::Settings;
    app.ui.settings_section = SettingsSection::Devices;
}

/// Open the `이 공간` panel for a workspace of a device.
pub fn open_space_panel(app: &mut App, ws: WsKey) {
    app.ui.right = Some(RightPanel::Space { ws });
}

pub fn show_space_panel(app: &mut App, ui: &mut egui::Ui, ws: WsKey) {
    space::show(app, ui, ws);
}

/// A terminal tab of a device's session: connection states above the terminal, and input typed
/// while the device is unreachable kept as "보내지 않은 입력" instead of being lost or sent later
/// by itself.
pub fn show_terminal_tab(
    app: &mut App,
    ui: &mut egui::Ui,
    host: HostKey,
    session: SessionId,
    tab: crate::state::TabId,
) {
    let HostKey::Device(_) = host else {
        crate::terminal::show_tab(app, ui, host, session, tab);
        return;
    };
    let area = ui.max_rect();
    let mut head = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area)
            .id_salt(("remote-banner", session))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let live = banner::show(app, &mut head, host, session);
    let rest = egui::Rect::from_min_max(egui::pos2(area.left(), area.top() + head.min_rect().height()), area.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rest).id_salt(("remote-term", session)));
    if head.min_rect().height() > 0.5 {
        crate::terminal::show_tab_below_banner(app, &mut child, host, session);
    } else {
        crate::terminal::show_tab(app, &mut child, host, session, tab);
    }
    // After the terminal took its focus for this frame (so the first key after a click counts).
    banner::capture_unsent(app, ui.ctx(), host, session);
    if !live {
        // The last screen stays readable but is clearly not current.
        ui.painter().rect_filled(rest, 0.0, egui::Color32::from_black_alpha(110));
    }
}

/// How a check stands, as a colored icon with words next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Ok,
    Bad,
    Ask,
    Wait,
    Busy,
    /// A CLI that is not installed there (see `data::NOT_INSTALLED`).
    Missing,
}

impl Mark {
    fn glyph(self) -> &'static str {
        match self {
            Mark::Ok => icons::CHECK,
            Mark::Bad => icons::TRIANGLE_ALERT,
            Mark::Ask => icons::CIRCLE_HELP,
            Mark::Wait => icons::CIRCLE_PAUSE,
            Mark::Busy => icons::LOADER_CIRCLE,
            Mark::Missing => crate::shell::data::NOT_INSTALLED_ICON,
        }
    }
    fn color(self) -> egui::Color32 {
        match self {
            Mark::Ok => c::MINT,
            Mark::Bad => c::ROSE,
            Mark::Ask | Mark::Missing => c::YELLOW,
            Mark::Wait | Mark::Busy => c::GREY,
        }
    }
}

/// One check line: icon, title and explanation, then actions on the right.
pub fn check_row(ui: &mut egui::Ui, mark: Mark, title: &str, msg: &str, actions: impl FnOnce(&mut egui::Ui)) {
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, c::LINE));
    egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 11)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.add_space(0.0);
            ui.label(icon(mark.glyph(), 15.0, mark.color()));
            if mark == Mark::Busy {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(400));
            }
            let w = ui.available_width();
            ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                actions(ui);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(egui::Label::new(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG)).wrap());
                    if !msg.is_empty() {
                        ui.add(egui::Label::new(RichText::new(msg).font(font(12.5)).color(c::TEXT_2)).wrap());
                    }
                });
            });
        });
    });
}

/// Small grey action button used in check rows.
pub fn small_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_STRONG))
        .fill(c::CONTROL)
        .corner_radius(CornerRadius::same(6))
        .min_size(egui::vec2(0.0, 28.0));
    mark(text, ui.add(b))
}

/// Let verification scripts click this by label (`click:item:<label>`); no effect otherwise.
pub fn mark(label: &str, r: egui::Response) -> egui::Response {
    crate::terminal::verify::target(label, r.rect);
    r
}

/// A rounded card on the document ground (the design's `#17181C` sections).
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui);
    });
}

/// Section title and explanation at the top of a card.
pub fn card_head(ui: &mut egui::Ui, title: &str, desc: &str) {
    egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 8 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(title).font(theme::semibold(14.0)).color(c::TEXT_STRONG));
        if !desc.is_empty() {
            ui.add(egui::Label::new(RichText::new(desc).font(font(12.5)).color(c::TEXT_3)).wrap());
        }
    });
}
