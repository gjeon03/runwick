//! "새 대화에 인계": start a new session in the project's workspace whose first message is
//! composed from chosen pins, the memo, remaining items and file paths — previewed and
//! editable before sending. It says plainly that this is a handover, not a resumed
//! conversation.

use super::fmt;
use crate::App;
use crate::client::HostKey;
use crate::shell::{actions, data};
use crate::state::WsKey;
use crate::theme::{self, Seg, c, font, medium, semibold, ty};
use egui::{CornerRadius, Margin, RichText};
use runwick_core::api::projects::{ProjectId, WorkspaceId, WorkspaceKind};
use runwick_core::api::records::{Pin, PinId};
use runwick_core::api::sessions::{SessionsStart, StartSession, Tool};
use runwick_core::api::timeline::{NoteItem, Resume};
use std::collections::BTreeSet;

pub struct Draft {
    pub project: ProjectId,
    tool: Tool,
    pins: BTreeSet<PinId>,
    memo: bool,
    items: bool,
    files: String,
    /// The message to send (editable).
    text: String,
    /// What was generated last; while `text` equals it, choices regenerate it.
    generated: String,
    sending: bool,
}

pub fn open(app: &mut App, r: &Resume) {
    let tool = r.last.as_ref().map(|l| l.session.tool).filter(|t| t.is_agent()).unwrap_or(Tool::Claude);
    let chosen = &app.feat.timeline.handover_pins;
    let pins = r.pins.iter().filter(|p| chosen.contains(&p.id)).map(|p| p.id).collect();
    app.feat.timeline.handover = Some(Draft {
        project: r.project_id,
        tool,
        pins,
        memo: !r.note.memo.trim().is_empty(),
        items: r.note.items.iter().any(|i| !i.done),
        files: String::new(),
        text: String::new(),
        generated: String::new(),
        sending: false,
    });
}

/// The first message of the new conversation.
pub fn compose(
    from: Option<(&str, Tool)>,
    memo: Option<&str>,
    items: &[NoteItem],
    pins: &[&Pin],
    files: &[&str],
) -> String {
    let mut s = String::from("이전 작업을 이어받는 새 대화입니다. (Runwick 인계 · 원래 대화를 재개한 것이 아닙니다)\n");
    if let Some((title, tool)) = from {
        s.push_str(&format!("이전 세션: \"{title}\" ({})\n", tool.label()));
    }
    if let Some(m) = memo.map(str::trim).filter(|m| !m.is_empty()) {
        s.push_str(&format!("\n## 메모\n{m}\n"));
    }
    if !items.is_empty() {
        s.push_str("\n## 남은 일\n");
        for i in items {
            s.push_str(&format!("- [{}] {}\n", if i.done { "x" } else { " " }, i.text));
        }
    }
    if !pins.is_empty() {
        s.push_str("\n## 참고 기록 (핀)\n");
        for p in pins {
            let (src, _) = crate::views::history::pins_source_line(p);
            s.push_str(&format!("\n### {}\n출처: {src}\n", p.title));
            for line in p.text.lines() {
                s.push_str(&format!("> {line}\n"));
            }
            if !p.note.trim().is_empty() {
                s.push_str(&format!("메모: {}\n", p.note.trim()));
            }
        }
    }
    if !files.is_empty() {
        s.push_str("\n## 관련 파일\n");
        for f in files {
            s.push_str(&format!("- {f}\n"));
        }
    }
    s.push_str("\n위 자료를 먼저 읽고, 이어서 할 일을 정리한 뒤 진행해 주세요.");
    s
}

/// Where the new session runs: the last session's workspace, else the project's default one.
fn target(app: &mut App, r: &Resume) -> Option<(WorkspaceId, String)> {
    if let Some(l) = r.last.as_ref().filter(|l| l.workspace_exists) {
        return Some((l.session.workspace_id, l.workspace_name.clone()));
    }
    let list = data::workspaces(app).data?;
    list.iter()
        .find(|w| {
            w.project_id == r.project_id && w.kind == WorkspaceKind::Default && w.path_exists && w.remote.is_none()
        })
        .map(|w| (w.id, w.name.clone()))
}

pub fn show(app: &mut App, ui: &mut egui::Ui, r: &Resume) {
    let dest = target(app, r);
    let Some(d) = app.feat.timeline.handover.as_mut() else {
        return;
    };
    let mut close = false;
    let mut send = None;
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(16)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.label(RichText::new("새 대화에 인계").font(semibold(15.0)).color(c::TEXT_STRONG));
        ui.add(
            egui::Label::new(
                RichText::new("원래 대화를 재개하지 않고, 고른 자료를 첫 메시지로 담아 새 세션을 시작해요. 보내기 전에 내용을 고칠 수 있어요.")
                    .font(font(ty::SMALL))
                    .color(c::TEXT_2),
            )
            .wrap(),
        );
        ui.horizontal(|ui| {
            fmt::label(ui, "CLI");
            let sel = if d.tool == Tool::Codex { 1 } else { 0 };
            if let Some(i) = theme::segmented(ui, &[Seg::text("Claude Code"), Seg::text("Codex")], sel, 24.0) {
                d.tool = if i == 1 { Tool::Codex } else { Tool::Claude };
            }
        });
        fmt::label(ui, "담을 핀");
        if r.pins.is_empty() {
            ui.label(RichText::new("고를 핀이 없어요. 기록에서 핀을 고정하면 여기에서 고를 수 있어요.").font(font(12.0)).color(c::TEXT_3));
        }
        egui::ScrollArea::vertical().id_salt("handover-pins").max_height(180.0).show(ui, |ui| {
            for p in &r.pins {
                let mut on = d.pins.contains(&p.id);
                let (src, warn) = crate::views::history::pins_source_line(p);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut on, "").changed() {
                        if on {
                            d.pins.insert(p.id);
                        } else {
                            d.pins.remove(&p.id);
                        }
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        theme::truncated(ui, p.title.clone(), medium(12.5), c::TEXT_STRONG);
                        theme::truncated(ui, src, font(11.0), if warn { c::YELLOW } else { c::TEXT_3 });
                    });
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled(!r.note.memo.trim().is_empty(), egui::Checkbox::new(&mut d.memo, "내 메모 포함"));
            ui.add_enabled(!r.note.items.is_empty(), egui::Checkbox::new(&mut d.items, "남은 일 포함"));
        });
        fmt::label(ui, "관련 파일 (한 줄에 하나)");
        ui.add(egui::TextEdit::multiline(&mut d.files).font(theme::mono(12.0)).desired_rows(2).desired_width(f32::INFINITY).hint_text("src/auth/session.ts"));

        let pins: Vec<&Pin> = r.pins.iter().filter(|p| d.pins.contains(&p.id)).collect();
        let items: Vec<NoteItem> = if d.items { r.note.items.clone() } else { Vec::new() };
        let files: Vec<&str> = d.files.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let from = r.last.as_ref().map(|l| (l.session.title.as_str(), l.session.tool));
        let fresh = compose(from, d.memo.then_some(r.note.memo.as_str()), &items, &pins, &files);
        if d.text == d.generated {
            d.text = fresh.clone();
        }
        d.generated = fresh;
        ui.horizontal(|ui| {
            fmt::label(ui, "보낼 첫 메시지");
            if d.text != d.generated && fmt::quiet_link(ui, "고른 내용으로 다시 만들기").clicked() {
                d.text = d.generated.clone();
            }
        });
        egui::ScrollArea::vertical().id_salt("handover-text").max_height(260.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut d.text).font(font(12.5)).desired_rows(8).desired_width(f32::INFINITY));
        });
        match &dest {
            Some((_, name)) => {
                ui.label(RichText::new(format!("{name} 공간에서 {}로 새 세션을 시작해요.", d.tool.label())).font(font(12.0)).color(c::TEXT_2));
            }
            None => {
                ui.horizontal_wrapped(|ui| {
                    theme::chip(ui, theme::Tone::Recover, "작업 폴더를 찾을 수 없어 새 대화를 시작할 수 없어요");
                });
            }
        }
        ui.horizontal(|ui| {
            let ok = dest.is_some() && !d.text.trim().is_empty() && !d.sending;
            let label = if d.sending { "시작하는 중…" } else { "새 대화 시작" };
            let b = egui::Button::new(RichText::new(label).font(medium(13.0)).color(if ok { c::ON_EMBER } else { c::TEXT_3 }))
                .fill(if ok { c::EMBER } else { c::CONTROL })
                .min_size(egui::vec2(0.0, 30.0));
            if ui.add_enabled(ok, b).clicked()
                && let Some((ws, _)) = &dest
            {
                d.sending = true;
                send = Some((*ws, d.tool, d.text.clone(), d.pins.clone()));
            }
            if theme::secondary_button(ui, "취소").clicked() {
                close = true;
            }
        });
    });
    if close {
        app.feat.timeline.handover = None;
    }
    if let Some((ws, tool, text, used)) = send {
        let project = r.project_id;
        let title = format!(
            "인계: {}",
            r.last.as_ref().map(|l| l.session.title.clone()).unwrap_or_else(|| r.project_name.clone())
        );
        let (cols, rows) = crate::terminal::grid_for(ui.ctx(), ui.max_rect().size());
        let params = StartSession {
            workspace_id: ws,
            tool,
            title: Some(title.chars().take(60).collect()),
            initial_prompt: Some(text),
            parent_id: None,
            role: None,
            cols,
            rows,
        };
        app.hub.call::<SessionsStart>(HostKey::Local, params, move |app, res| match res {
            Ok(info) => {
                app.feat.timeline.handover = None;
                for id in &used {
                    app.feat.timeline.handover_pins.remove(id);
                }
                actions::open_session(app, project, WsKey { host: HostKey::Local, id: ws }, info.session.id, false);
                app.toast(crate::state::ToastKind::Success, "인계 자료를 담아 새 대화를 시작했어요");
            }
            Err(e) => {
                if let Some(d) = app.feat.timeline.handover.as_mut() {
                    d.sending = false;
                }
                app.toast_error(&format!("새 대화를 시작하지 못했어요: {}", e.message));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runwick_core::api::records::PinKind;

    #[test]
    fn handover_says_it_is_not_a_resume_and_quotes_pins() {
        let now = runwick_core::now();
        let pin = Pin {
            id: uuid::Uuid::now_v7(),
            kind: PinKind::Terminal,
            title: "테스트 실패 로그".into(),
            note: "스테이징 확인".into(),
            text: "FAIL a\nexpected 1".into(),
            message_id: None,
            session_id: None,
            project_id: None,
            session_title: "로그인 오류 수정".into(),
            tool: Some(Tool::Claude),
            source_at: now,
            created_at: now,
            updated_at: now,
            source_available: false,
        };
        let items = [NoteItem { text: "통합".into(), done: false }, NoteItem { text: "재현".into(), done: true }];
        let s = compose(Some(("로그인 오류 수정", Tool::Claude)), Some("  e2e 아직  "), &items, &[&pin], &["src/a.ts"]);
        assert!(s.starts_with("이전 작업을 이어받는 새 대화입니다. (Runwick 인계 · 원래 대화를 재개한 것이 아닙니다)"));
        assert!(s.contains("이전 세션: \"로그인 오류 수정\" (Claude Code)"));
        assert!(s.contains("## 메모\ne2e 아직\n"));
        assert!(s.contains("- [ ] 통합\n- [x] 재현\n"));
        assert!(s.contains("> FAIL a\n> expected 1\n메모: 스테이징 확인"));
        assert!(s.contains("터미널에서 고정 · 로그인 오류 수정"));
        assert!(s.contains("## 관련 파일\n- src/a.ts\n"));
        let bare = compose(None, Some("  "), &[], &[], &[]);
        assert!(!bare.contains("##"));
    }
}
