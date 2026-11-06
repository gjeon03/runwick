//! The full-width notice a window shows when this computer's host did not accept it as a
//! Runwick window (`host.window_status`): why, in the person's words, and what to do. Instead of
//! every list failing with "Runwick 창에서만 할 수 있는 요청이에요".
//!
//! For another Runwick build (the app was moved or updated while the host kept running) the
//! window may ask the host to stop its runs and exit (`host.restart_for_update`) after the person
//! saw which runs stop and confirmed; the connection then starts a host from this build.

use crate::App;
use crate::client::HostKey;
use crate::theme::{self, Tone, c, font, medium, mono, semibold, ty};
use egui::{CornerRadius, RichText, Vec2};
use runwick_core::api::host::{HostRestartForUpdate, HostWindowStatus, WindowRefusal, WindowStatus};

/// Draw the notice in place of the page when the host refused this window. Returns whether it
/// did.
pub fn show(app: &mut App, ui: &mut egui::Ui) -> bool {
    if !app.hub.state(HostKey::Local).is_connected() {
        return false;
    }
    let Some(status) = app.hub.query::<HostWindowStatus>(HostKey::Local, &()).data.map(|s| (*s).clone()) else {
        return false;
    };
    let Some(reason) = status.refusal else {
        return false;
    };
    let version = app.hub.local_info().map(|i| i.version).unwrap_or_default();
    let mut act = Act::None;
    egui::CentralPanel::default().frame(egui::Frame::new().fill(c::WINDOW)).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("window-refusal").auto_shrink([false, false]).show(ui, |ui| {
            let width = (ui.available_width() - 64.0).clamp(280.0, 640.0);
            ui.add_space(72.0);
            ui.vertical_centered(|ui| {
                ui.set_max_width(width);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    act = body(ui, reason, &status, &version, confirm(ui.ctx()));
                });
            });
        });
    });
    match act {
        Act::None => {}
        Act::Ask => set_confirm(ui.ctx(), true),
        Act::Cancel => set_confirm(ui.ctx(), false),
        Act::Restart => {
            set_confirm(ui.ctx(), false);
            app.hub.call::<HostRestartForUpdate>(HostKey::Local, (), |app, res| match res {
                // The host exits; the window reconnects and starts it again from this build.
                Ok(_) => app.toast_info("실행 관리자를 멈췄어요. 이 Runwick으로 다시 시작해요"),
                Err(e) => app.toast_error(&format!("실행 관리자를 다시 시작하지 못했어요: {}", e.message)),
            });
        }
    }
    true
}

enum Act {
    None,
    Ask,
    Cancel,
    Restart,
}

fn confirm_id() -> egui::Id {
    egui::Id::unique("window-refusal-confirm")
}

fn confirm(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(confirm_id())).unwrap_or(false)
}

fn set_confirm(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(confirm_id(), on));
}

/// Title and explanation for a reason (Korean, for display).
pub fn words(reason: WindowRefusal, host_exe: Option<&str>, version: &str) -> (String, String) {
    match reason {
        WindowRefusal::DifferentBinary => (
            "실행 관리자가 다른 Runwick에서 돌고 있어요".into(),
            format!(
                "지금 실행 관리자는 {} 의 Runwick {version}이에요. 이 창의 Runwick과 같은 프로그램이 아니어서 안전을 위해 이 창에는 권한을 주지 않았어요. 앱을 옮기거나 업데이트했을 때 이렇게 돼요.",
                host_exe.unwrap_or("다른 위치")
            ),
        ),
        WindowRefusal::InsideRun => (
            "Runwick 터미널 안에서 연 창이라 권한이 없어요".into(),
            "Runwick이 관리하는 터미널(에이전트와 명령이 도는 곳)에서 시작한 창은 설정과 실행을 바꿀 수 없어요. Finder나 Dock에서 열거나, 일반 터미널에서 ‘open -a Runwick’으로 여세요.".into(),
        ),
        WindowRefusal::Hooks => (
            "검증용 설정으로 연 창이에요".into(),
            "화면 검증용 환경변수를 켠 채 시작한 창은 설정과 실행을 바꿀 수 없어요. 평소처럼 Finder나 Dock에서 Runwick을 여세요. (개발 중 검증 방법은 저장소의 AGENTS.md에 있어요.)".into(),
        ),
        WindowRefusal::NoToken => (
            "이 창을 확인하지 못했어요".into(),
            "실행 관리자가 이 창을 알아보지 못했어요. 같은 데이터 폴더를 쓰는 Runwick인지 확인하고 창을 다시 여세요.".into(),
        ),
        WindowRefusal::UnknownProcess => (
            "이 창을 확인하지 못했어요".into(),
            "이 창이 어떤 프로그램인지 확인하지 못했어요. Runwick을 다시 여세요.".into(),
        ),
        WindowRefusal::NoSsh => (
            "이 창을 확인하지 못했어요".into(),
            "SSH 로그인 없이 시작한 중계는 창으로 인정하지 않아요. 원격 기기는 SSH로 연결하세요.".into(),
        ),
    }
}

fn body(ui: &mut egui::Ui, reason: WindowRefusal, st: &WindowStatus, version: &str, confirming: bool) -> Act {
    let mut act = Act::None;
    let (title, text) = words(reason, st.host_exe.as_deref(), version);
    ui.spacing_mut().item_spacing.y = 8.0;
    theme::chip(ui, Tone::Attention, "이 창에는 권한이 없어요");
    ui.label(RichText::new(title).font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
    ui.add(egui::Label::new(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_2)).wrap());
    if !st.may_restart {
        return act;
    }
    ui.add_space(8.0);
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(14)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            let head = if st.running.is_empty() {
                "실행 중인 작업이 없어요. 바로 다시 시작해도 돼요.".to_string()
            } else {
                format!("다시 시작하면 멈추는 실행 {}개", st.running.len())
            };
            ui.label(RichText::new(head).font(medium(ty::BODY)).color(c::TEXT_STRONG));
            for r in &st.running {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&r.tool).font(mono(12.0)).color(c::TEXT_3));
                    theme::truncated(ui, &r.title, font(ty::BODY), c::TEXT);
                });
            }
            ui.add(
                egui::Label::new(
                    RichText::new("기록과 핀은 그대로예요. 멈춘 세션은 다시 연 뒤 같은 대화로 재개할 수 있어요.")
                        .font(font(12.0))
                        .color(c::TEXT_3),
                )
                .wrap(),
            );
        },
    );
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
        if confirming {
            if theme::danger_button(ui, "실행을 멈추고 다시 시작").clicked() {
                act = Act::Restart;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                act = Act::Cancel;
            }
        } else if theme::primary_button(ui, "실행 관리자를 새 Runwick으로 다시 시작").clicked() {
            act = Act::Ask;
        }
    });
    ui.add(
        egui::Label::new(
            RichText::new("또는 이전 Runwick을 열어 명령 팔레트(⌘K)의 ‘작업까지 종료’로 끈 뒤 이 창을 다시 여세요.")
                .font(font(12.0))
                .color(c::TEXT_3),
        )
        .wrap(),
    );
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_says_what_happened_and_what_to_do() {
        for r in [
            WindowRefusal::NoToken,
            WindowRefusal::UnknownProcess,
            WindowRefusal::InsideRun,
            WindowRefusal::DifferentBinary,
            WindowRefusal::Hooks,
            WindowRefusal::NoSsh,
        ] {
            let (title, text) = words(r, Some("~/Downloads/Runwick.app/Contents/MacOS/runwick"), "0.1.0");
            assert!(!title.is_empty() && text.len() > title.len(), "{r:?}");
        }
        let (_, text) = words(WindowRefusal::DifferentBinary, Some("~/Downloads/x/runwick"), "0.1.0");
        assert!(text.contains("~/Downloads/x/runwick") && text.contains("0.1.0"), "{text}");
        assert!(words(WindowRefusal::InsideRun, None, "").1.contains("open -a Runwick"));
    }
}
