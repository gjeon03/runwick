//! Above a device's terminal (RemoteLost, RemoteSpace): connection lost with the last confirmed
//! time and its cause, reconnected, disconnected on purpose, refused (version), and input typed
//! while unreachable. The terminal below keeps its last screen.

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::remote::{self, Status, unsent};
use crate::state::SettingsSection;
use crate::theme::{self, c, font, icon, medium, semibold, ty};
use egui::{CornerRadius, RichText};
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::{SessionId, SessionsGet};
use runwick_core::josa;

/// Keep what is typed into a device's terminal while it is unreachable (never sent by itself).
pub fn capture_unsent(app: &mut App, ctx: &egui::Context, host: HostKey, session: SessionId) {
    let HostKey::Device(id) = host else { return };
    if remote::status(app, id).is_connected() || app.ui.modal.is_some() || app.ui.palette_open {
        return;
    }
    if crate::terminal::focused_session(app) != Some((host, session)) {
        return;
    }
    let keys = unsent::typed(&ctx.input(|i| i.events.clone()));
    app.feat.devices.remote.unsent.capture((host, session), &keys);
}

/// Icon, its color, title, message, cause line and the action (label, primary, act).
type Head = (&'static str, egui::Color32, String, String, Option<String>, Option<(&'static str, bool, Act)>);

enum Act {
    Reconnect,
    DismissBack,
    Settings,
    Send,
    Drop,
    DismissUnsent,
    DismissStop,
}

/// Draw the banner when one applies. Returns whether the device is live (connected and used).
pub fn show(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId) -> bool {
    let HostKey::Device(id) = host else {
        return true;
    };
    let name = remote::device_name(app, id);
    let status = remote::status(app, id);
    let live = status.is_connected();
    let key = (host, session);
    let mut acts = Vec::new();
    let held = app.feat.devices.remote.unsent.get(&key).cloned();
    let record = app.hub.query::<SessionsGet>(host, &ById { id: session }).data;
    let tool = record.as_ref().map(|r| r.session.tool.label()).unwrap_or("CLI");

    let head: Option<Head> = match &status {
        Status::Connected(_) => {
            if let Some((title, msg)) = super::space::stop_note(app, id) {
                Some((icons::CIRCLE_STOP, c::GREY, title, msg, None, Some(("닫기", false, Act::DismissStop))))
            } else if let Some(b) = remote::back(app, id) {
                let alive = crate::terminal::live(app, host, session)
                    .or_else(|| record.as_ref().and_then(|r| r.live.clone()))
                    .is_some_and(|l| l.pid.is_some());
                let gap = if b.after.as_secs() < 60 {
                    "잠깐 끊긴".to_string()
                } else {
                    format!("끊긴 {}", remote::duration_words(b.after))
                };
                let msg = if alive {
                    format!("{gap} 동안에도 {name}에서 실행은 계속됐어요. 지금 화면을 다시 받아 왔어요.")
                } else {
                    format!("{gap} 사이 이 실행은 끝났어요. 기록은 {name}에 남아 있어요.")
                };
                // The held input must be decided before the note goes away.
                let pending = held.as_ref().is_some_and(|h| h.outcome.is_none());
                let act = (!pending).then_some(("닫기", false, Act::DismissBack));
                Some((icons::CHECK, c::MINT, "다시 연결됐어요".into(), msg, None, act))
            } else {
                None
            }
        }
        Status::Off => Some((
            icons::UNPLUG,
            c::GREY,
            "연결을 끊었어요".into(),
            format!("{name}의 실행은 계속돼요. 다시 연결하면 지금 상태부터 보여 드릴게요."),
            None,
            Some(("다시 연결", true, Act::Reconnect)),
        )),
        Status::Unreachable { lost, .. } | Status::Connecting { lost, failure: Some(_), .. } => {
            let cause = remote::explain(&status, &name)
                .map(|(c, e)| format!("원인 · {c} — {e} AI 작업이 실패한 것은 아니에요."));
            let (title, msg) = match lost {
                Some(l) => {
                    let mut m = format!("마지막 확인 {}", remote::clock(l.at_ts));
                    if let Some(n) = l.running.filter(|n| *n > 0) {
                        m.push_str(&format!(" · 그때 실행 {n}개가 돌고 있었어요"));
                    }
                    m.push_str(". 지금 상태는 확인할 수 없어요.");
                    (format!("{} 연결이 끊겼어요", josa::gwa_wa(&name)), m)
                }
                None => {
                    let seen = remote::last_seen(app, id)
                        .map(|t| format!("마지막 확인 {}. ", crate::shell::data::ago(t)))
                        .unwrap_or_default();
                    (format!("{name}에 연결하지 못했어요"), format!("{seen}다시 연결되면 지금 상태를 보여 드릴게요."))
                }
            };
            let connecting = matches!(status, Status::Connecting { .. });
            let act = if connecting {
                ("다시 연결하는 중…", false, Act::Reconnect)
            } else {
                ("다시 연결", true, Act::Reconnect)
            };
            Some((icons::WIFI_OFF, c::GREY, title, msg, cause, Some(act)))
        }
        Status::Connecting { .. } => Some((
            icons::LOADER_CIRCLE,
            c::GREY,
            format!("{name}에 연결하는 중…"),
            "실행은 그 기기에서 계속되고 있어요. 연결되면 지금 화면을 받아 와요.".into(),
            None,
            None,
        )),
        Status::Refused(remote::Refusal::Version { .. }) => Some((
            icons::TRIANGLE_ALERT,
            c::YELLOW,
            "Runwick 버전이 달라 연결하지 않았어요".into(),
            remote::explain(&status, &name).map(|x| x.1).unwrap_or_default(),
            None,
            Some(("기기 설정 열기", false, Act::Settings)),
        )),
        Status::Refused(_) | Status::Setup(_) => Some((
            icons::TRIANGLE_ALERT,
            c::YELLOW,
            format!("{name}에 연결하지 않았어요"),
            remote::explain(&status, &name).map(|x| x.1).unwrap_or_default(),
            None,
            Some(("기기 설정 열기", false, Act::Settings)),
        )),
        Status::Unknown => None,
    };

    let show_unsent = held.is_some();
    if head.is_none() && !show_unsent {
        return live;
    }
    egui::Frame::new().fill(c::PANEL).inner_margin(egui::Margin { left: 16, right: 14, top: 11, bottom: 12 }).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            if let Some((glyph, color, title, msg, cause, act)) = head {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(icon(glyph, 15.0, color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if let Some((label, primary, a)) = act {
                            let enabled = !label.ends_with('…');
                            let r = ui.add_enabled_ui(enabled, |ui| {
                                if primary {
                                    super::mark(label, theme::primary_button(ui, label))
                                } else {
                                    super::small_button(ui, label)
                                }
                            });
                            if r.inner.clicked() {
                                acts.push(a);
                            }
                        }
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add(
                                egui::Label::new(RichText::new(title).font(semibold(ty::BODY)).color(c::TEXT_STRONG))
                                    .wrap(),
                            );
                            ui.add(egui::Label::new(RichText::new(msg).font(font(12.5)).color(c::TEXT_2)).wrap());
                            if let Some(cause) = cause {
                                ui.add(egui::Label::new(RichText::new(cause).font(font(12.5)).color(c::TEXT_2)).wrap());
                            }
                        });
                    });
                });
            }
            if let Some(h) = &held {
                unsent_box(ui, h, live, tool, &mut acts, app, host, session);
            }
        },
    );
    ui.painter().hline(ui.max_rect().x_range(), ui.min_rect().bottom(), egui::Stroke::new(1.0, c::LINE));

    for a in acts {
        match a {
            Act::Reconnect => remote::reconnect(app, id),
            Act::DismissBack => remote::dismiss_back(app, id),
            Act::Settings => {
                app.feat.settings.devices.select(id);
                crate::shell::actions::open_settings(app, Some(SettingsSection::Devices));
            }
            Act::Send => send_unsent(app, host, session),
            Act::Drop => app.feat.devices.remote.unsent.discard(&key),
            Act::DismissUnsent => app.feat.devices.remote.unsent.dismiss(&key),
            Act::DismissStop => super::space::dismiss_stop(app, id),
        }
    }
    live
}

#[allow(clippy::too_many_arguments)]
fn unsent_box(
    ui: &mut egui::Ui,
    h: &unsent::Held,
    live: bool,
    tool: &str,
    acts: &mut Vec<Act>,
    app: &App,
    host: HostKey,
    session: SessionId,
) {
    let current = crate::terminal::live(app, host, session);
    let me = app.hub.state(host).info().map(|i| i.client_id);
    let held_elsewhere = current.as_ref().and_then(|l| l.input_owner_client).is_some_and(|o| Some(o) != me);
    let running = current.as_ref().is_some_and(|l| l.pid.is_some());
    let msg = match (h.outcome, live) {
        (Some(unsent::Outcome::Sent), _) => format!("보냈어요. {tool}에서 이어서 확인하세요."),
        (Some(unsent::Outcome::Dropped), _) => "버렸어요.".to_string(),
        (None, false) => {
            "연결이 끊겨 보내지 않았어요. 다시 연결되면 지금 상태를 확인한 뒤 보낼지 물어볼게요.".to_string()
        }
        (None, true) => {
            let ga = josa::particle(tool, josa::Particle::IGa);
            let now = match &current {
                Some(l) if l.attention.is_some() => format!("{tool}{ga} 확인을 기다리고 있어요."),
                Some(l) if l.busy => format!("{tool}{ga} 응답하는 중이에요."),
                Some(l) if l.pid.is_some() => format!("{tool}{ga} 입력을 기다리고 있어요."),
                _ => "이 실행은 지금 돌고 있지 않아요.".to_string(),
            };
            format!("끊긴 동안 쓴 입력이라 자동으로 보내지 않았어요. 지금 {now}")
        }
    };
    egui::Frame::new()
        .fill(c::ROW)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(12, 9))
        .outer_margin(egui::Margin { left: 25, ..Default::default() })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    match (h.outcome, live) {
                        (None, true) => {
                            if super::mark("버리기", theme::link_button(ui, "버리기")).clicked() {
                                acts.push(Act::Drop);
                            }
                            let ok = running && !held_elsewhere;
                            let tip = if held_elsewhere {
                                "다른 화면이 입력 중이에요. 먼저 입력을 가져오세요"
                            } else if !running {
                                "실행 중이 아니라 보낼 곳이 없어요"
                            } else {
                                "지금 상태를 확인한 뒤 보내세요"
                            };
                            let b =
                                ui.add_enabled_ui(ok, |ui| super::small_button(ui, &format!("{tool}에 보내기"))).inner;
                            if b.on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                                acts.push(Act::Send);
                            }
                        }
                        (Some(_), _) => {
                            if theme::link_button(ui, "닫기").clicked() {
                                acts.push(Act::DismissUnsent);
                            }
                        }
                        (None, false) => {}
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new("보내지 않은 입력").font(medium(12.5)).color(c::TEXT_STRONG));
                            ui.label(
                                RichText::new(format!("“{}”", unsent::preview(&h.text)))
                                    .font(theme::mono(12.5))
                                    .color(c::TEXT_STRONG),
                            );
                        });
                        ui.add(egui::Label::new(RichText::new(msg).font(font(ty::SMALL)).color(c::TEXT_2)).wrap());
                    });
                });
            });
        });
}

/// The person chose to send the held input: only now does it go to the session.
fn send_unsent(app: &mut App, host: HostKey, session: SessionId) {
    if !app.hub.state(host).is_connected() {
        app.toast_error("연결된 뒤에 보낼 수 있어요.");
        return;
    }
    let Some(bytes) = app.feat.devices.remote.unsent.take_for_send(&(host, session)) else {
        return;
    };
    let sent = app.hub.conn(host).is_some_and(|c| c.send_input(session, &bytes));
    if !sent {
        app.toast_error("보내지 못했어요. 연결이 방금 끊겼어요. 터미널에서 다시 입력하세요.");
    }
}
