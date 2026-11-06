//! Settings › 협업: what a CLI may do through `runwick delegate` — whether agent CLIs are told
//! about the command when they start, the automatic assignment limit (with the observed numbers
//! next to it, never mixed), and which tools a delegation may start. Changes apply to new
//! assignments and new runs; work already handed out is not changed.

use super::general::{divider, group, header, toggle};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::data;
use crate::theme::{self, Tone, c, font, icon, medium, mono, ty};
use egui::RichText;
use runwick_core::api::activity;
use runwick_core::api::collab::{CollabOverview, CollabSettings, CollabSettingsGet, CollabSettingsSet};
use runwick_core::api::sessions::Tool;

/// Highest automatic assignment limit the host accepts.
const MAX_LIMIT: u32 = 16;
const HOST: HostKey = HostKey::Local;

#[derive(Default)]
pub struct State {
    /// Values shown while a save is on its way (so controls do not jump back).
    draft: Option<CollabSettings>,
}

/// The agent commands, as an agent CLI is told about them.
const COMMANDS: &[(&str, &str)] = &[
    (
        "runwick delegate --tool codex \"…\"",
        "다른 CLI에 일을 맡기고 작업자 ID를 받아요 (--split <branch>면 분리한 공간에서)",
    ),
    ("runwick wait <id>", "작업자가 이번 차례를 마칠 때까지 기다리고 답을 받아요"),
    ("runwick send <id> \"…\"", "후속 지시. 작업자가 응답 중이면 대기열에 들어가요"),
    ("runwick report --summary \"…\"", "맡은 일의 결과를 남겨요 (--file, --check, --not-done)"),
    ("runwick workers · result · status", "맡긴 일 목록, 결과, 자동 배정 상태"),
];

fn save(app: &mut App, s: CollabSettings) {
    app.feat.settings.collab.draft = Some(s.clone());
    app.hub.call::<CollabSettingsSet>(HOST, s, |app, res| {
        app.feat.settings.collab.draft = None;
        if let Err(e) = res {
            app.toast_error(&format!("협업 설정을 저장하지 못했어요: {}", e.message));
        }
    });
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(
        ui,
        "협업",
        "CLI가 runwick 명령으로 다른 CLI에게 일을 맡길 때의 범위예요. 바꾼 값은 새로 맡기는 일부터 적용되고, 이미 맡긴 일은 그대로 둡니다.",
    );
    let q = app.hub.query::<CollabSettingsGet>(HOST, &());
    let saved = match (&app.feat.settings.collab.draft, &q.data) {
        (Some(d), _) => d.clone(),
        (None, Some(s)) => s.as_ref().clone(),
        (None, None) => {
            let text = match &q.error {
                Some(e) => format!("협업 설정을 불러오지 못했어요: {}", e.message),
                None => "협업 설정을 불러오는 중…".into(),
            };
            ui.label(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3));
            return;
        }
    };
    let mut s = saved.clone();
    let overview = app.hub.query::<CollabOverview>(HOST, &()).data;
    let tools = data::tool_lines(app, HOST, false);

    group(ui, "협업 안내", |ui| {
        row(
            ui,
            "CLI에 협업 명령 알려 주기",
            "Claude Code에게 runwick delegate·wait·report 사용법을 짧게 알려 줘요. 끄면 CLI가 스스로 위임하지 않지만, 명령은 계속 쓸 수 있어요.",
            |ui| {
                toggle(ui, &mut s.guidance, "CLI에 협업 명령 알려 주기");
            },
        );
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Claude Code는 시작할 때 --append-system-prompt로 받아요. 이미 실행 중인 Claude Code에는 다음 시작이나 세션 새로고침부터 적용돼요. Codex에는 전달하지 않아요: Codex의 기본 샌드박스가 Runwick 연결을 막아 runwick 명령을 쓸 수 없고, 설정 옵션(-c)을 주면 Codex의 공유 백그라운드 서버가 꺼져요. Codex 작업자의 결과는 대화 기록에 남은 마지막 답에서 가져와요.",
                )
                .font(font(12.0))
                .color(c::TEXT_3),
            )
            .wrap(),
        );
        divider(ui);
        egui::Grid::new("collab-commands").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
            for (cmd, what) in COMMANDS {
                ui.label(RichText::new(*cmd).font(mono(12.0)).color(c::TEXT_STRONG));
                ui.add(egui::Label::new(RichText::new(*what).font(font(12.5)).color(c::TEXT_2)).wrap());
                ui.end_row();
            }
        });
    });

    group(ui, "자동 배정 한도", |ui| {
        row(
            ui,
            "동시에 진행할 자동 배정",
            "AI가 맡긴 일이 이 수만큼 진행 중이면 새로 맡긴 일은 순서를 기다려요. 응답 중인 다른 CLI도 함께 셉니다. 열린 CLI의 입력은 막지 않고, 내가 직접 요청한 일도 기다리지 않아요.",
            |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let up = ui
                    .add_enabled_ui(s.auto_limit < MAX_LIMIT, |ui| theme::icon_button(ui, icons::PLUS, "한도 늘리기"))
                    .inner;
                if up.clicked() {
                    s.auto_limit += 1;
                }
                ui.label(RichText::new(format!("{}개", s.auto_limit)).font(mono(12.5)).color(c::TEXT_STRONG));
                let down =
                    ui.add_enabled_ui(s.auto_limit > 1, |ui| theme::icon_button(ui, icons::MINUS, "한도 줄이기")).inner;
                if down.clicked() {
                    s.auto_limit -= 1;
                }
            },
        );
        divider(ui);
        ui.horizontal(|ui| {
            let (tone, text) = match &overview {
                Some(o) => {
                    let tone = if o.in_use >= o.settings.auto_limit { Tone::Waiting } else { Tone::Running };
                    (tone, format!("지금: {}", activity::slots_line(o)))
                }
                None => (Tone::Waiting, "지금 상태 확인 중…".to_string()),
            };
            ui.label(icon(tone.icon(), 13.0, tone.fg()));
            ui.label(RichText::new(text).font(font(12.5)).color(c::TEXT));
        });
        ui.label(
            RichText::new("CLI가 스스로 띄우는 내장 subagent와 셸에서 실행한 명령은 관측할 수 없어 집계에서 빠져요.")
                .font(font(12.0))
                .color(c::TEXT_3),
        );
    });

    group(ui, "맡길 수 있는 도구", |ui| {
        ui.add(
            egui::Label::new(
                RichText::new("허용하지 않은 도구로 맡기면 시작하지 않고 그 이유를 맡긴 CLI에 돌려줘요. 셸에는 일을 맡길 수 없어요.")
                    .font(font(12.0))
                    .color(c::TEXT_3),
            )
            .wrap(),
        );
        for (i, tool) in [Tool::Claude, Tool::Codex].into_iter().enumerate() {
            if i > 0 {
                divider(ui);
            }
            let line = tools.iter().find(|(t, ..)| *t == tool).map(|(_, l, _)| l);
            let mut on = s.allowed_tools.contains(&tool);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 120.0).max(160.0));
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(tool.label()).font(medium(ty::BODY)).color(c::TEXT_STRONG));
                    if let Some(l) = line {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(icon(l.tone.icon(), 12.0, l.tone.fg()));
                            ui.label(RichText::new(&l.text).font(font(12.0)).color(c::TEXT_3));
                        });
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if toggle(ui, &mut on, &format!("{} 허용", tool.label())) {
                        if on {
                            s.allowed_tools.push(tool);
                        } else {
                            s.allowed_tools.retain(|t| *t != tool);
                        }
                    }
                });
            });
        }
        if s.allowed_tools.is_empty() {
            ui.horizontal(|ui| {
                ui.label(icon(Tone::Attention.icon(), 13.0, Tone::Attention.fg()));
                ui.label(
                    RichText::new("허용한 도구가 없어 새 위임은 모두 거절돼요")
                        .font(font(12.5))
                        .color(Tone::Attention.fg()),
                );
            });
        }
    });

    if s != saved {
        save(app, s);
    }
}

/// A setting: title and explanation on the left, the control on the right.
fn row(ui: &mut egui::Ui, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 200.0).max(160.0));
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
            ui.add(egui::Label::new(RichText::new(desc).font(font(12.0)).color(c::TEXT_3)).wrap());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}
