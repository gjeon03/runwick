//! S11 결과 (Result.dc.html, Failed.dc.html): what a worker answered and changed, the checks it
//! reported (stale once files changed afterwards), what it did not do, and the next steps:
//! 통합 준비, 파일 열기, 검토 요청, 이어서 수정 / 처음부터 다시. `확인함` is separate from
//! verification. In the right panel it is compact; as a tab it also shows the selected diff.

use super::{Place, look, place, requests_of, short};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, data, widgets};
use crate::state::{RightPanel, TabKind};
use crate::theme::{self, c, font, icon, medium, mono, semibold, ty};
use egui::{Color32, CornerRadius, Margin, RichText, Sense};
use runwick_core::api::collab::*;
use runwick_core::api::projects::{ById, WorkspaceKind};
use runwick_core::api::sessions::{SessionInfo, Tool};
use uuid::Uuid;

enum Act {
    Select(String),
    Switch(Uuid),
    Ack(bool),
    Merge,
    OpenFile(String),
    Review,
    Retry(RetryMode),
    CopyPath,
    Peek,
    ToggleLog(usize),
}

pub fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut s: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        s.push('…');
    }
    s
}

struct Ctx {
    host: HostKey,
    view: RequestView,
    worker: Option<SessionInfo>,
    place: Place,
    in_other_ws: bool,
}

fn load(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid) -> Option<Ctx> {
    let q = app.hub.query::<CollabGet>(host, &ById { id: request });
    let Some(view) = q.data.as_deref().cloned() else {
        let text = match &q.error {
            Some(e) => format!("결과를 불러오지 못했어요: {}", e.message),
            None => "결과를 불러오는 중…".into(),
        };
        egui::Frame::new().inner_margin(Margin::same(14)).show(ui, |ui| widgets::empty_note(ui, &text));
        return None;
    };
    let worker = data::all_sessions(app, host)
        .data
        .and_then(|l| l.iter().find(|s| s.session.id == view.request.worker_id).cloned());
    let place = worker.as_ref().map(|w| place(app, host, &w.session)).unwrap_or_default();
    let in_other_ws = place.ws.is_some() && place.ws != app.ui.selected_ws;
    Some(Ctx { host, view, worker, place, in_other_ws })
}

pub fn show_panel(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid) {
    let Some(cx) = load(app, ui, host, request) else {
        return;
    };
    let mut acts = Vec::new();
    let siblings = cx
        .view
        .request
        .parent_id
        .and_then(|p| requests_of(app, host, p).data)
        .map(|l| {
            l.iter()
                .filter(|r| {
                    r.id == request || (!r.acknowledged && matches!(r.status, ReqStatus::Done | ReqStatus::Failed))
                })
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected = app.feat.collab.result_file.get(&request).cloned();
    let open_logs = app.feat.collab.open_logs.clone();
    // Measured outside the scroll area: its content may not push the panel wider.
    let panel_w = (ui.available_width() - 28.0).max(120.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::same(14)).show(ui, |ui| {
            let w = panel_w;
            // The panel is narrow: nothing inside may push it wider.
            ui.set_width(w);
            ui.set_max_width(w);
            ui.spacing_mut().item_spacing.y = 16.0;
            if siblings.len() > 1 {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let w = (ui.available_width() - 4.0 * (siblings.len() as f32 - 1.0)) / siblings.len() as f32;
                    for r in &siblings {
                        if switcher(ui, r, r.id == request, w) {
                            acts.push(Act::Switch(r.id));
                        }
                    }
                });
            }
            if cx.view.request.status == ReqStatus::Failed {
                failed_head(ui, &cx);
            }
            summary(ui, &cx);
            files(ui, &cx, selected.as_deref(), &mut acts);
            if let Some(path) = &selected {
                diff_view(app, ui, host, request, path, 260.0);
            }
            checks(ui, &cx, &open_logs, &mut acts);
            not_done(ui, &cx);
            if cx.in_other_ws {
                note(ui, &format!(
                    "다른 공간의 결과라 여기서는 읽기만 합니다. 파일을 열면 {} 공간으로 전환하고, 돌아올 수 있어요.",
                    cx.place.workspace
                ));
            }
            buttons(ui, &cx, &mut acts);
            ack(ui, &cx, &mut acts);
        });
    });
    apply(app, &cx, acts);
}

pub fn show_tab(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid) {
    ui.painter().rect_filled(ui.max_rect(), CornerRadius::ZERO, c::DOC);
    let Some(cx) = load(app, ui, host, request) else {
        return;
    };
    let mut acts = Vec::new();
    let selected = app.feat.collab.result_file.get(&request).cloned();
    let open_logs = app.feat.collab.open_logs.clone();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::symmetric(32, 28)).show(ui, |ui| {
            let w = ui.available_width().min(960.0);
            let side = (ui.available_width() - w) / 2.0;
            ui.horizontal(|ui| {
                ui.add_space(side.max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing.y = 18.0;
                    tab_head(ui, &cx, &mut acts);
                    let two = w > 720.0;
                    let col = if two { (w - 12.0) / 2.0 } else { w };
                    let left = |ui: &mut egui::Ui, acts: &mut Vec<Act>| {
                        card(ui, col, |ui| {
                            // After a failure the card lists what is left over; otherwise it is the result.
                            let heading = if cx.view.request.status == ReqStatus::Failed { "남아 있는 것" } else { "결과" };
                            ui.label(RichText::new(heading).font(semibold(13.0)).color(c::TEXT_STRONG));
                            summary(ui, &cx);
                            files(ui, &cx, selected.as_deref(), acts);
                            checks(ui, &cx, &open_logs, acts);
                            not_done(ui, &cx);
                            let link = ui.add(
                                egui::Label::new(RichText::new("전체 로그와 원문 보기").font(font(12.5)).color(c::TEXT_2).underline())
                                    .sense(Sense::click()),
                            );
                            if link.clicked() {
                                acts.push(Act::Peek);
                            }
                        });
                    };
                    if two {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            left(ui, &mut acts);
                            card(ui, col, |ui| right_card(app, ui, &cx, request, selected.as_deref()));
                        });
                    } else {
                        left(ui, &mut acts);
                        card(ui, col, |ui| right_card(app, ui, &cx, request, selected.as_deref()));
                    }
                    if cx.in_other_ws {
                        note(ui, &format!(
                            "다른 공간의 결과라 여기서는 읽기만 합니다. 파일을 열면 {} 공간으로 전환하고, 돌아올 수 있어요.",
                            cx.place.workspace
                        ));
                    }
                    note(ui, "자동으로 다시 실행하지 않았어요. 이전 시도의 기록은 어떤 선택을 해도 지워지지 않습니다.");
                    ack(ui, &cx, &mut acts);
                });
            });
        });
    });
    apply(app, &cx, acts);
}

fn card(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(16)).show(
        ui,
        |ui| {
            // Cards sit side by side in a horizontal row; their content still runs top to bottom.
            ui.vertical(|ui| {
                ui.set_width(width - 32.0);
                ui.set_max_width(width - 32.0);
                ui.spacing_mut().item_spacing.y = 12.0;
                add(ui);
            });
        },
    );
}

fn right_card(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, request: Uuid, selected: Option<&str>) {
    let r = &cx.view.request;
    if r.status == ReqStatus::Failed {
        ui.label(
            RichText::new(format!("이어서 수정하면 시도 {}로 진행해요", r.attempt + 1))
                .font(semibold(13.0))
                .color(c::TEXT_STRONG),
        );
        let files = r.result.as_ref().map(|x| x.files.len()).unwrap_or(0);
        ui.label(RichText::new("다시 쓰는 것").font(medium(12.0)).color(c::MINT));
        bullet(ui, icons::CHECK, c::MINT, &format!("작업 폴더에 남아 있는 변경 {files}개 (미커밋 포함)"));
        bullet(ui, icons::CHECK, c::MINT, "이전 시도의 대화와 출력 기록");
        ui.label(RichText::new("다시 수행하는 것").font(medium(12.0)).color(c::YELLOW));
        bullet(ui, icons::ROTATE_CCW, c::YELLOW, "맡긴 일 중 끝내지 못한 부분");
        let passed = r.result.as_ref().is_some_and(|x| x.checks.iter().any(|c| c.passed));
        let checks = if passed {
            "검증 다시 실행. 이전 통과는 지금 파일에 쓰지 않아요"
        } else {
            "검증 실행. 이전 시도에 통과 기록이 없어요"
        };
        bullet(ui, icons::ROTATE_CCW, c::YELLOW, checks);
        return;
    }
    match selected {
        Some(path) => {
            ui.label(RichText::new(path).font(mono(12.0)).color(c::TEXT_STRONG));
            diff_view(app, ui, cx.host, request, path, 520.0);
        }
        None => {
            ui.label(RichText::new("변경 내용").font(semibold(13.0)).color(c::TEXT_STRONG));
            let none = cx.view.request.result.as_ref().is_some_and(|x| x.files.is_empty());
            let note = if none {
                "바뀐 파일이 없어요"
            } else {
                "왼쪽에서 파일을 고르면 시작 기준과 비교한 변경을 보여 줘요"
            };
            widgets::empty_note(ui, note);
        }
    }
}

fn bullet(ui: &mut egui::Ui, glyph: &str, color: Color32, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label(icon(glyph, 13.0, color));
        ui.add(egui::Label::new(RichText::new(text).font(font(12.5)).color(c::TEXT)).wrap());
    });
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(font(12.0)).color(c::TEXT_3)).wrap());
}

fn switcher(ui: &mut egui::Ui, r: &Request, on: bool, w: f32) -> bool {
    let lk = look(r, false);
    let resp = egui::Frame::new()
        .fill(if on { c::CONTROL } else { c::ROW })
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            // In a horizontal row: title over state, not side by side.
            ui.vertical(|ui| {
                ui.set_width(w - 20.0);
                ui.set_max_width(w - 20.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                theme::truncated(ui, &r.title, medium(12.5), if on { c::TEXT_STRONG } else { c::TEXT });
                let label = if r.status == ReqStatus::Done && !r.acknowledged { "새 결과" } else { &lk.label };
                ui.label(RichText::new(label).font(font(11.5)).color(lk.tone.fg()));
            });
        })
        .response;
    let click = ui.interact(resp.rect, ui.scope_id().with(("switch", r.id)), Sense::click());
    !on && click.clicked()
}

fn failed_head(ui: &mut egui::Ui, cx: &Ctx) {
    let r = &cx.view.request;
    let what = failure_label(r.result.as_ref());
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.horizontal(|ui| {
            ui.label(icon(icons::TRIANGLE_ALERT, 13.0, c::ROSE));
            ui.label(RichText::new(format!("{what} · 시도 {}", r.attempt)).font(medium(12.5)).color(c::ROSE));
        });
        if let Some(reason) = &r.reason {
            ui.add(egui::Label::new(RichText::new(reason).font(font(12.5)).color(c::TEXT_2)).wrap());
        }
    });
}

fn tab_head(ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>) {
    let r = &cx.view.request;
    let lk = look(r, false);
    ui.horizontal_top(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 260.0).max(240.0));
            ui.spacing_mut().item_spacing.y = 4.0;
            let state = if r.status == ReqStatus::Failed {
                failure_label(r.result.as_ref()).to_string()
            } else {
                lk.label.clone()
            };
            ui.horizontal(|ui| {
                ui.label(icon(lk.icon, 14.0, lk.tone.fg()));
                ui.label(RichText::new(format!("{state} · 시도 {}", r.attempt)).font(medium(12.5)).color(lk.tone.fg()));
            });
            ui.add(egui::Label::new(RichText::new(&r.title).font(semibold(ty::TITLE)).color(c::TEXT_STRONG)).wrap());
            if let Some(reason) = &r.reason {
                ui.add(egui::Label::new(RichText::new(reason).font(font(ty::BODY)).color(c::TEXT_2)).wrap());
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            buttons_inner(ui, cx, acts, 34.0);
        });
    });
}

fn summary(ui: &mut egui::Ui, cx: &Ctx) {
    let v = &cx.view;
    let r = &v.request;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        match &v.answer {
            Some(a) => {
                let from = match v.answer_from {
                    Some(SummaryFrom::Report) => "작업자 보고",
                    Some(SummaryFrom::Transcript) => "대화 기록의 마지막 답",
                    Some(SummaryFrom::Screen) => "화면의 마지막 부분 · 대화 기록 없음",
                    None => "",
                };
                let text: String = a.lines().take(12).collect::<Vec<_>>().join("\n");
                ui.add(egui::Label::new(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_STRONG)).wrap());
                ui.label(RichText::new(from).font(font(11.5)).color(c::TEXT_3));
            }
            None if r.status == ReqStatus::Done => {
                ui.label(
                    RichText::new("결과 수집 중 · 대화 기록에서 답을 아직 찾지 못했어요")
                        .font(font(12.5))
                        .color(c::TEXT_3),
                );
            }
            None => {
                ui.label(RichText::new("아직 결과 요약이 없어요").font(font(12.5)).color(c::TEXT_3));
            }
        }
        let mut meta = format!(
            "{} · {} / {}",
            r.tool.label(),
            cx.place.workspace,
            cx.place.branch.clone().unwrap_or_else(|| "branch 없음".into())
        );
        let res = r.result.as_ref();
        match (res.and_then(|x| x.base_rev.as_deref()), res.and_then(|x| x.head_rev.as_deref())) {
            (Some(b), Some(h)) => meta.push_str(&format!("\n{} → {} · 시도 {}", short(b), short(h), r.attempt)),
            _ => meta.push_str(&format!("\n시도 {}", r.attempt)),
        }
        let resp = ui.label(RichText::new(meta).font(mono(11.0)).color(c::TEXT_3));
        resp.on_hover_text(&v.worker_cwd);
        if !v.folder_exists {
            ui.label(RichText::new("작업 폴더가 없어져 기록해 둔 결과만 보여요").font(font(12.0)).color(c::YELLOW));
        }
    });
}

fn files(ui: &mut egui::Ui, cx: &Ctx, selected: Option<&str>, acts: &mut Vec<Act>) {
    changed_files(ui, cx, selected, acts);
    reported_unchanged(ui, cx);
}

/// Files the worker named in its report that git does not show as changed since the start.
fn unchanged_reports(r: &WorkResult) -> Vec<String> {
    r.reported_files.iter().filter(|p| !r.files.iter().any(|f| &f.path == *p)).cloned().collect()
}

fn reported_unchanged(ui: &mut egui::Ui, cx: &Ctx) {
    let Some(res) = cx.view.request.result.as_ref() else {
        return;
    };
    let list = unchanged_reports(res);
    if list.is_empty() {
        return;
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(RichText::new("작업자가 보고한 파일 (변경 없음)").font(medium(12.0)).color(c::TEXT_3));
        for p in &list {
            theme::truncated(ui, p, mono(11.5), c::TEXT_2);
        }
    });
}

fn changed_files(ui: &mut egui::Ui, cx: &Ctx, selected: Option<&str>, acts: &mut Vec<Act>) {
    let list = cx.view.request.result.as_ref().map(|x| x.files.clone()).unwrap_or_default();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new(format!("변경 파일 {}", list.len())).font(medium(12.0)).color(c::TEXT_3));
        if list.is_empty() {
            let text = if cx.view.request.result.is_none() {
                "작업이 끝나면 바뀐 파일을 보여 줘요"
            } else {
                "시작 기준과 비교해 바뀐 파일이 없어요"
            };
            ui.label(RichText::new(text).font(font(12.5)).color(c::TEXT_3));
            return;
        }
        if cx.view.request.result.as_ref().is_some_and(|x| x.shared_folder) {
            ui.horizontal_top(|ui| {
                ui.label(icon(icons::TRIANGLE_ALERT, 13.0, c::YELLOW));
                ui.add(
                    egui::Label::new(
                        RichText::new("같은 폴더에서 다른 실행이 바꾼 파일이 섞여 있을 수 있어요")
                            .font(font(12.5))
                            .color(c::YELLOW),
                    )
                    .wrap(),
                );
            });
        }
        egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for f in &list {
                let on = selected == Some(f.path.as_str());
                let resp = widgets::row(ui, 32.0, on, &f.path);
                let r = resp.rect.shrink2(egui::vec2(10.0, 0.0));
                let mut right = r.right();
                for (text, color) in [
                    (f.removed.filter(|d| *d > 0).map(|d| format!("−{d}")), Color32::from_rgb(0xFF, 0x9C, 0x9C)),
                    (f.added.map(|a| format!("+{a}")), Color32::from_rgb(0x7E, 0xE0, 0xA8)),
                ] {
                    if let Some(t) = text {
                        let w = widgets::text_width(ui, &t, mono(11.0));
                        ui.painter().text(
                            egui::pos2(right, r.center().y),
                            egui::Align2::RIGHT_CENTER,
                            &t,
                            mono(11.0),
                            color,
                        );
                        right -= w + 8.0;
                    }
                }
                let status = if f.status == "?" { "새 파일" } else { f.status.as_str() };
                let label = format!("{}  {status}", tail(&f.path));
                widgets::paint_clipped(
                    ui,
                    egui::Rect::from_min_max(r.min, egui::pos2(right - 4.0, r.max.y)),
                    &label,
                    mono(11.5),
                    c::TEXT,
                    true,
                );
                if resp.on_hover_text(&f.path).clicked() {
                    acts.push(Act::Select(f.path.clone()));
                }
            }
        });
    });
}

/// The end of a long path stays readable ("…/auth/refresh.test.ts").
fn tail(path: &str) -> String {
    const MAX: usize = 48;
    let n = path.chars().count();
    if n <= MAX {
        return path.to_string();
    }
    format!("…{}", path.chars().skip(n - MAX + 1).collect::<String>())
}

fn diff_view(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid, path: &str, max_h: f32) {
    let q = app.hub.query::<CollabDiff>(host, &DiffQuery { id: request, path: path.to_string() });
    let Some(d) = q.data else {
        let t = q.error.map(|e| e.message).unwrap_or_else(|| "변경을 불러오는 중…".into());
        widgets::empty_note(ui, &t);
        return;
    };
    egui::Frame::new().fill(c::TERMINAL).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            if d.binary {
                ui.label(
                    RichText::new("바이너리 파일이라 내용을 비교해 보여 줄 수 없어요")
                        .font(font(12.0))
                        .color(c::TEXT_3),
                );
                return;
            }
            if d.text.trim().is_empty() {
                ui.label(
                    RichText::new("시작 기준과 같아요 (또는 파일이 없어졌어요)").font(font(12.0)).color(c::TEXT_3),
                );
                return;
            }
            // Inside a row of cards the first height offered is small; ask for what the lines need.
            let row_h = ui.fonts_mut(|f| f.row_height(&mono(12.0)));
            let want = (d.text.lines().count().min(4000) as f32 * row_h).min(max_h);
            let area = egui::ScrollArea::both().max_height(max_h).min_scrolled_height(want).auto_shrink([false, true]);
            area.id_salt(("diff", request, path)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for line in d.text.lines().take(4000) {
                    let color = if line.starts_with("+++")
                        || line.starts_with("---")
                        || line.starts_with("diff ")
                        || line.starts_with("index ")
                    {
                        c::TEXT_3
                    } else if line.starts_with('+') {
                        Color32::from_rgb(0xBD, 0xF2, 0xD2)
                    } else if line.starts_with('-') {
                        Color32::from_rgb(0xFF, 0xB4, 0xBA)
                    } else if line.starts_with("@@") {
                        c::SKY
                    } else {
                        c::TEXT_2
                    };
                    ui.add(egui::Label::new(RichText::new(line).font(mono(12.0)).color(color)).extend());
                }
                if d.truncated {
                    ui.label(RichText::new("… 너무 길어 일부만 보여요").font(font(12.0)).color(c::TEXT_3));
                }
            });
        },
    );
}

fn checks(ui: &mut egui::Ui, cx: &Ctx, open_logs: &std::collections::HashSet<(Uuid, usize)>, acts: &mut Vec<Act>) {
    let r = &cx.view.request;
    let list = r.result.as_ref().map(|x| x.checks.clone()).unwrap_or_default();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new("검증").font(medium(12.0)).color(c::TEXT_3));
        egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10)).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 8.0;
                if list.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(icon(icons::MINUS, 13.0, c::TEXT_3));
                        ui.label(RichText::new("보고된 검증 없음 · 미검증").font(font(12.5)).color(c::TEXT_2));
                    });
                    return;
                }
                if cx.view.checks_stale {
                    ui.horizontal(|ui| {
                        ui.label(icon(icons::TRIANGLE_ALERT, 13.0, c::YELLOW));
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "검증 뒤에 파일이 바뀌었어요. 아래 결과는 이전 버전 기준이라 통과로 쓰지 않아요.",
                                )
                                .font(font(12.5))
                                .color(c::YELLOW),
                            )
                            .wrap(),
                        );
                    });
                }
                for (i, ch) in list.iter().enumerate() {
                    let (glyph, color, word) = if cx.view.checks_stale {
                        (
                            icons::TRIANGLE_ALERT,
                            c::YELLOW,
                            if ch.passed { "통과 · 이전 버전 기준" } else { "실패 · 이전 버전 기준" },
                        )
                    } else if ch.passed {
                        (icons::CHECK, c::MINT, "통과")
                    } else {
                        (icons::X, c::ROSE, "실패")
                    };
                    ui.horizontal_top(|ui| {
                        ui.label(icon(glyph, 13.0, color)).on_hover_text(word);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add(egui::Label::new(RichText::new(&ch.command).font(mono(12.0)).color(c::TEXT)).wrap());
                            let at = ch.revision.as_deref().map(short).unwrap_or_else(|| "기준 없음".into());
                            ui.label(
                                RichText::new(format!("{word} · {at} 기준 · {}", data::ago(ch.at)))
                                    .font(font(12.0))
                                    .color(c::TEXT_3),
                            );
                            if let Some(log) = &ch.log {
                                let open = open_logs.contains(&(r.id, i));
                                let t = if open { "로그 접기" } else { "로그 보기" };
                                if ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(t).font(font(12.0)).color(c::TEXT_2).underline(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .clicked()
                                {
                                    acts.push(Act::ToggleLog(i));
                                }
                                if open {
                                    egui::Frame::new()
                                        .fill(c::TERMINAL)
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::same(8))
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(log)
                                                        .font(mono(11.5))
                                                        .color(Color32::from_rgb(0xFF, 0x9C, 0x9C)),
                                                )
                                                .wrap(),
                                            );
                                        });
                                }
                            }
                        });
                    });
                }
            },
        );
    });
}

fn not_done(ui: &mut egui::Ui, cx: &Ctx) {
    let list = cx.view.request.result.as_ref().map(|x| x.not_done.clone()).unwrap_or_default();
    if list.is_empty() {
        return;
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(RichText::new("하지 못한 일").font(medium(12.0)).color(c::TEXT_3));
        for n in &list {
            bullet(ui, icons::MINUS, c::TEXT_3, n);
        }
    });
}

fn buttons(ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        buttons_inner(ui, cx, acts, 30.0);
    });
}

fn btn(ui: &mut egui::Ui, text: &str, primary: bool, h: f32) -> egui::Response {
    let b = if primary {
        egui::Button::new(RichText::new(text).font(medium(12.5)).color(c::ON_EMBER)).fill(c::EMBER)
    } else {
        egui::Button::new(RichText::new(text).font(font(12.5)).color(c::TEXT_STRONG)).fill(c::CONTROL)
    };
    ui.add(b.min_size(egui::vec2(0.0, h)).corner_radius(CornerRadius::same(7)))
}

/// Context actions. Exactly one is ember: 이어서 수정 after a failure, 통합 준비 for a finished
/// worktree, otherwise none.
fn buttons_inner(ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>, h: f32) {
    let r = &cx.view.request;
    let failed = r.status == ReqStatus::Failed;
    let can_merge = r.status == ReqStatus::Done && cx.place.kind == Some(WorkspaceKind::Worktree);
    let first_file = r.result.as_ref().and_then(|x| x.files.iter().find(|f| f.status != "D")).map(|f| f.path.clone());
    let rtl = ui.layout().prefer_right_to_left();
    let mut items: Vec<(&str, bool, Act)> = Vec::new();
    if can_merge {
        items.push(("통합 준비", true, Act::Merge));
    }
    if let Some(p) = app_file(cx, first_file) {
        items.push(("파일 열기", false, Act::OpenFile(p)));
    }
    if !r.status.is_open() && !failed {
        items.push(("검토 요청", false, Act::Review));
    }
    if failed {
        items.push(("처음부터 다시", false, Act::Retry(RetryMode::Fresh)));
        items.push(("이어서 수정", true, Act::Retry(RetryMode::Continue)));
    } else if r.status == ReqStatus::Done {
        items.push(("이어서 수정", false, Act::Retry(RetryMode::Continue)));
    }
    if rtl {
        items.reverse();
    }
    for (text, primary, act) in items {
        if btn(ui, text, primary, h).clicked() {
            acts.push(act);
        }
    }
    if widgets::icon_btn(ui, icons::COPY, "작업 폴더 경로 복사", h, h, 14.0).clicked() {
        acts.push(Act::CopyPath);
    }
}

fn app_file(cx: &Ctx, first: Option<String>) -> Option<String> {
    first.filter(|_| cx.view.folder_exists && cx.place.ws.is_some())
}

fn ack(ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>) {
    let r = &cx.view.request;
    if r.status.is_open() {
        return;
    }
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() - 6.0, egui::Stroke::new(1.0, c::LINE));
    let mut on = r.acknowledged;
    let label = RichText::new("확인함으로 표시").font(font(12.5)).color(c::TEXT);
    ui.horizontal(|ui| {
        if ui.checkbox(&mut on, label).changed() {
            acts.push(Act::Ack(on));
        }
        ui.label(RichText::new("(검증 통과와는 별개예요)").font(font(12.5)).color(c::TEXT_3));
    });
}

pub fn retry(app: &mut App, host: HostKey, id: Uuid, mode: RetryMode) {
    let (cols, rows) = app.shell.center.terminal_grid();
    app.hub.call::<CollabRetry>(host, Retry { id, mode, note: None, cols, rows }, move |app, res| match res {
        Ok(d) => {
            let what = match mode {
                RetryMode::Continue => "같은 작업자에게 이어서 맡겼어요",
                RetryMode::Fresh => "새 작업자에게 처음부터 맡겼어요",
            };
            app.toast_info(&format!("시도 {} · {what}", d.request.attempt));
        }
        Err(e) => app.toast_error(&e.message),
    });
}

fn apply(app: &mut App, cx: &Ctx, acts: Vec<Act>) {
    let host = cx.host;
    let r = &cx.view.request;
    for a in acts {
        match a {
            Act::Select(p) => {
                let cur = app.feat.collab.result_file.get(&r.id);
                if cur == Some(&p) {
                    app.feat.collab.result_file.remove(&r.id);
                } else {
                    app.feat.collab.result_file.insert(r.id, p);
                }
            }
            Act::Switch(id) => app.ui.right = Some(RightPanel::Result { host, request: id }),
            Act::Ack(on) => app.hub.send::<CollabAck>(host, Ack { id: r.id, acknowledged: on }),
            Act::Merge => {
                if let (Some(project), Some(ws)) = (cx.place.project_id, cx.place.ws) {
                    actions::select_workspace(app, project, ws);
                    app.ui.open_tab(ws, TabKind::Merge { ws });
                }
            }
            Act::OpenFile(path) => {
                if let (Some(project), Some(ws)) = (cx.place.project_id, cx.place.ws) {
                    actions::select_workspace(app, project, ws);
                    crate::views::docs::open(app, ws, &path, false);
                }
            }
            Act::Review => review(app, cx),
            Act::Retry(mode) => retry(app, host, r.id, mode),
            Act::CopyPath => {
                if let Some(ctx) = app.shell.center.ctx() {
                    actions::copy_path(app, &ctx, &cx.view.worker_cwd);
                }
            }
            Act::Peek => super::peek::open(app, host, r.worker_id, None),
            Act::ToggleLog(i) => {
                let key = (r.id, i);
                if !app.feat.collab.open_logs.remove(&key) {
                    app.feat.collab.open_logs.insert(key);
                }
            }
        }
    }
}

/// "검토 요청": the other CLI reviews this result in the same workspace (a person's request).
fn review(app: &mut App, cx: &Ctx) {
    let r = &cx.view.request;
    let Some(worker) = &cx.worker else { return };
    let tool = if r.tool == Tool::Codex { Tool::Claude } else { Tool::Codex };
    let files: Vec<String> =
        r.result.as_ref().map(|x| x.files.iter().map(|f| f.path.clone()).collect()).unwrap_or_default();
    let mut prompt = format!(
        "다음 작업 결과를 검토해 주세요. 바뀐 파일을 직접 읽고 문제와 확인한 범위를 알려 주세요.\n\n맡긴 일: {}\n",
        r.title
    );
    if let Some(a) = &cx.view.answer {
        prompt.push_str(&format!("작업자 요약: {}\n", first_line(a, 400)));
    }
    if !files.is_empty() {
        prompt.push_str(&format!("바뀐 파일: {}\n", files.join(", ")));
    }
    if let Some(base) = r.start_rev.as_deref() {
        prompt.push_str(&format!("비교 기준 revision: {}\n", short(base)));
    }
    let (cols, rows) = app.shell.center.terminal_grid();
    let d = Delegate {
        parent_id: r.parent_id,
        workspace_id: Some(worker.session.workspace_id),
        tool,
        role: Some("검토".into()),
        title: Some(format!("검토: {}", r.title)),
        prompt,
        split: None,
        key: None,
        source: Source::Person,
        cols,
        rows,
    };
    let host = cx.host;
    let parent = r.parent_id;
    app.hub.call::<CollabDelegate>(host, d, move |app, res| match res {
        Ok(d) => {
            app.toast_info(&format!("{}에게 검토를 맡겼어요 ({})", d.request.tool.label(), d.request.receipt()));
            if let Some(p) = parent {
                app.ui.right = Some(RightPanel::Collab { host, session: p });
            }
        }
        Err(e) => app.toast_error(&e.message),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_files_git_does_not_show_are_listed_apart() {
        let changed = ChangedFile { path: "src/a.rs".into(), status: "M".into(), added: None, removed: None };
        let r = WorkResult {
            files: vec![changed],
            reported_files: vec!["src/a.rs".into(), "docs/plan.md".into()],
            ..Default::default()
        };
        assert_eq!(unchanged_reports(&r), vec!["docs/plan.md".to_string()]);
        assert!(unchanged_reports(&WorkResult::default()).is_empty());
    }

    #[test]
    fn long_paths_keep_their_end() {
        assert_eq!(tail("src/a.rs"), "src/a.rs");
        let long = format!("{}/refresh.test.ts", "very/deep/folder".repeat(5));
        let t = tail(&long);
        assert!(t.starts_with('…') && t.ends_with("refresh.test.ts") && t.chars().count() == 48);
        assert_eq!(first_line("\n  첫 줄\n둘째", 10), "첫 줄");
    }
}
