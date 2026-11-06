//! 새 문서: a name, Markdown or plain text, in the chosen folder. The host never replaces an
//! existing file; a clash is explained in the dialog.

use crate::App;
use crate::shell::data;
use crate::state::WsKey;
use crate::theme::{self, Seg, c, font, mono};
use egui::{Align, Layout, RichText, Vec2};
use runwick_core::api::files::{CreateFile, FilesCreate, NewDocKind, display_path};

pub struct Dialog {
    ws: WsKey,
    dir: String,
    name: String,
    kind: NewDocKind,
    busy: bool,
    error: Option<String>,
    focus: bool,
}

impl Dialog {
    pub fn new(ws: WsKey, dir: String) -> Self {
        Self { ws, dir, name: String::new(), kind: NewDocKind::Md, busy: false, error: None, focus: true }
    }
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.feat.files.new_doc.take() else {
        return;
    };
    let ws_name = data::workspaces(app)
        .data
        .and_then(|l| data::find_ws(&l, &d.ws).map(|w| w.name.clone()))
        .unwrap_or_else(|| "작업 공간".into());
    let composing = app.shell.keys.composing;
    let mut submit = false;
    let mut cancel = false;
    let (_, close) = crate::shell::dialog(ctx, "files-new-doc", 440.0, |ui| {
        let place = if d.dir.is_empty() { "맨 위 폴더".to_string() } else { format!("{}/", display_path(&d.dir)) };
        crate::shell::dialog_title(ui, "새 문서", Some(&format!("{ws_name} · 위치: {place}")));
        let id = egui::Id::unique("files-new-doc-name");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut d.name)
                .id(id)
                .hint_text(RichText::new("예: 회의 메모").color(c::TEXT_3))
                .font(font(13.5))
                .desired_width(f32::INFINITY)
                .min_size(Vec2::new(0.0, 30.0))
                .align(egui::Align2::LEFT_CENTER),
        );
        if std::mem::take(&mut d.focus) {
            resp.request_focus();
        }
        if resp.changed() {
            d.error = None;
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !composing {
            submit = true;
        }
        ui.horizontal(|ui| {
            let sel = match d.kind {
                NewDocKind::Md => 0,
                NewDocKind::Txt => 1,
            };
            if let Some(i) = theme::segmented(ui, &[Seg::text("Markdown (.md)"), Seg::text("텍스트 (.txt)")], sel, 26.0)
            {
                d.kind = if i == 0 { NewDocKind::Md } else { NewDocKind::Txt };
            }
        });
        let ext = d.kind.extension();
        let trimmed = d.name.trim();
        if !trimmed.is_empty() {
            let file = if trimmed.to_lowercase().ends_with(&format!(".{ext}")) {
                trimmed.to_string()
            } else {
                format!("{trimmed}.{ext}")
            };
            let full = if d.dir.is_empty() { file } else { format!("{}/{file}", d.dir) };
            ui.label(RichText::new(format!("만들 파일: {}", display_path(&full))).font(mono(11.5)).color(c::TEXT_3));
        }
        if let Some(err) = &d.error {
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::icon(crate::icons::CIRCLE_ALERT, 12.0, theme::Tone::Recover.fg()));
                ui.label(RichText::new(err).font(font(12.5)).color(theme::Tone::Recover.fg()));
            });
        }
        ui.add_space(4.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ready = !d.name.trim().is_empty() && !d.busy;
            let label = if d.busy { "만드는 중…" } else { "만들기" };
            if ui.add_enabled_ui(ready, |ui| theme::primary_button(ui, label)).inner.clicked() {
                submit = true;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                cancel = true;
            }
        });
    });
    if cancel || (close && !d.busy) {
        return;
    }
    if submit && !d.busy && !d.name.trim().is_empty() {
        d.busy = true;
        let ws = d.ws;
        let dir = d.dir.clone();
        let params = CreateFile { workspace_id: ws.id, rel_dir: dir.clone(), name: d.name.clone(), kind: d.kind };
        app.hub.call::<FilesCreate>(ws.host, params, move |app, res| match res {
            Ok(created) => {
                app.feat.files.new_doc = None;
                if !dir.is_empty() {
                    app.feat.files.expanded.entry(ws).or_default().insert(dir);
                }
                crate::views::docs::open(app, ws, &created.path, false);
                app.toast(crate::state::ToastKind::Success, "새 문서를 만들었어요");
            }
            Err(e) => {
                if let Some(d) = app.feat.files.new_doc.as_mut() {
                    d.busy = false;
                    d.error = Some(e.message);
                    d.focus = true;
                }
            }
        });
    }
    app.feat.files.new_doc = Some(d);
}
