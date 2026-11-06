//! Wiki dialogs: 새 문서 (also started from a message or pin with its words quoted), 선택한
//! 세션으로 초안 만들기, and importing Markdown files (read on this computer and sent as text, so
//! it works for wikis on other computers too).

use super::{Ctx, Seed};
use crate::App;
use crate::shell::{dialog, dialog_title};
use crate::state::ToastKind;
use crate::theme::{self, c, font, ty};
use egui::RichText;
use runwick_core::api::jobs::{JobKind, JobTarget, JobsRequest, RequestJob};
use runwick_core::api::sessions::{SessionQuery, SessionsList, Tool};
use runwick_core::api::wiki::*;
use std::collections::BTreeSet;
use uuid::Uuid;

pub(super) enum Dialog {
    New {
        cx: Ctx,
        title: String,
        tags: String,
        busy: bool,
        focus: bool,
        /// Words from the records the page starts with.
        seed: Option<Seed>,
    },
    Draft {
        cx: Ctx,
        chosen: BTreeSet<Uuid>,
        busy: bool,
    },
}

pub(super) fn open_new(app: &mut App, cx: &Ctx) {
    app.feat.wiki.dialog = Some(Dialog::New {
        cx: cx.clone(),
        title: String::new(),
        tags: String::new(),
        busy: false,
        focus: true,
        seed: None,
    });
}

/// 새 문서 with words from the records quoted under the title.
pub(super) fn open_seeded(app: &mut App, cx: &Ctx, seed: Seed) {
    app.feat.wiki.dialog = Some(Dialog::New {
        cx: cx.clone(),
        title: seed.title.clone(),
        tags: String::new(),
        busy: false,
        focus: true,
        seed: Some(seed),
    });
}

pub(super) fn open_draft(app: &mut App, cx: &Ctx) {
    app.feat.wiki.dialog = Some(Dialog::Draft { cx: cx.clone(), chosen: BTreeSet::new(), busy: false });
}

pub(super) fn import_files(app: &mut App, cx: &Ctx) {
    let Some(paths) = rfd::FileDialog::new()
        .set_title("위키로 가져올 Markdown 파일")
        .add_filter("Markdown", &["md", "markdown", "txt"])
        .pick_files()
    else {
        return;
    };
    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match std::fs::read(&p).map(String::from_utf8) {
            Ok(Ok(text)) => files.push(ImportFile { name, text }),
            _ => unreadable.push(name),
        }
    }
    if !unreadable.is_empty() {
        app.toast_error(&format!("UTF-8 텍스트가 아니라 읽지 못했어요: {}", unreadable.join(", ")));
    }
    if files.is_empty() {
        return;
    }
    app.hub.call::<WikiImport>(cx.host, ImportPages { project_id: cx.project, files }, |app, res| match res {
        Ok(r) => {
            if !r.created.is_empty() {
                app.toast(ToastKind::Success, format!("문서 {}개를 가져왔어요", r.created.len()));
            }
            for (name, why) in r.skipped {
                app.toast_info(&format!("{name}: {why}"));
            }
        }
        Err(e) => app.toast_error(&e.message),
    });
}

/// Draw the open dialog, if any.
pub(super) fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.feat.wiki.dialog.take() else {
        return;
    };
    let keep = match &mut d {
        Dialog::New { cx, title, tags, busy, focus, seed } => {
            new_page(app, ctx, cx, NewPage { title, tags, busy, focus, seed: seed.as_ref() })
        }
        Dialog::Draft { cx, chosen, busy } => draft(app, ctx, cx, chosen, busy),
    };
    if keep && app.feat.wiki.dialog.is_none() {
        app.feat.wiki.dialog = Some(d);
    }
}

/// The fields of the 새 문서 dialog being edited.
struct NewPage<'a> {
    title: &'a mut String,
    tags: &'a mut String,
    busy: &'a mut bool,
    focus: &'a mut bool,
    seed: Option<&'a Seed>,
}

fn new_page(app: &mut App, ctx: &egui::Context, cx: &Ctx, f: NewPage<'_>) -> bool {
    let NewPage { title, tags, busy, focus, seed } = f;
    let mut create = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "wiki-new", 440.0, |ui| {
        let note = match seed {
            Some(_) => {
                format!("{}의 위키에 고른 기록을 인용한 문서를 만들어요. 만든 뒤 바로 고칠 수 있어요.", cx.project_name)
            }
            None => format!("{}의 위키에 직접 쓰는 문서예요. AI는 이 글을 바로 고치지 않아요.", cx.project_name),
        };
        dialog_title(ui, "새 문서", Some(&note));
        let resp = ui.add(
            egui::TextEdit::singleline(title).hint_text("문서 제목").font(font(14.0)).desired_width(f32::INFINITY),
        );
        if std::mem::take(focus) {
            resp.request_focus();
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            create = true;
        }
        ui.add(
            egui::TextEdit::singleline(tags)
                .hint_text("태그 (쉼표로 구분, 선택)")
                .font(font(13.0))
                .desired_width(f32::INFINITY),
        );
        if let Some(seed) = seed {
            seed_preview(ui, seed);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ok = !title.trim().is_empty() && !*busy && cx.connected;
            if ui.add_enabled_ui(ok, |ui| theme::primary_button(ui, "만들기")).inner.clicked() {
                create = true;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                cancel = true;
            }
        });
    });
    if close || cancel {
        return false;
    }
    if create && !title.trim().is_empty() && !*busy {
        *busy = true;
        let tab = cx.tab;
        let host = cx.host;
        let req = CreatePage {
            project_id: cx.project,
            title: title.trim().to_string(),
            body: format!("# {}\n\n{}", title.trim(), seed.map_or("", |s| s.body.as_str())),
            tags: tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            sources: seed.and_then(|s| s.session).into_iter().collect(),
        };
        app.hub.call::<WikiCreate>(cx.host, req, move |app, res| {
            app.feat.wiki.dialog = None;
            match res {
                Ok(page) => {
                    let st = app.feat.wiki.tabs.entry(tab).or_default();
                    st.page = Some(page.summary.slug.clone());
                    st.review = None;
                    st.history = None;
                    let key = (host, page.summary.project_id, page.summary.slug.clone());
                    app.feat.wiki.edits.insert(key, super::page::Edit::from(&page));
                }
                Err(e) => app.toast_error(&e.message),
            }
        });
    }
    true
}

/// The quoted words (first lines) and their source, as they will go into the page.
fn seed_preview(ui: &mut egui::Ui, seed: &Seed) {
    egui::Frame::new()
        .fill(c::ROW)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            let quote: String = seed
                .body
                .lines()
                .filter_map(|l| l.strip_prefix('>'))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            let mut job = egui::text::LayoutJob::simple(quote, font(12.5), c::TEXT_2, ui.available_width());
            job.wrap.max_rows = 4;
            ui.add(egui::Label::new(job).wrap());
            theme::truncated(ui, format!("출처 · {}", seed.source), font(ty::SMALL), c::TEXT_3);
        });
}

fn draft(app: &mut App, ctx: &egui::Context, cx: &Ctx, chosen: &mut BTreeSet<Uuid>, busy: &mut bool) -> bool {
    let q =
        SessionQuery { project_id: Some(cx.project), workspace_id: None, include_hidden: true, include_archived: true };
    let sessions = app.hub.query::<SessionsList>(cx.host, &q);
    let mut go = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "wiki-draft", 520.0, |ui| {
        dialog_title(
            ui,
            "세션으로 초안 만들기",
            Some(
                "고른 세션의 대화 기록으로 새 위키 문서 초안을 만들어요. 초안은 검토하기 전까지 ‘AI 초안’으로 표시돼요.",
            ),
        );
        match (&sessions.data, &sessions.error) {
            (Some(list), _) => {
                let agents: Vec<_> = list.iter().filter(|s| s.session.tool != Tool::Shell).collect();
                if agents.is_empty() {
                    ui.label(
                        RichText::new("이 프로젝트에는 아직 Claude Code나 Codex 세션이 없어요.")
                            .font(font(12.5))
                            .color(c::TEXT_3),
                    );
                }
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for s in agents {
                        let mut on = chosen.contains(&s.session.id);
                        let label = format!(
                            "{} · {}",
                            s.session.title,
                            super::when(s.session.last_active_at.unwrap_or(s.session.created_at))
                        );
                        if ui.checkbox(&mut on, RichText::new(label).font(font(13.0)).color(c::TEXT)).changed() {
                            if on {
                                chosen.insert(s.session.id);
                            } else {
                                chosen.remove(&s.session.id);
                            }
                        }
                    }
                });
            }
            (None, Some(e)) => super::home::error_text(ui, &e.message),
            (None, None) => super::home::loading(ui, "세션을 불러오는 중…"),
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("구독 사용량을 씁니다 · 설치된 Claude Code(없으면 Codex)를 내 계정으로 실행해요. 배터리에서도 바로 실행해요.")
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ok = !chosen.is_empty() && !*busy && cx.connected;
            let label = if chosen.is_empty() {
                "초안 만들기".to_string()
            } else {
                format!("세션 {}개로 초안 만들기", chosen.len())
            };
            if ui.add_enabled_ui(ok, |ui| theme::primary_button(ui, &label)).inner.clicked() {
                go = true;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                cancel = true;
            }
        });
    });
    if close || cancel {
        return false;
    }
    if go && !chosen.is_empty() && !*busy {
        *busy = true;
        let req = RequestJob {
            kind: JobKind::WikiDraft,
            project_id: cx.project,
            target: JobTarget { day: None, sessions: chosen.iter().copied().collect() },
        };
        app.hub.call::<JobsRequest>(cx.host, req, |app, res| {
            app.feat.wiki.dialog = None;
            match res {
                Ok(_) => app.toast_info("초안을 만들고 있어요. 끝나면 검토할 것에 보여요"),
                Err(e) => app.toast_error(&e.message),
            }
        });
    }
    true
}
