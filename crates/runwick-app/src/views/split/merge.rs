//! S11 통합 (Merge.dc.html): bring a separated workspace's branch into the project's default
//! workspace with a local `git merge` (not committed, never pushed). Conflicts list the files with
//! 해결 맡기기 · 직접 해결 · 보류 (`git merge --abort`); earlier checks are shown as from the
//! previous version, never as passed. Cleanup: archive, or remove the folder after a check of
//! running sessions, uncommitted and unpushed work.

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, data, widgets};
use crate::state::{TabKind, WsKey};
use crate::theme::{self, c, font, icon, medium, mono, semibold, ty};
use egui::{Color32, CornerRadius, Margin, RichText};
use runwick_core::api::collab::{CollabDelegate, CollabList, Delegate, RequestQuery, Source};
use runwick_core::api::projects::{ById, ProjectId, Workspace, WorkspaceKind, WorkspaceUpdate, WorkspacesUpdate};
use runwick_core::api::sessions::Tool;
use runwick_core::api::worktree::*;

#[derive(Default)]
pub struct State {
    /// The folder-removal check is open for this workspace.
    removing: Option<WsKey>,
    force: bool,
    busy: bool,
}

enum Act {
    Merge,
    Abort,
    Delegate(Vec<String>),
    OpenFile(String),
    OpenTarget,
    Archive,
    AskRemove,
    Remove,
    CancelRemove,
    Force(bool),
}

pub fn show_tab(app: &mut App, ui: &mut egui::Ui, ws: WsKey, _tab: crate::state::TabId) {
    ui.painter().rect_filled(ui.max_rect(), CornerRadius::ZERO, c::DOC);
    if ws.host != HostKey::Local {
        pad(ui, |ui| widgets::empty_note(ui, "다른 기기의 공간은 그 기기에서 통합해요"));
        return;
    }
    let wl = data::workspaces(app).data;
    let all_ws = app.hub.query::<runwick_core::api::projects::WorkspacesList>(
        HostKey::Local,
        &runwick_core::api::projects::WorkspaceQuery { project_id: None, include_archived: true },
    );
    let me = all_ws.data.as_ref().and_then(|l| l.iter().find(|w| w.id == ws.id).cloned());
    let Some(me) = me else {
        pad(ui, |ui| {
            widgets::empty_note(ui, if all_ws.data.is_some() { "이 공간을 찾을 수 없어요" } else { "불러오는 중…" })
        });
        return;
    };
    let target = wl.as_ref().and_then(|l| {
        l.iter()
            .find(|w| w.project_id == me.project_id && w.kind == WorkspaceKind::Default && w.remote.is_none())
            .cloned()
    });
    let pq = app.hub.query::<WorktreeMergePreview>(ws.host, &ById { id: ws.id });
    let Some(pv) = pq.data.as_deref().cloned() else {
        let t = pq
            .error
            .map(|e| format!("통합 정보를 읽지 못했어요: {}", e.message))
            .unwrap_or_else(|| "통합할 변경을 확인하는 중…".into());
        pad(ui, |ui| widgets::empty_note(ui, &t));
        return;
    };
    let conflict_texts: Vec<(String, Option<String>)> = pv
        .conflicts
        .iter()
        .map(|p| {
            let q = app.hub.query::<WorktreeConflict>(ws.host, &ConflictQuery { workspace_id: ws.id, path: p.clone() });
            (p.clone(), q.data.map(|s| s.as_ref().clone()).or(q.error.map(|e| e.message)))
        })
        .collect();
    let checks = earlier_checks(app, ws);
    let removing = app.feat.split.merge.removing == Some(ws);
    let rc = removing.then(|| app.hub.query::<WorktreeRemoveCheck>(ws.host, &ById { id: ws.id }).data).flatten();
    let unsaved = if removing { unsaved_docs(app, ws) } else { Vec::new() };
    let force = app.feat.split.merge.force;
    let busy = app.feat.split.merge.busy;
    let mut acts = Vec::new();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin::symmetric(32, 28)).show(ui, |ui| {
            let w = ui.available_width().min(880.0);
            let side = ((ui.available_width() - w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing.y = 18.0;
                    head(ui, &me, &pv);
                    if pv.in_progress && pv.conflicts.is_empty() {
                        merged_card(ui, busy, &mut acts);
                    } else if !pv.conflicts.is_empty() {
                        for (i, (path, text)) in conflict_texts.iter().enumerate() {
                            conflict_card(ui, path, text.as_deref(), i == 0, &pv.conflicts, busy, &mut acts);
                        }
                    } else {
                        preview_card(ui, &pv, busy, &mut acts);
                    }
                    ui.columns(2, |cols| {
                        card(&mut cols[0], |ui| {
                            ui.label(RichText::new("검증").font(semibold(13.0)).color(c::TEXT_STRONG));
                            if checks.is_empty() {
                                ui.label(
                                    RichText::new("보고된 검증이 없어요. 통합한 뒤에 검증을 실행하세요.")
                                        .font(font(12.5))
                                        .color(c::TEXT_2),
                                );
                            } else {
                                for (cmd, rev) in &checks {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!("{cmd} 통과는 이전 버전({rev}) 기준이에요."))
                                                .font(font(12.5))
                                                .color(c::YELLOW),
                                        )
                                        .wrap(),
                                    );
                                }
                                ui.label(
                                    RichText::new("통합한 뒤 다시 실행해야 통과로 표시합니다.")
                                        .font(font(12.5))
                                        .color(c::TEXT_2),
                                );
                            }
                        });
                        card(&mut cols[1], |ui| {
                            ui.label(RichText::new("이 단계의 범위").font(semibold(13.0)).color(c::TEXT_STRONG));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "로컬 통합까지만 진행해요. 커밋, push, PR, 배포는 따로 지시해야 실행됩니다.",
                                    )
                                    .font(font(12.5))
                                    .color(c::TEXT_2),
                                )
                                .wrap(),
                            );
                        });
                    });
                    cleanup(ui, &me, removing, rc.as_deref(), &unsaved, force, busy, &mut acts);
                });
            });
        });
    });

    for a in acts {
        apply(app, ws, &me, target.as_ref(), a);
    }
}

fn pad(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(Margin::same(24)).show(ui, add);
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(14)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui);
        },
    );
}

/// Passing checks reported by workers of this workspace (shown as from an earlier version).
fn earlier_checks(app: &mut App, ws: WsKey) -> Vec<(String, String)> {
    let workers: Vec<uuid::Uuid> = data::all_sessions(app, ws.host)
        .data
        .map(|l| l.iter().filter(|s| s.session.workspace_id == ws.id).map(|s| s.session.id).collect())
        .unwrap_or_default();
    let reqs = app.hub.query::<CollabList>(ws.host, &RequestQuery { limit: 300, ..Default::default() }).data;
    let mut out: Vec<(String, String)> = Vec::new();
    for r in reqs.iter().flat_map(|l| l.iter()).filter(|r| workers.contains(&r.worker_id)) {
        for ch in r.result.iter().flat_map(|x| x.checks.iter()).filter(|c| c.passed) {
            let rev = ch.revision.as_deref().map(crate::views::collab::short).unwrap_or_else(|| "기준 없음".into());
            if !out.iter().any(|(c, _)| *c == ch.command) {
                out.push((ch.command.clone(), rev));
            }
        }
    }
    out
}

fn head(ui: &mut egui::Ui, me: &Workspace, pv: &MergePreview) {
    let target = pv.target_branch.clone().unwrap_or_else(|| "기본 공간".into());
    let (glyph, color, line) = if !pv.conflicts.is_empty() {
        (icons::CIRCLE_ALERT, c::EMBER, format!("충돌 {}개 · 내 결정이 필요해요", pv.conflicts.len()))
    } else if pv.in_progress {
        (icons::CIRCLE_CHECK, c::MINT, "합쳤어요 · 아직 커밋하지 않았어요".to_string())
    } else if let Some(r) = &pv.reason {
        (icons::TRIANGLE_ALERT, c::YELLOW, r.clone())
    } else {
        (icons::GIT_MERGE, c::TEXT_2, format!("가져올 커밋 {}개", pv.commits.len()))
    };
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.horizontal(|ui| {
            ui.label(icon(glyph, 13.0, color));
            ui.label(RichText::new(line).font(medium(12.5)).color(color));
        });
        ui.add(
            egui::Label::new(
                RichText::new(format!("{}를 {target}에 통합 준비", me.name))
                    .font(semibold(ty::TITLE))
                    .color(c::TEXT_STRONG),
            )
            .wrap(),
        );
        let rev = |b: &Option<String>, r: &Option<String>| {
            format!("{} {}", b.clone().unwrap_or_default(), r.clone().unwrap_or_default())
        };
        let mut meta = format!(
            "{} → {}",
            rev(&pv.source_branch, &pv.source_rev).trim(),
            rev(&pv.target_branch, &pv.target_rev).trim()
        );
        if let Some(b) = &pv.merge_base {
            meta.push_str(&format!(" · 출발 {b}"));
        }
        ui.label(RichText::new(meta).font(mono(12.0)).color(c::TEXT_2));
    });
}

fn preview_card(ui: &mut egui::Ui, pv: &MergePreview, busy: bool, acts: &mut Vec<Act>) {
    card(ui, |ui| {
        ui.label(RichText::new(format!("가져올 커밋 {}", pv.commits.len())).font(semibold(13.0)).color(c::TEXT_STRONG));
        if pv.commits.is_empty() {
            ui.label(
                RichText::new("기본 공간에 없는 커밋이 없어요. 커밋한 뒤에 통합할 수 있어요.")
                    .font(font(12.5))
                    .color(c::TEXT_2),
            );
        }
        for cm in pv.commits.iter().take(30) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&cm.rev).font(mono(12.0)).color(c::TEXT_3));
                theme::truncated(ui, &cm.subject, font(12.5), c::TEXT);
            });
        }
        if !pv.files.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("바뀐 파일 {}", pv.files.len())).font(medium(12.0)).color(c::TEXT_3));
            for f in pv.files.iter().take(60) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&f.status).font(mono(11.5)).color(c::TEXT_3));
                    ui.label(RichText::new(&f.path).font(mono(11.5)).color(c::TEXT));
                    if let Some(a) = f.added {
                        ui.label(
                            RichText::new(format!("+{a}")).font(mono(11.0)).color(Color32::from_rgb(0x7E, 0xE0, 0xA8)),
                        );
                    }
                    if let Some(d) = f.removed.filter(|d| *d > 0) {
                        ui.label(
                            RichText::new(format!("−{d}")).font(mono(11.0)).color(Color32::from_rgb(0xFF, 0x9C, 0x9C)),
                        );
                    }
                });
            }
        }
        if pv.source_dirty > 0 {
            ui.label(
                RichText::new(format!("이 공간의 커밋하지 않은 변경 {}개는 통합에 들어가지 않아요", pv.source_dirty))
                    .font(font(12.5))
                    .color(c::YELLOW),
            );
        }
        if !pv.target_dirty.is_empty() {
            ui.label(
                RichText::new(format!(
                    "기본 공간에 커밋하지 않은 변경 {}개가 있어 통합하지 않아요. 먼저 정리해 주세요.",
                    pv.target_dirty.len()
                ))
                .font(font(12.5))
                .color(c::YELLOW),
            );
        }
        ui.add_space(4.0);
        let ok = !busy && pv.reason.is_none() && !pv.commits.is_empty() && pv.target_dirty.is_empty();
        let b = egui::Button::new(
            RichText::new(if busy { "통합하는 중…" } else { "통합하기" }).font(medium(12.5)).color(c::ON_EMBER),
        )
        .fill(c::EMBER)
        .min_size(egui::vec2(0.0, 30.0))
        .corner_radius(CornerRadius::same(7));
        if ui.add_enabled(ok, b).on_hover_text("기본 공간에서 git merge를 해요. 커밋과 push는 하지 않아요").clicked()
        {
            acts.push(Act::Merge);
        }
    });
}

fn merged_card(ui: &mut egui::Ui, busy: bool, acts: &mut Vec<Act>) {
    card(ui, |ui| {
        ui.add(
            egui::Label::new(
                RichText::new(
                    "변경을 기본 공간에 합쳤어요. 아직 커밋하지 않았어요. 커밋은 CLI에 지시하거나 직접 하세요.",
                )
                .font(font(12.5))
                .color(c::TEXT),
            )
            .wrap(),
        );
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, "기본 공간 열기").clicked() {
                acts.push(Act::OpenTarget);
            }
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(RichText::new("통합 취소").font(medium(13.0)).color(c::ROSE))
                        .fill(c::ROSE_BG)
                        .min_size(egui::vec2(0.0, 28.0)),
                )
                .on_hover_text("git merge --abort: 통합 전 상태로 돌려요")
                .clicked()
            {
                acts.push(Act::Abort);
            }
        });
    });
}

fn conflict_card(
    ui: &mut egui::Ui,
    path: &str,
    text: Option<&str>,
    first: bool,
    all: &[String],
    busy: bool,
    acts: &mut Vec<Act>,
) {
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new().inner_margin(Margin { left: 14, right: 10, top: 10, bottom: 10 }).show(ui, |ui| {
            let row = egui::vec2(ui.available_width(), 30.0);
            ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(RichText::new(path).font(mono(12.5)).color(c::TEXT_STRONG));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if ui
                        .add_enabled(!busy, egui::Button::new("보류").min_size(egui::vec2(0.0, 30.0)))
                        .on_hover_text("통합을 취소하고 이전 상태로 돌려요 (git merge --abort)")
                        .clicked()
                    {
                        acts.push(Act::Abort);
                    }
                    if ui.add(egui::Button::new("직접 해결").min_size(egui::vec2(0.0, 30.0))).clicked() {
                        acts.push(Act::OpenFile(path.to_string()));
                    }
                    let b = if first {
                        egui::Button::new(RichText::new("해결 맡기기").font(medium(12.5)).color(c::ON_EMBER))
                            .fill(c::EMBER)
                    } else {
                        egui::Button::new("해결 맡기기")
                    };
                    if ui
                        .add_enabled(!busy, b.min_size(egui::vec2(0.0, 30.0)))
                        .on_hover_text("CLI에 충돌 목록을 주고 해결을 맡겨요")
                        .clicked()
                    {
                        acts.push(Act::Delegate(all.to_vec()));
                    }
                });
            });
        });
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, c::LINE));
        egui::Frame::new().inner_margin(Margin::symmetric(0, 10)).show(ui, |ui| {
            let Some(text) = text else {
                ui.label(RichText::new("  파일을 읽는 중…").font(font(12.0)).color(c::TEXT_3));
                return;
            };
            let lines = conflict_window(text);
            // The card sits in a centered row whose first height is small; ask for the room the
            // lines need (up to 320) instead of what the row offers.
            let row_h = ui.fonts_mut(|f| f.row_height(&mono(12.5))) + 2.0;
            let want = (lines.len() as f32 * row_h).min(320.0);
            egui::ScrollArea::both()
                .max_height(320.0)
                .min_scrolled_height(want)
                .auto_shrink([false, true])
                .id_salt(("conflict", path))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut side = 0u8;
                    for line in lines {
                        let (fg, bg) = if line.starts_with("<<<<<<<") {
                            side = 1;
                            (c::TEXT_3, c::ROW)
                        } else if line.starts_with("|||||||") && side == 1 {
                            // diff3 style: the common base follows until "=======".
                            side = 3;
                            (c::TEXT_3, c::ROW)
                        } else if line.starts_with("=======") && matches!(side, 1 | 3) {
                            side = 2;
                            (c::TEXT_3, c::ROW)
                        } else if line.starts_with(">>>>>>>") {
                            side = 0;
                            (c::TEXT_3, c::ROW)
                        } else if side == 1 {
                            (Color32::from_rgb(0xBD, 0xF2, 0xD2), c::MINT_BG)
                        } else if side == 2 {
                            (Color32::from_rgb(0xC5, 0xE0, 0xFF), c::SKY_BG)
                        } else if side == 3 {
                            (c::TEXT_3, Color32::TRANSPARENT)
                        } else {
                            (c::TEXT_2, Color32::TRANSPARENT)
                        };
                        egui::Frame::new().fill(bg).inner_margin(Margin::symmetric(14, 1)).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.add(
                                egui::Label::new(
                                    RichText::new(if line.is_empty() { " " } else { line }).font(mono(12.5)).color(fg),
                                )
                                .extend(),
                            );
                        });
                    }
                });
        });
    });
}

/// Lines around the conflict markers (a few lines of context), so big files stay readable.
fn conflict_window(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let marks: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"))
        .map(|(i, _)| i)
        .collect();
    if marks.is_empty() {
        return lines.into_iter().take(200).collect();
    }
    let mut keep = vec![false; lines.len()];
    for pair in marks.chunks(2) {
        let start = pair[0].saturating_sub(3);
        let end = (pair.get(1).copied().unwrap_or(pair[0]) + 3).min(lines.len() - 1);
        for k in keep.iter_mut().take(end + 1).skip(start) {
            *k = true;
        }
    }
    let mut out = Vec::new();
    let mut gap = false;
    for (i, l) in lines.iter().enumerate() {
        if keep[i] {
            out.push(*l);
            gap = false;
        } else if !gap {
            out.push("⋯");
            gap = true;
        }
    }
    out
}

/// Documents of this workspace open with unsaved edits (they would point at a removed folder).
fn unsaved_docs(app: &mut App, ws: WsKey) -> Vec<String> {
    let paths: Vec<String> = app
        .ui
        .tabset(&ws)
        .map(|set| {
            set.tabs
                .iter()
                .filter_map(|t| match &t.kind {
                    TabKind::Doc { path, .. } => Some(path.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    paths.into_iter().filter(|p| crate::views::docs::is_dirty(app, ws, p)).collect()
}

/// Switch to a separated workspace and open its merge tab, with the folder-removal check
/// already open when `remove` (from the sidebar menu).
pub fn open_merge(app: &mut App, project: ProjectId, ws: WsKey, remove: bool) {
    actions::select_workspace(app, project, ws);
    app.ui.open_tab(ws, TabKind::Merge { ws });
    if remove {
        app.feat.split.merge.removing = Some(ws);
        app.feat.split.merge.force = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn cleanup(
    ui: &mut egui::Ui,
    me: &Workspace,
    removing: bool,
    rc: Option<&RemoveCheck>,
    unsaved: &[String],
    force: bool,
    busy: bool,
    acts: &mut Vec<Act>,
) {
    card(ui, |ui| {
        ui.label(RichText::new("통합이 끝나면 공간 정리").font(semibold(13.0)).color(c::TEXT_STRONG));
        ui.add(egui::Label::new(RichText::new("보관은 목록에서 접기만 해요. 폴더 제거는 실행 중 세션과 커밋하지 않은 변경, 올리지 않은 커밋을 먼저 확인해요. 대화 기록과 결과는 어느 쪽이든 남습니다.").font(font(12.5)).color(c::TEXT_2)).wrap());
        if !removing {
            ui.horizontal(|ui| {
                if theme::secondary_button(ui, "공간 보관").clicked() {
                    acts.push(Act::Archive);
                }
                if theme::secondary_button(ui, "폴더 제거…").clicked() {
                    acts.push(Act::AskRemove);
                }
            });
            return;
        }
        let Some(rc) = rc else {
            widgets::empty_note(ui, "제거해도 되는지 확인하는 중…");
            return;
        };
        ui.add_space(4.0);
        ui.label(RichText::new(data::tilde(&me.path)).font(mono(11.5)).color(c::TEXT));
        let mut blocked = false;
        if let Some(r) = &rc.reason {
            ui.label(RichText::new(r).font(font(12.5)).color(c::YELLOW));
            blocked = true;
        }
        if !rc.running.is_empty() {
            blocked = true;
            let names: Vec<String> = rc.running.iter().map(|s| format!("{} ({})", s.title, s.tool.label())).collect();
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "실행 중인 세션 {}개: {} · 먼저 종료해 주세요",
                        rc.running.len(),
                        names.join(", ")
                    ))
                    .font(font(12.5))
                    .color(c::ROSE),
                )
                .wrap(),
            );
        }
        if !unsaved.is_empty() {
            blocked = true;
            let names: Vec<&str> = unsaved.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect();
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "저장하지 않은 문서 {}개: {} · 먼저 저장하거나 닫아 주세요",
                        unsaved.len(),
                        names.join(", ")
                    ))
                    .font(font(12.5))
                    .color(c::ROSE),
                )
                .wrap(),
            );
        }
        let dirty = rc.dirty.len() as u32 + rc.untracked;
        if dirty > 0 {
            ui.label(
                RichText::new(format!(
                    "커밋하지 않은 변경 {}개 · 추적하지 않는 파일 {}개",
                    rc.dirty.len(),
                    rc.untracked
                ))
                .font(font(12.5))
                .color(c::YELLOW),
            );
        }
        match rc.unpushed {
            Some(0) => {}
            Some(n) => {
                let to = rc.upstream.clone().unwrap_or_else(|| "출발 기준".into());
                ui.label(
                    RichText::new(format!("{to}에 없는 커밋 {n}개 · 폴더를 지워도 branch에는 남아요"))
                        .font(font(12.5))
                        .color(c::YELLOW),
                );
            }
            None => {
                ui.label(RichText::new("올리지 않은 커밋 수 확인 불가").font(font(12.5)).color(c::TEXT_3));
            }
        }
        let mut f = force;
        if dirty > 0
            && ui.checkbox(&mut f, RichText::new("커밋하지 않은 변경도 함께 지울게요").font(font(12.5))).changed()
        {
            acts.push(Act::Force(f));
        }
        ui.horizontal(|ui| {
            let ok = !busy && !blocked && (dirty == 0 || force);
            if ui
                .add_enabled(
                    ok,
                    egui::Button::new(RichText::new("폴더 제거").font(medium(13.0)).color(c::ROSE))
                        .fill(c::ROSE_BG)
                        .min_size(egui::vec2(0.0, 28.0)),
                )
                .clicked()
            {
                acts.push(Act::Remove);
            }
            if theme::secondary_button(ui, "취소").clicked() {
                acts.push(Act::CancelRemove);
            }
        });
    });
}

fn conflict_prompt(name: &str, files: &[String]) -> String {
    format!(
        "{name} 공간의 branch를 기본 공간에 통합하다가 충돌이 났어요. 아래 파일의 충돌 표시(<<<<<<< ======= >>>>>>>)를 양쪽 의도를 살려 정리하고, 관련 테스트를 실행해 주세요. 커밋과 push는 하지 마세요. 끝나면 runwick report로 결과를 남겨 주세요.\n\n충돌 파일:\n{}",
        files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    )
}

fn apply(app: &mut App, ws: WsKey, me: &Workspace, target: Option<&Workspace>, a: Act) {
    let host = ws.host;
    let target_key = target.map(|t| WsKey { host, id: t.id });
    match a {
        Act::Merge => {
            app.feat.split.merge.busy = true;
            app.hub.call::<WorktreeMerge>(host, ById { id: ws.id }, |app, res| {
                app.feat.split.merge.busy = false;
                match res {
                    Ok(o) => app.toast_info(&o.message),
                    Err(e) => app.toast_error(&e.message),
                }
            });
        }
        Act::Abort => {
            app.feat.split.merge.busy = true;
            app.hub.call::<WorktreeMergeAbort>(host, ById { id: ws.id }, |app, res| {
                app.feat.split.merge.busy = false;
                match res {
                    Ok(()) => app.toast_info("통합을 취소하고 이전 상태로 돌렸어요"),
                    Err(e) => app.toast_error(&e.message),
                }
            });
        }
        Act::Delegate(files) => {
            let Some(t) = target else {
                return app.toast_error("기본 공간을 찾을 수 없어요");
            };
            let (cols, rows) = app.shell.center.terminal_grid();
            let d = Delegate {
                parent_id: None,
                workspace_id: Some(t.id),
                tool: Tool::Claude,
                role: Some("충돌 해결".into()),
                title: Some(format!("통합 충돌 해결: {}", me.name)),
                prompt: conflict_prompt(&me.name, &files),
                split: None,
                key: None,
                source: Source::Person,
                cols,
                rows,
            };
            let (project, key) = (t.project_id, WsKey { host, id: t.id });
            app.hub.call::<CollabDelegate>(host, d, move |app, res| match res {
                Ok(d) => {
                    app.toast_info(&format!("충돌 해결을 맡겼어요 ({})", d.request.receipt()));
                    actions::open_session(app, project, key, d.worker.session.id, false);
                }
                Err(e) => app.toast_error(&e.message),
            });
        }
        Act::OpenFile(path) => {
            if let (Some(t), Some(key)) = (target, target_key) {
                actions::select_workspace(app, t.project_id, key);
                crate::views::docs::open(app, key, &path, false);
            }
        }
        Act::OpenTarget => {
            if let (Some(t), Some(key)) = (target, target_key) {
                actions::select_workspace(app, t.project_id, key);
            }
        }
        Act::Archive => {
            let upd = WorkspaceUpdate { id: ws.id, archived: Some(true), ..Default::default() };
            let name = me.name.clone();
            app.hub.call::<WorkspacesUpdate>(host, upd, move |app, res| match res {
                Ok(_) => app.toast_info(&format!("{name} 공간을 목록에서 접었어요. 폴더와 기록은 그대로예요")),
                Err(e) => app.toast_error(&e.message),
            });
        }
        Act::AskRemove => {
            app.feat.split.merge.removing = Some(ws);
            app.feat.split.merge.force = false;
        }
        Act::CancelRemove => app.feat.split.merge.removing = None,
        Act::Force(f) => app.feat.split.merge.force = f,
        Act::Remove => {
            app.feat.split.merge.busy = true;
            let force = app.feat.split.merge.force;
            let back = target.map(|t| (t.project_id, WsKey { host, id: t.id }));
            app.hub.call::<WorktreeRemove>(host, Remove { workspace_id: ws.id, force }, move |app, res| {
                app.feat.split.merge.busy = false;
                app.feat.split.merge.removing = None;
                match res {
                    Ok(()) => {
                        app.toast_info("폴더를 제거했어요. 대화 기록과 결과는 남아 있어요");
                        if let Some(id) = app
                            .ui
                            .tabset(&ws)
                            .and_then(|s| s.tabs.iter().find(|t| t.kind == TabKind::Merge { ws }).map(|t| t.id))
                        {
                            app.ui.close_tab(&ws, id);
                        }
                        if let Some((project, key)) = back {
                            actions::select_workspace(app, project, key);
                        }
                    }
                    Err(e) => app.toast_error(&e.message),
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_window_keeps_markers_and_context() {
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!("line {i}\n"));
        }
        text.push_str("<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\n");
        for i in 0..20 {
            text.push_str(&format!("tail {i}\n"));
        }
        let w = conflict_window(&text);
        assert_eq!(w[0], "⋯");
        assert!(w.contains(&"<<<<<<< ours") && w.contains(&">>>>>>> theirs") && w.contains(&"line 19"));
        assert!(!w.contains(&"line 0") && w.last() == Some(&"⋯"));
        assert!(conflict_prompt("x", &["a.rs".into()]).contains("- a.rs"));
    }
}
