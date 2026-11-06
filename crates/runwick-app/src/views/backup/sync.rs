//! GitHub sync section: connection, status (last success, pending changes, last error), the
//! manual and automatic sync, the last report with conflicts, projects whose folder is not on
//! this computer, the data repository's history and disconnecting.

use super::{include_exclude, section_title, status_line};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::data::ago;
use crate::theme::{self, Seg, Tone, c, font, icon, medium, mono, semibold, ty};
use crate::views::history::fmt;
use egui::{CornerRadius, RichText};
use runwick_core::api::projects::{ProjectUpdate, ProjectsUpdate};
use runwick_core::api::sync::*;

const HOST: HostKey = HostKey::Local;

#[derive(Default)]
pub struct State {
    /// "지금 동기화" was pressed and has not answered yet.
    running: bool,
    history: bool,
    confirm_disconnect: bool,
}

/// What travels, in one line for the include box.
fn included() -> String {
    SyncKind::ALL.iter().map(|k| k.label()).collect::<Vec<_>>().join(" · ")
}

fn excluded() -> String {
    NOT_SYNCED.join(" · ")
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    section_title(
        ui,
        "GitHub 동기화",
        Some(
            "개인 GitHub 비공개 저장소 하나에 기록을 올려 두고, 다른 컴퓨터에서 같은 저장소를 연결하면 이어서 써요. 토큰은 저장하지 않고 이 컴퓨터의 git 인증을 그대로 써요.",
        ),
    );
    let q = app.hub.query::<SyncStatusGet>(HOST, &());
    match (q.data, q.error) {
        (Some(s), _) => {
            theme::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 8.0;
                match &s.repo {
                    None => not_connected(app, ui),
                    Some(repo) => connected(app, ui, &s, repo),
                }
            });
            if s.repo.is_some() {
                super::review::show(app, ui);
                ui.add_space(6.0);
                unlinked(app, ui);
                if app.feat.backup.sync.history {
                    ui.add_space(6.0);
                    history(app, ui);
                }
            }
        }
        (None, Some(e)) => status_line(ui, Tone::Recover, "확인 불가", &e.message),
        (None, None) => status_line(ui, Tone::Waiting, "확인 중", "동기화 상태를 읽고 있어요"),
    }
    ui.add_space(4.0);
    include_exclude(ui, &included(), &excluded());
}

fn not_connected(app: &mut App, ui: &mut egui::Ui) {
    ui.label(RichText::new("아직 연결된 저장소가 없어요").font(medium(ty::BODY)).color(c::TEXT_STRONG));
    let gh = app.hub.query::<SyncGhStatus>(HOST, &());
    match (gh.data.as_deref(), gh.error) {
        (Some(g), _) if g.logged_in => status_line(
            ui,
            Tone::Running,
            "GitHub CLI 로그인됨",
            &format!(
                "{} 계정에서 새 비공개 저장소를 만들거나 내 저장소에서 고를 수 있어요",
                g.account.as_deref().unwrap_or("이")
            ),
        ),
        (Some(g), _) => status_line(ui, Tone::Waiting, "GitHub CLI 사용 불가", g.problem.as_deref().unwrap_or("")),
        (None, Some(e)) => status_line(ui, Tone::Waiting, "확인 불가", &e.message),
        (None, None) => status_line(ui, Tone::Waiting, "확인 중", "GitHub CLI를 찾고 있어요"),
    }
    ui.horizontal(|ui| {
        let r = theme::primary_button(ui, "저장소 연결…");
        crate::terminal::verify::target("backup:connect", r.rect);
        if r.clicked() {
            app.feat.backup.connect = Some(super::connect::Wizard::new(false));
        }
        ui.label(
            RichText::new("이미 다른 컴퓨터에서 쓰던 저장소를 고르면 그 기록을 가져와요")
                .font(font(12.0))
                .color(c::TEXT_3),
        );
    });
}

fn connected(app: &mut App, ui: &mut egui::Ui, s: &SyncStatus, repo: &RepoRef) {
    ui.horizontal(|ui| {
        ui.label(icon(icons::FOLDER_GIT_2, 16.0, c::TEXT_2));
        ui.label(RichText::new(&repo.display).font(semibold(ty::BODY)).color(c::TEXT_STRONG));
        fmt::device_badge(ui, &repo.branch, false).on_hover_text("동기화 브랜치");
    });
    theme::truncated(ui, &repo.url, mono(11.5), c::TEXT_3);
    let (tone, chip, text) = describe(s);
    status_line(ui, tone, chip, &text);
    if let (Some(a), Some(b)) = (s.ahead, s.behind)
        && (a > 0 || b > 0)
    {
        ui.label(
            RichText::new(format!("올리지 못한 커밋 {a}개 · 아직 받지 않은 커밋 {b}개"))
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
        );
    }
    ui.horizontal(|ui| {
        let busy = s.running.is_some() || app.feat.backup.sync.running;
        let label = if busy { "동기화 중…" } else { "지금 동기화" };
        let r = ui.add_enabled_ui(!busy, |ui| theme::primary_button(ui, label)).inner;
        if r.clicked() {
            run_now(app);
        }
        let h = if app.feat.backup.sync.history { "기록 닫기" } else { "기록 보기" };
        if theme::secondary_button(ui, h).on_hover_text("데이터 저장소의 동기화 기록").clicked() {
            app.feat.backup.sync.history = !app.feat.backup.sync.history;
        }
        if ui.add_enabled_ui(!busy, |ui| theme::danger_button(ui, "연결 끊기")).inner.clicked() {
            app.feat.backup.sync.confirm_disconnect = true;
        }
    });
    auto(app, ui, s.auto);
    if let Some(r) = &s.last_report {
        report(ui, r);
    }
}

fn describe(s: &SyncStatus) -> (Tone, &'static str, String) {
    if let Some(step) = s.running {
        return (Tone::Info, "동기화 중", format!("{}…", step.label()));
    }
    if let Some(e) = &s.last_error {
        let at = s.last_success.map(|t| format!(" · 마지막 성공 {}", ago(t))).unwrap_or_default();
        let step = e.step.map(|st| format!("{} 단계에서 멈췄어요. ", st.label())).unwrap_or_default();
        return (Tone::Recover, e.kind.label(), format!("{step}{}{at}", e.message));
    }
    match (s.last_success, s.pending) {
        (None, _) => (Tone::Waiting, "아직 동기화 안 함", "지금 동기화를 누르면 첫 동기화를 해요".into()),
        (Some(t), Some(true)) => (Tone::Check, "올리지 않은 변경 있음", format!("마지막 동기화 {}", ago(t))),
        (Some(t), _) => (Tone::Running, "동기화됨", format!("{} · {}", ago(t), s.device_name)),
    }
}

fn run_now(app: &mut App) {
    app.feat.backup.sync.running = true;
    app.hub.call::<SyncRun>(HOST, (), |app, res| {
        app.feat.backup.sync.running = false;
        match res {
            Ok(r) => app.toast(crate::state::ToastKind::Success, summary(&r)),
            Err(e) => app.toast_error(&e.message),
        }
    });
}

fn counts_text(list: &[KindCount]) -> String {
    list.iter().map(|k| format!("{} {}", k.kind.label(), super::group(k.count))).collect::<Vec<_>>().join(" · ")
}

/// One line for a finished sync.
pub fn summary(r: &SyncReport) -> String {
    let mut parts = Vec::new();
    if !r.imported.is_empty() {
        parts.push(format!("가져옴 {}", counts_text(&r.imported)));
    }
    if !r.exported.is_empty() {
        parts.push(format!("올림 {}", counts_text(&r.exported)));
    }
    if parts.is_empty() {
        "동기화했어요 · 바뀐 기록 없음".into()
    } else {
        format!("동기화했어요 · {}", parts.join(" / "))
    }
}

fn auto(app: &mut App, ui: &mut egui::Ui, a: AutoSync) {
    crate::views::settings::general::divider(ui);
    ui.horizontal(|ui| {
        let mut on = a.enabled;
        ui.label(RichText::new("변경이 생기면 자동으로 동기화").font(font(ty::BODY)).color(c::TEXT));
        if crate::views::settings::general::toggle(ui, &mut on, "자동 동기화") {
            set_auto(app, AutoSync { enabled: on, ..a });
        }
    });
    if a.enabled {
        ui.horizontal(|ui| {
            ui.label(RichText::new("최대").font(font(12.5)).color(c::TEXT_2));
            let labels: Vec<String> = AUTO_INTERVALS
                .iter()
                .map(|m| if *m >= 60 { format!("{}시간", m / 60) } else { format!("{m}분") })
                .collect();
            let segs: Vec<Seg> = labels.iter().map(|l| Seg::text(l)).collect();
            let cur = AUTO_INTERVALS.iter().position(|m| *m == a.interval_min).unwrap_or(1);
            if let Some(i) = theme::segmented(ui, &segs, cur, 26.0) {
                set_auto(app, AutoSync { interval_min: AUTO_INTERVALS[i], ..a });
            }
            ui.label(RichText::new("마다").font(font(12.5)).color(c::TEXT_2));
        });
    }
    ui.add(
        egui::Label::new(
            RichText::new("켜 두면 변경을 정해진 간격보다 자주 올리지 않고, 가끔 다른 컴퓨터의 변경도 받아요. Runwick을 끌 때 남은 변경도 올려요.")
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
        )
        .wrap(),
    );
}

fn set_auto(app: &mut App, a: AutoSync) {
    app.hub.call::<SyncSetAuto>(HOST, a, |app, res| {
        if let Err(e) = res {
            app.toast_error(&e.message);
        }
    });
}

fn report(ui: &mut egui::Ui, r: &SyncReport) {
    crate::views::settings::general::divider(ui);
    ui.label(
        RichText::new(format!("마지막 동기화 결과 · {}", fmt::when_full(r.finished_at)))
            .font(medium(12.5))
            .color(c::TEXT_2),
    );
    let line = |ui: &mut egui::Ui, head: &str, list: &[KindCount]| {
        let text = if list.is_empty() { "없음".to_string() } else { counts_text(list) };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(head).font(medium(12.0)).color(c::TEXT_3));
            ui.label(RichText::new(text).font(font(12.5)).color(c::TEXT));
        });
    };
    line(ui, "가져옴", &r.imported);
    line(ui, "올림", &r.exported);
    if r.attempts > 1 {
        ui.label(
            RichText::new(format!("다른 컴퓨터가 같은 때 올려서 {}번 만에 마쳤어요", r.attempts))
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
        );
    }
    for cf in &r.conflicts {
        ui.horizontal_wrapped(|ui| {
            theme::chip(ui, Tone::Check, cf.kind.label());
            ui.label(RichText::new(&cf.title).font(medium(12.5)).color(c::TEXT_STRONG));
            ui.label(RichText::new(&cf.resolution).font(font(12.5)).color(c::TEXT_2));
        });
    }
    if !r.skipped.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(format!("읽지 못해 그대로 둔 파일 {}개", r.skipped.len())).font(font(12.5)).color(c::YELLOW),
        )
        .id_salt("sync-skipped")
        .show(ui, |ui| {
            for s in &r.skipped {
                theme::truncated(ui, s, mono(11.5), c::TEXT_2);
            }
        });
    }
}

/// Projects synced from another computer without a folder here.
fn unlinked(app: &mut App, ui: &mut egui::Ui) {
    let q = app.hub.query::<SyncUnlinked>(HOST, &());
    let Some(list) = q.data.filter(|l| !l.is_empty()) else {
        if let Some(e) = q.error {
            status_line(ui, Tone::Recover, "확인 불가", &e.message);
        }
        return;
    };
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(icon(icons::UNLINK, 14.0, c::YELLOW));
            ui.label(RichText::new("이 컴퓨터에 연결된 폴더 없음").font(medium(ty::BODY)).color(c::TEXT_STRONG));
        });
        ui.label(
            RichText::new("기록은 그대로 읽을 수 있어요. 같은 저장소를 이 컴퓨터에서 열면 자동으로 연결돼요.")
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
        );
        for p in list.iter() {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 110.0).max(120.0));
                    theme::truncated(ui, &p.name, medium(ty::BODY), c::TEXT);
                    let where_ = if p.elsewhere.is_empty() {
                        p.remote_url.clone().unwrap_or_default()
                    } else {
                        p.elsewhere.join(" · ")
                    };
                    if !where_.is_empty() {
                        theme::truncated(ui, crate::shell::data::tilde(&where_), mono(11.5), c::TEXT_3);
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::secondary_button(ui, "폴더 연결").clicked() {
                        link(app, p.id, &p.name);
                    }
                });
            });
        }
    });
}

fn link(app: &mut App, id: runwick_core::api::projects::ProjectId, name: &str) {
    let Some(dir) = rfd::FileDialog::new().set_title(format!("{name} 폴더 연결")).pick_folder() else {
        return;
    };
    let name = name.to_string();
    let p = ProjectUpdate { id, root: Some(dir.to_string_lossy().to_string()), ..Default::default() };
    app.hub.call::<ProjectsUpdate>(HOST, p, move |app, res| match res {
        Ok(_) => app.toast(crate::state::ToastKind::Success, format!("{name}에 폴더를 연결했어요")),
        Err(e) => app.toast_error(&e.message),
    });
}

fn history(app: &mut App, ui: &mut egui::Ui) {
    let q = app.hub.query::<SyncHistory>(HOST, &());
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("동기화 기록").font(medium(ty::BODY)).color(c::TEXT_STRONG));
        match (q.data.as_deref().map(Vec::as_slice), q.error) {
            (Some([]), _) => {
                ui.label(RichText::new("아직 기록이 없어요").font(font(12.5)).color(c::TEXT_3));
            }
            (Some(list), _) => {
                for cm in list {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(fmt::when_full(cm.at)).font(font(12.0)).color(c::TEXT_3));
                        fmt::device_badge(ui, &cm.device, false);
                        let rest = cm.summary.splitn(3, " · ").nth(2).unwrap_or(&cm.summary).to_string();
                        theme::truncated(ui, rest, font(12.5), c::TEXT);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(&cm.id).font(mono(11.0)).color(c::TEXT_OFF));
                        });
                    });
                }
            }
            (None, Some(e)) => status_line(ui, Tone::Recover, "확인 불가", &e.message),
            (None, None) => status_line(ui, Tone::Waiting, "확인 중", "기록을 읽고 있어요"),
        }
    });
}

/// Disconnect confirmation.
pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    if !app.feat.backup.sync.confirm_disconnect {
        return;
    }
    let (choice, close) = crate::shell::dialog(ctx, "sync-disconnect", 440.0, |ui| {
        crate::shell::dialog_title(
            ui,
            "동기화 연결을 끊을까요?",
            Some("이 컴퓨터의 기록과 GitHub 저장소는 그대로 남아요. 이 컴퓨터만 더 이상 올리고 받지 않아요."),
        );
        egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("올리지 않은 변경이 있으면 저장소에는 없지만, 이 컴퓨터 기록에는 그대로 있어요. 나중에 다시 연결하면 합쳐요.")
                    .font(font(12.5))
                    .color(c::TEXT_2),
            );
        });
        ui.add_space(6.0);
        let mut choice = None;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::danger_button(ui, "연결 끊기").clicked() {
                choice = Some(true);
            }
            if theme::secondary_button(ui, "취소").clicked() {
                choice = Some(false);
            }
        });
        choice
    });
    if close || choice == Some(false) {
        app.feat.backup.sync.confirm_disconnect = false;
    }
    if choice == Some(true) {
        app.feat.backup.sync.confirm_disconnect = false;
        app.hub.call::<SyncDisconnect>(HOST, (), |app, res| match res {
            Ok(()) => app.toast_info("동기화 연결을 끊었어요. 기록과 저장소는 그대로예요"),
            Err(e) => app.toast_error(&e.message),
        });
    }
}
