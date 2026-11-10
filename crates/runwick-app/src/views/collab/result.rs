//! Worker result reader shared by the panel and expanded center tabs.

use super::{Place, place, short};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{data, widgets};
use crate::theme::{self, c, font, icon, medium, mono, ty};
use egui::{Color32, CornerRadius, Margin, RichText, Sense};
use runwick_core::api::collab::*;
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::{SessionInfo, Tool};
use uuid::Uuid;

enum Act {
    Select(String),
    Peek,
    Direct,
    Stop,
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
    let r = &view.request;
    let key = (host, r.id, r.updated_at);
    let previous = app.feat.collab.read_versions.get(&key);
    let first = previous.is_none();
    if !r.status.is_open() && !r.acknowledged && previous.is_none_or(super::ReadAttempt::can_retry) {
        app.feat
            .collab
            .read_versions
            .insert(key, super::ReadAttempt { at: std::time::Instant::now(), pending_or_done: true });
        app.hub.call::<CollabRead>(host, ById { id: r.id }, move |app, res| {
            if let Err(e) = res {
                if let Some(attempt) = app.feat.collab.read_versions.get_mut(&key) {
                    attempt.pending_or_done = false;
                }
                if first {
                    app.toast_error(&format!("읽음 표시를 저장하지 못했어요: {}", e.message));
                }
            }
        });
    }
    Some(Ctx { host, view, worker, place })
}

pub fn show_panel(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid) {
    show(app, ui, host, request, true);
}

pub fn show_tab(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid) {
    ui.painter().rect_filled(ui.max_rect(), CornerRadius::ZERO, c::DOC);
    show(app, ui, host, request, false);
}

fn show(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid, panel: bool) {
    let Some(cx) = load(app, ui, host, request) else { return };
    let mut acts = Vec::new();
    let selected = app.feat.collab.result_file.get(&request).cloned();
    let open_logs = app.feat.collab.open_logs.clone();
    let width = (ui.available_width() - 28.0).clamp(120.0, 960.0);
    egui::ScrollArea::vertical().id_salt(("result", host, request, panel)).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width(width);
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.y = 14.0;
            ui.heading(&cx.view.request.title);
            if cx.view.request.status == ReqStatus::Failed {
                failed_head(ui, &cx);
            }
            buttons(ui, &cx, &mut acts);
            summary(ui, &cx);
            files(ui, &cx, selected.as_deref(), &mut acts);
            if let Some(path) = &selected {
                diff_view(app, ui, host, request, path, if panel { 260.0 } else { 520.0 });
            }
            checks(ui, &cx, &open_logs, &mut acts);
            not_done(ui, &cx);
            if let Some(answer) = &cx.view.answer {
                ui.label(RichText::new("전체 답변").font(medium(12.0)).color(c::TEXT_3));
                ui.add(egui::Label::new(RichText::new(answer).font(font(ty::BODY)).color(c::TEXT)).wrap());
            }
        });
    });
    apply(app, &cx, acts);
}

fn bullet(ui: &mut egui::Ui, glyph: &str, color: Color32, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label(icon(glyph, 13.0, color));
        ui.add(egui::Label::new(RichText::new(text).font(font(12.5)).color(c::TEXT)).wrap());
    });
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
                let text = r.result.as_ref().and_then(|r| r.summary.clone()).unwrap_or_else(|| first_line(a, 240));
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
                        ui.label(
                            RichText::new(checks_empty_label(r.tool, cx.view.answer.is_some()))
                                .font(font(12.5))
                                .color(c::TEXT_2),
                        );
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

fn checks_empty_label(tool: Tool, answer: bool) -> &'static str {
    if tool == Tool::Codex && answer { "검증 내용은 답변에 있어요" } else { "보고된 검증 없음" }
}

fn buttons(ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>) {
    ui.horizontal_wrapped(|ui| {
        if theme::secondary_button(ui, "화면 보기").clicked() {
            acts.push(Act::Peek);
        }
        if theme::secondary_button(ui, "직접 대화").clicked() {
            acts.push(Act::Direct);
        }
        if ui
            .add_enabled(
                cx.worker.as_ref().is_some_and(|w| {
                    w.live.as_ref().is_some_and(|l| {
                        matches!(
                            l.phase,
                            runwick_core::api::sessions::Phase::Running | runwick_core::api::sessions::Phase::Starting
                        )
                    })
                }),
                egui::Button::new("멈추기"),
            )
            .clicked()
        {
            acts.push(Act::Stop);
        }
    });
}

fn apply(app: &mut App, cx: &Ctx, acts: Vec<Act>) {
    let host = cx.host;
    let r = &cx.view.request;
    for a in acts {
        match a {
            Act::Select(p) => {
                if app.feat.collab.result_file.get(&r.id) == Some(&p) {
                    app.feat.collab.result_file.remove(&r.id);
                } else {
                    app.feat.collab.result_file.insert(r.id, p);
                }
            }
            Act::Peek => super::peek::open(app, host, r.worker_id, None),
            Act::Direct => super::direct(app, host, r.worker_id),
            Act::Stop => app.shell.modal.ask_stop(host, r.worker_id),
            Act::ToggleLog(i) => {
                let key = (r.id, i);
                if !app.feat.collab.open_logs.remove(&key) {
                    app.feat.collab.open_logs.insert(key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_structured_checks_are_neutral_and_codex_points_to_the_answer() {
        assert_eq!(checks_empty_label(Tool::Claude, true), "보고된 검증 없음");
        assert_eq!(checks_empty_label(Tool::Codex, false), "보고된 검증 없음");
        assert_eq!(checks_empty_label(Tool::Codex, true), "검증 내용은 답변에 있어요");
    }

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
