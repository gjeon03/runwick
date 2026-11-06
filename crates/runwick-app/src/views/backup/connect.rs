//! Choosing the sync repository: a new private repository, one from the GitHub account, or a
//! pasted URL → the repository is cloned into a separate folder and read (preview: what is
//! there, what is new here, what will not come) → connect and first sync → folders to link.
//!
//! The same dialog serves "다른 컴퓨터의 기록 가져오기"; nothing is imported before the person
//! confirms the preview.

use super::{counts_table, status_line};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{dialog, dialog_title};
use crate::theme::{self, Tone, c, font, icon, medium, mono, semibold, ty};
use crate::views::history::fmt;
use egui::{CornerRadius, RichText, Vec2};
use runwick_core::api::sync::*;

const HOST: HostKey = HostKey::Local;

enum Step {
    Choose,
    /// Cloning and reading the repository.
    Checking(String),
    Preview(Box<ConnectPreview>),
    Connecting(String),
    Done(Box<SyncReport>),
}

pub struct Wizard {
    /// Opened as "다른 컴퓨터의 기록 가져오기" (restore from an existing repository).
    import: bool,
    step: Step,
    url: String,
    new_name: String,
    filter: String,
    creating: bool,
    error: Option<String>,
}

impl Wizard {
    pub fn new(import: bool) -> Self {
        Self {
            import,
            step: Step::Choose,
            url: String::new(),
            new_name: DEFAULT_REPO_NAME.into(),
            filter: String::new(),
            creating: false,
            error: None,
        }
    }
}

enum Act {
    Preview(String),
    Create(String),
    Connect(uuid::Uuid),
    Back,
    Close,
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut w) = app.feat.backup.connect.take() else {
        return;
    };
    let title = if w.import { "다른 컴퓨터의 기록 가져오기" } else { "GitHub 저장소 연결" };
    let mut acts = Vec::new();
    let (_, close) = dialog(ctx, "sync-connect", 620.0, |ui| {
        let desc = match &w.step {
            Step::Choose if w.import => {
                "다른 컴퓨터에서 동기화하던 GitHub 저장소를 고르세요. 먼저 따로 받아서 확인하고, 가져오기 전에 무엇이 들어오는지 보여 드려요."
            }
            Step::Choose => {
                "기록을 올릴 개인 비공개 저장소를 고르세요. 다른 컴퓨터에서 같은 저장소를 연결하면 이어서 써요."
            }
            _ => "",
        };
        dialog_title(ui, title, (!desc.is_empty()).then_some(desc));
        if matches!(w.step, Step::Choose) {
            choose(app, ui, &mut w, &mut acts);
        }
        match &w.step {
            Step::Choose => {}
            Step::Checking(url) => working(
                ui,
                "저장소를 확인하고 있어요",
                &format!("{} · 받기 → 형식 확인 → 개수 세기", display_name(url)),
            ),
            Step::Preview(p) => preview(ui, p, w.import, &mut acts),
            Step::Connecting(name) => {
                let step = app.hub.query::<SyncStatusGet>(HOST, &()).data.and_then(|s| s.running);
                let label = step.map(|s| format!("{}…", s.label())).unwrap_or_else(|| "준비 중…".into());
                working(ui, &format!("{name}에서 첫 동기화를 하고 있어요"), &label);
            }
            Step::Done(r) => done(app, ui, r, &mut acts),
        }
        if let Some(e) = &w.error {
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(c::ROSE_BG)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(icon(icons::CIRCLE_ALERT, 13.0, c::ROSE));
                        ui.label(RichText::new(e).font(font(12.5)).color(c::TEXT));
                    });
                });
        }
    });
    let busy = matches!(w.step, Step::Checking(_) | Step::Connecting(_)) || w.creating;
    if close && !busy {
        acts.push(Act::Close);
    }
    for a in acts {
        match a {
            Act::Close => {
                if let Step::Preview(p) = &w.step {
                    app.hub.send::<SyncDiscardPreview>(HOST, PreviewId { id: p.id });
                }
                return;
            }
            Act::Back => {
                if let Step::Preview(p) = &w.step {
                    app.hub.send::<SyncDiscardPreview>(HOST, PreviewId { id: p.id });
                }
                w.step = Step::Choose;
                w.error = None;
            }
            Act::Preview(url) => start_preview(app, &mut w, url),
            Act::Create(name) => {
                w.creating = true;
                w.error = None;
                app.hub.call::<SyncCreateRepo>(HOST, NewRepo { name }, |app, res| {
                    let Some(mut w) = app.feat.backup.connect.take() else {
                        return;
                    };
                    w.creating = false;
                    match res {
                        Ok(repo) => start_preview(app, &mut w, repo.url),
                        Err(e) => w.error = Some(e.message),
                    }
                    app.feat.backup.connect = Some(w);
                });
            }
            Act::Connect(id) => {
                let name = match &w.step {
                    Step::Preview(p) => p.repo.display.clone(),
                    _ => String::new(),
                };
                w.step = Step::Connecting(name);
                w.error = None;
                app.hub.call::<SyncConnect>(HOST, PreviewId { id }, |app, res| {
                    let Some(w) = app.feat.backup.connect.as_mut() else {
                        if let Err(e) = res {
                            app.toast_error(&e.message);
                        }
                        return;
                    };
                    match res {
                        Ok(r) => w.step = Step::Done(Box::new(r)),
                        Err(e) => {
                            w.step = Step::Choose;
                            w.error = Some(e.message);
                        }
                    }
                });
            }
        }
    }
    app.feat.backup.connect = Some(w);
}

fn start_preview(app: &mut App, w: &mut Wizard, url: String) {
    let url = url.trim().to_string();
    if url.is_empty() {
        w.error = Some("저장소 주소를 입력해 주세요".into());
        return;
    }
    w.step = Step::Checking(url.clone());
    w.error = None;
    app.hub.call::<SyncPreview>(HOST, RepoUrl { url }, |app, res| {
        let Some(w) = app.feat.backup.connect.as_mut() else {
            if let Ok(p) = res {
                app.hub.send::<SyncDiscardPreview>(HOST, PreviewId { id: p.id });
            }
            return;
        };
        match res {
            Ok(p) => w.step = Step::Preview(Box::new(p)),
            Err(e) => {
                w.step = Step::Choose;
                w.error = Some(e.message);
            }
        }
    });
}

fn working(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(16.0).color(c::TEXT_2));
        ui.vertical(|ui| {
            ui.label(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
            ui.label(RichText::new(detail).font(font(12.5)).color(c::TEXT_3));
        });
    });
    ui.add_space(8.0);
    ui.label(
        RichText::new("다른 작업은 그대로 해도 돼요. 저장소가 크면 몇 분 걸릴 수 있어요.")
            .font(font(ty::SMALL))
            .color(c::TEXT_3),
    );
}

fn choose(app: &mut App, ui: &mut egui::Ui, w: &mut Wizard, acts: &mut Vec<Act>) {
    let gh = app.hub.query::<SyncGhStatus>(HOST, &());
    let logged_in = gh.data.as_ref().is_some_and(|g| g.logged_in);
    match (gh.data.as_deref(), gh.error) {
        (Some(g), _) if g.logged_in => {
            status_line(ui, Tone::Running, "GitHub CLI 로그인됨", g.account.as_deref().unwrap_or(""));
        }
        (Some(g), _) => status_line(ui, Tone::Waiting, "GitHub CLI 사용 불가", g.problem.as_deref().unwrap_or("")),
        (None, Some(e)) => status_line(ui, Tone::Waiting, "확인 불가", &e.message),
        (None, None) => status_line(ui, Tone::Waiting, "확인 중", "GitHub CLI를 찾고 있어요"),
    }
    if logged_in && !w.import {
        ui.add_space(4.0);
        ui.label(RichText::new("새 비공개 저장소 만들기").font(medium(12.5)).color(c::TEXT_2));
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut w.new_name)
                .font(mono(12.5))
                .desired_width(240.0)
                .hint_text(DEFAULT_REPO_NAME);
            ui.add(edit);
            let r = ui
                .add_enabled_ui(!w.creating, |ui| {
                    theme::primary_button(ui, if w.creating { "만드는 중…" } else { "만들기" })
                })
                .inner;
            if r.clicked() {
                acts.push(Act::Create(w.new_name.trim().to_string()));
            }
        });
        ui.label(
            RichText::new("내 GitHub 계정에 비공개로 만들어요. 대화 원문이 들어가니 공개로 바꾸지 마세요.")
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
        );
    }
    if logged_in {
        ui.add_space(6.0);
        repo_list(app, ui, w, acts);
    }
    ui.add_space(6.0);
    ui.label(RichText::new("저장소 주소 붙여넣기 (SSH 또는 HTTPS)").font(medium(12.5)).color(c::TEXT_2));
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut w.url)
            .font(mono(12.5))
            .desired_width(ui.available_width() - 70.0)
            .hint_text("git@github.com:me/runwick-data.git");
        let r = ui.add(edit);
        crate::terminal::verify::target("backup:url", r.rect);
        let go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let b = if logged_in { theme::secondary_button(ui, "확인") } else { theme::primary_button(ui, "확인") };
        if b.clicked() || go {
            acts.push(Act::Preview(w.url.clone()));
        }
    });
    ui.label(
        RichText::new(
            "이 컴퓨터의 git 인증(SSH 키, 자격 증명 도우미, gh)을 그대로 써요. Runwick은 토큰을 저장하지 않아요.",
        )
        .font(font(ty::SMALL))
        .color(c::TEXT_3),
    );
}

fn repo_list(app: &mut App, ui: &mut egui::Ui, w: &mut Wizard, acts: &mut Vec<Act>) {
    ui.label(RichText::new("내 저장소에서 고르기").font(medium(12.5)).color(c::TEXT_2));
    let q = app.hub.query::<SyncGhRepos>(HOST, &());
    let Some(list) = q.data else {
        match q.error {
            Some(e) => status_line(ui, Tone::Recover, "목록을 읽지 못했어요", &e.message),
            None => status_line(ui, Tone::Waiting, "읽는 중", "저장소 목록을 가져오고 있어요"),
        };
        return;
    };
    theme::search_field(ui, &mut w.filter, "저장소 이름으로 찾기", egui::Id::unique("sync-repo-filter"), 28.0);
    let needle = w.filter.trim().to_lowercase();
    let shown: Vec<&GhRepo> =
        list.iter().filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle)).collect();
    egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt("sync-repos").max_height(180.0).auto_shrink([false, true]).show(
            ui,
            |ui| {
                if shown.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("맞는 저장소가 없어요").font(font(12.5)).color(c::TEXT_3));
                    ui.add_space(6.0);
                }
                for r in shown {
                    let h = 34.0;
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), egui::Sense::click());
                    let name = r.name.clone();
                    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(6), c::CONTROL);
                    }
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect.shrink2(Vec2::new(10.0, 2.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    child.label(icon(
                        if r.private { icons::LOCK } else { icons::GLOBE },
                        12.0,
                        if r.private { c::TEXT_3 } else { c::YELLOW },
                    ));
                    child.label(RichText::new(&r.name).font(font(12.5)).color(c::TEXT_STRONG));
                    if !r.private {
                        child.label(RichText::new("공개 저장소").font(font(11.5)).color(c::YELLOW));
                    }
                    if let Some(t) = r.updated_at {
                        child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(fmt::when(t)).font(font(11.5)).color(c::TEXT_3));
                        });
                    }
                    theme::focus_ring(ui, &resp, CornerRadius::same(6));
                    let resp = if r.private {
                        resp
                    } else {
                        resp.on_hover_text("공개 저장소에는 대화 원문이 공개돼요. 비공개 저장소를 쓰세요")
                    };
                    if resp.clicked() {
                        acts.push(Act::Preview(r.url.clone()));
                    }
                }
            },
        );
    });
}

fn preview(ui: &mut egui::Ui, p: &ConnectPreview, import: bool, acts: &mut Vec<Act>) {
    ui.horizontal(|ui| {
        ui.label(icon(icons::FOLDER_GIT_2, 15.0, c::TEXT_2));
        ui.label(RichText::new(&p.repo.display).font(semibold(ty::BODY)).color(c::TEXT_STRONG));
        fmt::device_badge(ui, &p.repo.branch, false);
    });
    theme::truncated(ui, &p.repo.url, mono(11.5), c::TEXT_3);
    let ok = matches!(p.content, RepoContent::Empty | RepoContent::Runwick);
    match p.content {
        RepoContent::Empty => status_line(ui, Tone::Info, "비어 있는 저장소", "이 컴퓨터의 기록을 처음 올려요"),
        RepoContent::Runwick => status_line(
            ui,
            Tone::Running,
            "Runwick 기록 저장소",
            "아직 아무것도 가져오지 않았어요. 확인한 뒤에 연결해요",
        ),
        RepoContent::Other => status_line(
            ui,
            Tone::Recover,
            "다른 파일이 있는 저장소",
            "Runwick은 기록 전용 저장소에만 써요. 비어 있는 저장소를 골라 주세요",
        ),
        RepoContent::Newer => status_line(
            ui,
            Tone::Check,
            "더 새로운 형식",
            "이 저장소는 더 새로운 Runwick이 만들었어요. Runwick을 업데이트한 뒤 연결해 주세요",
        ),
    }
    if !p.devices.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("동기화한 기기").font(medium(12.0)).color(c::TEXT_3));
            for d in &p.devices {
                let label = match (d.this, d.last_sync) {
                    (true, _) => format!("{} (이 컴퓨터)", d.name),
                    (false, Some(t)) => format!("{} · {}", d.name, fmt::when(t)),
                    (false, None) => d.name.clone(),
                };
                fmt::device_badge(ui, &label, false);
            }
        });
    }
    if p.content == RepoContent::Runwick {
        counts_table(ui, "sync-preview-counts", &[("저장소에 있음", &p.in_repo), ("이 컴퓨터에 새로", &p.new_here)]);
        let nothing_new = p.new_here.iter().all(|k| k.count == 0);
        if nothing_new {
            ui.label(
                RichText::new("이미 이 컴퓨터에 있는 기록이에요. 다시 가져와도 겹치지 않아요.")
                    .font(font(ty::SMALL))
                    .color(c::TEXT_3),
            );
        }
        if !p.unlinked_projects.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "이 컴퓨터에 폴더가 없는 프로젝트 {}개: {}. 기록은 바로 볼 수 있고, 폴더는 나중에 연결해요.",
                        p.unlinked_projects.len(),
                        p.unlinked_projects.join(", ")
                    ))
                    .font(font(12.5))
                    .color(c::TEXT_2),
                )
                .wrap(),
            );
        }
    }
    egui::Frame::new().fill(c::NAV).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("가져오지 않는 것").font(medium(12.0)).color(c::TEXT_3));
            ui.add(egui::Label::new(RichText::new(NOT_SYNCED.join(" · ")).font(font(12.5)).color(c::TEXT)).wrap());
            ui.label(
                RichText::new(
                    "가져온다고 CLI를 실행하거나 재개하지 않아요. 다른 컴퓨터의 세션은 읽기 전용 기록으로 보여요.",
                )
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
            );
        },
    );
    for prob in &p.problems {
        status_line(ui, Tone::Check, "읽지 못한 파일", prob);
    }
    ui.add_space(4.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let label = if import { "가져오고 동기화 연결" } else { "연결하고 동기화" };
        let r = ui.add_enabled_ui(ok, |ui| theme::primary_button(ui, label)).inner;
        crate::terminal::verify::target("backup:confirm", r.rect);
        if r.clicked() {
            acts.push(Act::Connect(p.id));
        }
        if theme::secondary_button(ui, "다른 저장소 고르기").clicked() {
            acts.push(Act::Back);
        }
    });
}

fn done(app: &mut App, ui: &mut egui::Ui, r: &SyncReport, acts: &mut Vec<Act>) {
    status_line(ui, Tone::Running, "연결했어요", &super::sync::summary(r));
    if !r.imported.is_empty() {
        counts_table(ui, "sync-done-counts", &[("가져옴", &r.imported)]);
    }
    for cf in &r.conflicts {
        status_line(ui, Tone::Check, cf.kind.label(), &format!("{} · {}", cf.title, cf.resolution));
    }
    let unlinked = app.hub.query::<SyncUnlinked>(HOST, &()).data.map(|l| l.len()).unwrap_or(0);
    if unlinked > 0 {
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "이 컴퓨터에 폴더가 없는 프로젝트 {unlinked}개가 있어요. 설정의 ‘이 컴퓨터에 연결된 폴더 없음’에서 폴더를 연결하거나, 같은 저장소를 열면 자동으로 연결돼요."
                ))
                .font(font(12.5))
                .color(c::TEXT_2),
            )
            .wrap(),
        );
    }
    ui.add_space(4.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if theme::secondary_button(ui, "닫기").clicked() {
            acts.push(Act::Close);
        }
    });
}
