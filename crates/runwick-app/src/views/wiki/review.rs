//! AI change proposals: the 이전/제안 diff, its basis, and 적용 · 부분 적용 · 보류 · 거절.
//! Hunks come from the host already re-compared with the page's current text; hunks that
//! overlap the person's later edits are shown but can never be applied.

use super::{Ctx, TabState, when};
use crate::App;
use crate::icons;
use crate::theme::{self, Tone, c, font, medium, semibold, ty};
use egui::{Color32, CornerRadius, RichText};
use runwick_core::api::wiki::*;
use std::collections::BTreeSet;

pub(super) const DEL_BG: Color32 = Color32::from_rgb(0x2E, 0x15, 0x18);
pub(super) const DEL_FG: Color32 = Color32::from_rgb(0xFF, 0xC2, 0xC2);
pub(super) const ADD_BG: Color32 = Color32::from_rgb(0x10, 0x28, 0x1C);
pub(super) const ADD_FG: Color32 = Color32::from_rgb(0xBD, 0xF2, 0xD2);

/// One diff line on its tinted ground.
pub(super) fn diff_line(ui: &mut egui::Ui, kind: LineKind, text: &str) {
    let (bg, fg, mark) = match kind {
        LineKind::Added => (ADD_BG, ADD_FG, "+"),
        LineKind::Removed => (DEL_BG, DEL_FG, "−"),
        LineKind::Same => (Color32::TRANSPARENT, c::TEXT_3, " "),
    };
    egui::Frame::new().fill(bg).inner_margin(egui::Margin::symmetric(12, 2)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(RichText::new(mark).font(crate::theme::mono(12.5)).color(fg));
            let t = if text.is_empty() { " " } else { text };
            ui.add(egui::Label::new(RichText::new(t).font(font(13.5)).color(fg)).wrap());
        });
    });
}

fn hunk_view(ui: &mut egui::Ui, h: &Hunk, chosen: Option<&mut BTreeSet<u32>>) {
    let clean = h.kind == HunkKind::Clean;
    ui.spacing_mut().item_spacing.y = 0.0;
    let pick = chosen.is_some() && clean;
    if pick || !clean {
        hunk_head(ui, h, chosen);
    }
    for l in h.context_before.iter().filter(|l| !l.trim().is_empty()) {
        diff_line(ui, LineKind::Same, l);
    }
    if clean {
        for l in &h.old {
            diff_line(ui, LineKind::Removed, l);
        }
        for l in &h.new {
            diff_line(ui, LineKind::Added, l);
        }
    } else {
        ui.add_space(4.0);
        ui.label(RichText::new("지금 문서").font(font(11.5)).color(c::TEXT_3));
        for l in &h.old {
            diff_line(ui, LineKind::Same, l);
        }
        ui.add_space(4.0);
        ui.label(RichText::new("AI 제안 (참고만)").font(font(11.5)).color(c::TEXT_3));
        for l in &h.new {
            diff_line(ui, LineKind::Same, l);
        }
    }
    ui.add_space(10.0);
}

/// Checkbox for 부분 적용, or the note that the hunk overlaps the person's edit.
fn hunk_head(ui: &mut egui::Ui, h: &Hunk, chosen: Option<&mut BTreeSet<u32>>) {
    let clean = h.kind == HunkKind::Clean;
    egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            if let Some(set) = chosen
                && clean
            {
                let mut on = set.contains(&h.index);
                let label = RichText::new(format!("변경 {}", h.index + 1)).font(font(ty::SMALL)).color(c::TEXT_2);
                if ui.checkbox(&mut on, label).changed() {
                    if on {
                        set.insert(h.index);
                    } else {
                        set.remove(&h.index);
                    }
                }
            }
            if !clean {
                theme::chip(ui, Tone::Check, "내가 고친 부분과 겹쳐요 · 넣지 않아요");
            }
        });
    });
}

/// Sources and the decision check under a proposal.
fn basis(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, p: &Proposal) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("근거").font(font(12.5)).color(c::TEXT_3));
            ui.label(RichText::new(&p.rationale).font(font(12.5)).color(c::TEXT_2));
        });
        if !p.sources.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("원문").font(font(12.5)).color(c::TEXT_3));
                for s in p.sources.iter().take(6) {
                    let title = s.session_title.clone().unwrap_or_else(|| "세션".into());
                    let label = match s.at {
                        Some(at) => format!("{title} · {} 메시지", when(at)),
                        None => title,
                    };
                    let id = s.message_id.as_deref().and_then(|m| m.parse::<uuid::Uuid>().ok());
                    let resp = crate::views::history::fmt::quiet_link(ui, &label);
                    if resp.clicked() {
                        match id {
                            Some(m) => crate::views::history::open_message(app, cx.host, m, Vec::new()),
                            None => crate::views::history::open_session_records(app, cx.host, s.session_id),
                        }
                    }
                }
            });
        }
        if let Some(flag) = &p.decision_flag {
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::icon(icons::CIRCLE_HELP, 12.0, c::YELLOW));
                ui.label(RichText::new("결정 확인 필요").font(medium(12.5)).color(c::YELLOW));
                ui.label(RichText::new(flag).font(font(12.5)).color(c::YELLOW));
            });
        }
    });
}

/// A proposal against an existing page.
pub(super) fn card(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, p: &Proposal) {
    let detail = app.hub.query::<WikiProposalGet>(cx.host, &ProposalAction { id: p.id });
    egui::Frame::new()
        .fill(c::PANEL)
        .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    theme::chip_with_icon(ui, Tone::Info, Some(icons::SPARKLES), "AI 변경 제안");
                    if p.needs_rebase {
                        theme::chip_with_icon(ui, Tone::Check, Some(icons::REFRESH_CW), "재비교 필요");
                    }
                    if p.status == ProposalStatus::Held {
                        theme::chip(ui, Tone::Waiting, "보류함");
                    }
                    ui.label(
                        RichText::new(format!("{} 생성 · 내가 고친 부분은 건드리지 않아요", when(p.created_at)))
                            .font(font(12.0))
                            .color(c::TEXT_3),
                    );
                });
                if p.needs_rebase {
                    let v = detail
                        .data
                        .as_ref()
                        .and_then(|d| d.against_version)
                        .map(|v| format!("(v{v})"))
                        .unwrap_or_default();
                    ui.label(
                        RichText::new(format!(
                            "제안을 만든 뒤 문서가 바뀌어 지금 문서{v}와 다시 비교했어요. 겹치는 부분은 넣지 않아요."
                        ))
                        .font(font(12.0))
                        .color(c::YELLOW),
                    );
                }
            });
            ui.separator();
            let partial = st.partial.get_mut(&p.id);
            match (&detail.data, &detail.error) {
                (Some(d), _) => {
                    if d.hunks.is_empty() {
                        egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                            ui.label(
                                RichText::new("지금 문서에 이미 반영된 내용이에요. 바꿀 부분이 없어요.")
                                    .font(font(12.5))
                                    .color(c::TEXT_3),
                            );
                        });
                    } else {
                        egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 8)).show(ui, |ui| {
                            let mut partial = partial;
                            for h in &d.hunks {
                                hunk_view(ui, h, partial.as_deref_mut());
                            }
                        });
                    }
                }
                (None, Some(e)) => {
                    egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                        super::home::error_text(ui, &format!("제안을 비교하지 못했어요: {}", e.message));
                    });
                }
                (None, None) => {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(14, 8))
                        .show(ui, |ui| super::home::loading(ui, "지금 문서와 비교하는 중…"));
                }
            }
            ui.separator();
            basis(app, ui, cx, p);
            actions(app, ui, cx, st, p, detail.data.as_deref());
        });
}

fn actions(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, p: &Proposal, d: Option<&ProposalDetail>) {
    egui::Frame::new()
        .fill(c::PANEL_2)
        .corner_radius(CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 })
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let busy = st.busy.contains(&p.id);
            let clean: Vec<u32> = d
                .map(|d| d.hunks.iter().filter(|h| h.kind == HunkKind::Clean).map(|h| h.index).collect())
                .unwrap_or_default();
            ui.add_enabled_ui(!busy && cx.connected && d.is_some(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    if let Some(chosen) = st.partial.get(&p.id).cloned() {
                        let label = format!("고른 {}개 적용", chosen.len());
                        if ui.add_enabled_ui(!chosen.is_empty(), |ui| theme::primary_button(ui, &label)).inner.clicked()
                        {
                            apply(app, cx, p.id, d.and_then(|d| d.against_version), Some(chosen.into_iter().collect()));
                        }
                        if theme::secondary_button(ui, "부분 적용 취소").clicked() {
                            st.partial.remove(&p.id);
                        }
                        return;
                    }
                    let apply_label = if p.slug.is_none() { "새 문서로 적용" } else { "적용" };
                    if ui.add_enabled_ui(!clean.is_empty(), |ui| theme::primary_button(ui, apply_label)).inner.clicked()
                    {
                        apply(app, cx, p.id, d.and_then(|d| d.against_version), None);
                    }
                    if p.slug.is_some()
                        && clean.len() > 1
                        && theme::secondary_button(ui, "부분 적용").on_hover_text("넣을 변경만 골라요").clicked()
                    {
                        st.partial.insert(p.id, clean.iter().copied().collect());
                    }
                    if p.status == ProposalStatus::Open && theme::secondary_button(ui, "보류").clicked() {
                        decide(app, cx, p.id, false);
                    }
                    if theme::secondary_button(ui, "거절").clicked() {
                        decide(app, cx, p.id, true);
                    }
                    if busy {
                        ui.add(egui::Spinner::new().size(12.0));
                    }
                });
            });
            if !cx.connected {
                ui.label(RichText::new("연결되면 적용할 수 있어요").font(font(ty::SMALL)).color(c::TEXT_3));
            }
        });
}

fn set_busy(app: &mut App, tab: crate::state::TabId, id: ProposalId, on: bool) {
    if let Some(st) = app.feat.wiki.tabs.get_mut(&tab) {
        if on {
            st.busy.insert(id);
        } else {
            st.busy.remove(&id);
            st.partial.remove(&id);
        }
    }
}

fn apply(app: &mut App, cx: &Ctx, id: ProposalId, against: Option<u32>, hunks: Option<Vec<u32>>) {
    set_busy(app, cx.tab, id, true);
    let tab = cx.tab;
    let req = ApplyProposal { id, against_version: against, hunks };
    app.hub.call::<WikiApply>(cx.host, req, move |app, res| {
        set_busy(app, tab, id, false);
        match res {
            Ok(page) => {
                if let Some(st) = app.feat.wiki.tabs.get_mut(&tab)
                    && st.review == Some(id)
                {
                    st.review = None;
                    st.page = Some(page.summary.slug.clone());
                }
                app.toast(crate::state::ToastKind::Success, format!("제안을 적용했어요 · v{}", page.summary.version));
            }
            Err(e) => app.toast_error(&e.message),
        }
    });
}

fn decide(app: &mut App, cx: &Ctx, id: ProposalId, reject: bool) {
    set_busy(app, cx.tab, id, true);
    let tab = cx.tab;
    let done = move |app: &mut App, res: Result<Proposal, runwick_core::RpcError>| {
        set_busy(app, tab, id, false);
        match res {
            Ok(p) => {
                if let Some(st) = app.feat.wiki.tabs.get_mut(&tab)
                    && st.review == Some(id)
                    && p.status == ProposalStatus::Rejected
                {
                    st.review = None;
                }
                app.toast_info(if p.status == ProposalStatus::Rejected {
                    "제안을 거절했어요"
                } else {
                    "제안을 보류했어요"
                });
            }
            Err(e) => app.toast_error(&e.message),
        }
    };
    if reject {
        app.hub.call::<WikiReject>(cx.host, ProposalAction { id }, done);
    } else {
        app.hub.call::<WikiHold>(cx.host, ProposalAction { id }, done);
    }
}

/// Reviewing a proposal for a new page (from the list's review queue).
pub(super) fn show_new_page(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState) {
    let Some(id) = st.review else { return };
    let detail = app.hub.query::<WikiProposalGet>(cx.host, &ProposalAction { id });
    egui::ScrollArea::vertical().id_salt(("wiki-review", cx.tab)).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 28, top: 22, bottom: 28 }).show(ui, |ui| {
            let w = ui.available_width().min(720.0);
            let pad = ((ui.available_width() - w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    if crate::views::history::fmt::quiet_link(ui, "← 모든 문서").clicked() {
                        st.review = None;
                        return;
                    }
                    match (&detail.data, &detail.error) {
                        (Some(d), _) => {
                            let p = d.proposal.clone();
                            ui.label(RichText::new(&p.title).font(semibold(24.0)).color(c::TEXT_STRONG));
                            ui.label(
                                RichText::new("AI가 제안한 새 문서예요. 적용하기 전에는 위키에 생기지 않아요.")
                                    .font(font(12.5))
                                    .color(c::TEXT_3),
                            );
                            ui.add_space(10.0);
                            if p.status.is_closed() {
                                ui.label(RichText::new("이미 처리한 제안이에요.").font(font(12.5)).color(c::TEXT_3));
                                return;
                            }
                            egui::Frame::new()
                                .fill(c::PANEL)
                                .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
                                .corner_radius(CornerRadius::same(12))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    egui::Frame::new().inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        let md = super::page::preview_markdown(&d.proposed_body);
                                        egui_commonmark::CommonMarkViewer::new().show(ui, &mut app.feat.wiki.md, &md);
                                    });
                                    ui.separator();
                                    basis(app, ui, cx, &p);
                                    actions(app, ui, cx, st, &p, Some(d));
                                });
                        }
                        (None, Some(e)) => super::home::error_text(ui, &format!("제안을 열지 못했어요: {}", e.message)),
                        (None, None) => super::home::loading(ui, "제안을 불러오는 중…"),
                    }
                });
            });
        });
    });
}
