//! Settings › 공통 환경변수 (Settings.dc.html): one set of variables for every CLI on this
//! computer. Secret values stay masked; showing or copying one is an explicit action and the
//! value is forgotten again shortly. "저장됨" and "적용됨" are shown apart: running sessions that
//! started with an older version are listed with their refresh entry.

use super::general::{header, toggle};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{data, dialog, dialog_title};
use crate::theme::{self, Tone, c, font, medium, mono, ty};
use crate::views::refresh::{self, PendingFilter};
use egui::{CornerRadius, RichText, Stroke, Vec2};
use runwick_core::Ts;
use runwick_core::api::backup::BackupListGet;
use runwick_core::api::env::*;
use runwick_core::api::refresh::RefreshPending;
use runwick_core::api::sync::{SyncExposedKeys, SyncStagedVars};
use std::time::{Duration, Instant};

const HOST: HostKey = HostKey::Local;
/// A revealed secret is hidden again after this long.
const REVEAL_FOR: Duration = Duration::from_secs(20);

#[derive(Default)]
pub struct State {
    editor: Option<Editor>,
    /// Values shown on request (key, value, since).
    revealed: Vec<(String, String, Instant)>,
}

struct Editor {
    /// Editing an existing variable (its key cannot change).
    existing: Option<EnvVar>,
    key: String,
    value: String,
    description: String,
    secret: bool,
    show_value: bool,
    confirm_delete: bool,
    saving: bool,
    error: Option<String>,
}

impl Editor {
    fn new() -> Self {
        Self {
            existing: None,
            key: String::new(),
            value: String::new(),
            description: String::new(),
            secret: false,
            show_value: false,
            confirm_delete: false,
            saving: false,
            error: None,
        }
    }

    fn edit(v: &EnvVar) -> Self {
        Self {
            existing: Some(v.clone()),
            key: v.key.clone(),
            value: v.value.clone().unwrap_or_default(),
            description: v.description.clone(),
            secret: v.secret,
            ..Self::new()
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(
        ui,
        "공통 환경변수",
        "Runwick이 시작하는 모든 CLI에 같은 값을 넘깁니다. 프로젝트별 값은 .env 같은 프로젝트 도구에서 관리하세요.",
    );
    expire_reveals(app, ui.ctx());
    let q = app.hub.query::<EnvList>(HOST, &());
    let Some(state) = q.data else {
        let text = match &q.error {
            Some(e) => format!("공통 환경변수를 불러오지 못했어요: {}", e.message),
            None => "공통 환경변수를 불러오는 중…".into(),
        };
        ui.label(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3));
        return;
    };
    open_requested(app, &state);
    meta_line(ui, &state);
    staged_note(app, ui);
    ui.add_space(8.0);
    vars_card(app, ui, &state);
    ui.add_space(22.0);
    // "모두 vN을 쓰고 있어요" only means something once a variable exists.
    let current = (!state.vars.is_empty()).then_some(state.version);
    refresh::pending_list(app, ui, PendingFilter::Env, current);
    editor_dialog(app, ui.ctx());
}

/// Verification aid: `RUNWICK_UI_ENV_EDIT=<variable name>` (or `new`) opens the editor once
/// (dialogs are not part of `RUNWICK_UI_STATE`).
fn open_requested(app: &mut App, s: &EnvState) {
    static ASKED: std::sync::Once = std::sync::Once::new();
    ASKED.call_once(|| {
        let Some(key) = hook_env!("RUNWICK_UI_ENV_EDIT") else {
            return;
        };
        app.feat.settings.env.editor = match s.vars.iter().find(|v| v.key == key) {
            Some(v) => Some(Editor::edit(v)),
            None => Some(Editor::new()),
        };
    });
}

fn meta_line(ui: &mut egui::Ui, s: &EnvState) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let saved = match (s.version, s.saved_at) {
            (0, _) => "아직 저장한 변수가 없어요".to_string(),
            (v, Some(at)) => format!("v{v} 저장 · {}", data::ago(at)),
            (v, None) => format!("v{v} 저장"),
        };
        ui.label(RichText::new(saved).font(font(12.0)).color(c::TEXT_3));
        ui.label(RichText::new("·").font(font(12.0)).color(c::TEXT_OFF));
        let (glyph, text) = match s.secrets.kind {
            SecretStoreKind::Keychain => (icons::KEY_ROUND, "비밀값은 macOS 키체인에 보관해요".to_string()),
            SecretStoreKind::File => (
                icons::LOCK,
                format!("이 컴퓨터에는 키체인이 없어 비밀값을 {} 파일(본인만 읽기)에 보관해요", s.secrets.location),
            ),
        };
        ui.label(theme::icon(glyph, 11.0, c::TEXT_3));
        ui.label(RichText::new(text).font(font(12.0)).color(c::TEXT_3));
    });
    ui.label(
        RichText::new(
            "이 컴퓨터의 설정이에요. 다른 기기는 그 기기의 창에서 따로 관리하고, 백업·동기화에 비밀값은 포함하지 않아요.",
        )
        .font(font(12.0))
        .color(c::TEXT_3),
    );
    ui.add(
        egui::Label::new(
            RichText::new(
                "Runwick 화면에서는 값을 가려 두지만, 이 컴퓨터에서 같은 사용자로 실행되는 다른 프로그램은 키체인과 Runwick 데이터를 직접 읽을 수 있어요.",
            )
            .font(font(12.0))
            .color(c::TEXT_3),
        )
        .wrap(),
    );
}

/// Shown for a secret whose current value went out as plain text before it became secret.
const EXPOSED: &str =
    "이 값은 비밀로 바꾸기 전에 동기화나 백업에 들어갔어요 · 저장소 이력과 그때 만든 백업에 남아 있으니 값을 바꾸세요";

/// Where plain values went: names synced as plain (`sync.exposed_keys`), plain periods of the
/// variables (`env.plain_history`) and when this computer's backups were made. Empty while
/// unknown, so nothing is claimed without evidence.
struct Exposure {
    synced: Vec<String>,
    periods: Vec<PlainPeriod>,
    backups: Vec<Ts>,
}

fn exposure(app: &mut App) -> Exposure {
    let synced = app.hub.query::<SyncExposedKeys>(HOST, &()).data.map(|d| (*d).clone()).unwrap_or_default();
    let periods = app.hub.query::<EnvPlainHistory>(HOST, &()).data.map(|d| (*d).clone()).unwrap_or_default();
    let backups = app
        .hub
        .query::<BackupListGet>(HOST, &())
        .data
        .map(|l| l.items.iter().filter(|b| !b.copy).filter_map(|b| b.created_at).collect())
        .unwrap_or_default();
    Exposure { synced, periods, backups }
}

impl Exposure {
    /// Whether `key` keeps, as a secret, a value that was synced or backed up as plain text —
    /// already, or once saved (`becoming_secret`: the flag is being turned on). `keeps_value`
    /// is false when a new value replaces it.
    fn leaked(&self, key: &str, becoming_secret: bool, keeps_value: bool, now: Ts) -> bool {
        let Some(p) = self.periods.iter().find(|p| p.key == key) else {
            return false;
        };
        if !keeps_value || (p.until.is_none() && !becoming_secret) {
            return false;
        }
        let until = p.until.unwrap_or(now);
        self.synced.iter().any(|k| k == key) || self.backups.iter().any(|t| *t >= p.since && *t <= until)
    }
}

fn exposed_note(ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(theme::icon(Tone::Attention.icon(), 12.0, Tone::Attention.fg()));
        ui.label(RichText::new(EXPOSED).font(font(12.0)).color(Tone::Attention.fg()));
    });
}

/// Values from another computer wait for review in 백업·동기화 and are not in this list (or
/// any run) until accepted; say so, with the way there.
fn staged_note(app: &mut App, ui: &mut egui::Ui) {
    let waiting =
        app.hub.query::<SyncStagedVars>(HOST, &()).data.map(|l| l.iter().filter(|v| !v.rejected).count()).unwrap_or(0);
    if waiting == 0 {
        return;
    }
    ui.add_space(6.0);
    let mut open = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(theme::icon(Tone::Attention.icon(), 12.0, Tone::Attention.fg()));
        let text = format!(
            "다른 컴퓨터에서 온 값 {waiting}개가 검토를 기다려요. 적용하기 전에는 이 목록과 실행에 쓰이지 않아요."
        );
        ui.label(RichText::new(text).font(font(12.0)).color(c::TEXT_2));
        open = theme::link_button(ui, "백업·동기화에서 검토").clicked();
    });
    if open {
        app.ui.settings_section = crate::state::SettingsSection::Backup;
    }
}

/// Running sessions still waiting for each key (from the pending list).
fn waiting_for(app: &mut App, key: &str) -> usize {
    app.hub
        .query::<RefreshPending>(HOST, &())
        .data
        .map(|l| l.iter().filter(|p| p.env_changes.iter().any(|c| c.key == key)).count())
        .unwrap_or(0)
}

fn vars_card(app: &mut App, ui: &mut egui::Ui, s: &EnvState) {
    let mut edit = None;
    let mut reveal = None;
    let mut copy = None;
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        if s.vars.is_empty() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 14)).show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "아직 공통 환경변수가 없어요. 프록시나 API 토큰처럼 모든 CLI에 함께 넘길 값을 추가하세요.",
                        )
                        .font(font(12.5))
                        .color(c::TEXT_3),
                    )
                    .wrap(),
                );
            });
        }
        let exposure = exposure(app);
        let now = runwick_core::now();
        for v in &s.vars {
            let waiting = waiting_for(app, &v.key);
            let exposed = v.secret && exposure.leaked(&v.key, false, true, now);
            let shown = app.feat.settings.env.revealed.iter().find(|r| r.0 == v.key).map(|r| r.1.clone());
            match var_row(ui, v, waiting, shown.as_deref(), exposed) {
                Some(RowAct::Edit) => edit = Some(v.clone()),
                Some(RowAct::Reveal) => reveal = Some(v.key.clone()),
                Some(RowAct::Hide) => app.feat.settings.env.revealed.retain(|r| r.0 != v.key),
                Some(RowAct::Copy) => copy = Some(v.key.clone()),
                None => {}
            }
            let r = ui.available_rect_before_wrap();
            ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, c::LINE));
        }
        let add = crate::shell::widgets::row(ui, 40.0, false, "변수 추가");
        let p = ui.painter();
        p.text(
            add.rect.left_center() + Vec2::new(20.0, 0.0),
            egui::Align2::CENTER_CENTER,
            icons::PLUS,
            theme::icon_font(12.0),
            c::TEXT_2,
        );
        p.text(
            add.rect.left_center() + Vec2::new(32.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "변수 추가",
            font(12.5),
            c::TEXT_2,
        );
        if add.clicked() {
            app.feat.settings.env.editor = Some(Editor::new());
        }
    });
    if let Some(v) = edit {
        app.feat.settings.env.editor = Some(Editor::edit(&v));
    }
    if let Some(key) = reveal {
        fetch_value(app, key, false, ui.ctx().clone());
    }
    if let Some(key) = copy {
        fetch_value(app, key, true, ui.ctx().clone());
    }
}

enum RowAct {
    Edit,
    Reveal,
    Hide,
    Copy,
}

fn var_row(ui: &mut egui::Ui, v: &EnvVar, waiting: usize, shown: Option<&str>, exposed: bool) -> Option<RowAct> {
    let mut act = None;
    egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
        ui.set_min_height(34.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let w = ui.available_width();
            // Room for three icons on every row so the columns line up.
            let actions_w = 3.0 * 28.0;
            let state_w = 128.0;
            let rest = (w - actions_w - state_w - 36.0).max(160.0);
            ui.vertical(|ui| {
                ui.set_width(rest * 0.45);
                ui.spacing_mut().item_spacing.y = 1.0;
                theme::truncated(ui, &v.key, mono(12.5), c::TEXT_STRONG);
                let desc = if v.description.is_empty() {
                    if v.secret { "보안 저장소에 보관" } else { "설명 없음" }
                } else {
                    &v.description
                };
                theme::truncated(ui, desc, font(12.0), c::TEXT_3);
            });
            ui.vertical(|ui| {
                ui.set_width(rest * 0.55);
                ui.add_space(8.0);
                let value = match (v.secret, shown) {
                    (true, Some(s)) => s.to_string(),
                    (true, None) => "••••••••".to_string(),
                    (false, _) => v.value.clone().unwrap_or_default(),
                };
                let color = if v.secret && shown.is_none() { c::TEXT_3 } else { c::TEXT_2 };
                let value = if value.is_empty() { "(빈 값)".to_string() } else { value };
                theme::truncated(ui, value, mono(12.5), color);
            });
            ui.vertical(|ui| {
                ui.set_width(state_w);
                ui.add_space(8.0);
                let (text, color) = if waiting > 0 {
                    (format!("저장됨 · 적용 대기 {waiting}"), c::EMBER)
                } else {
                    ("저장됨".to_string(), c::TEXT_3)
                };
                ui.label(RichText::new(text).font(font(12.0)).color(color)).on_hover_text(if waiting > 0 {
                    "실행 중인 세션 중 이 값을 아직 받지 못한 세션이 있어요. 아래 목록에서 새로고침하세요."
                } else {
                    "실행 중인 세션은 모두 이 값을 쓰고 있어요"
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if theme::icon_button(ui, icons::PENCIL, &format!("{} 편집", v.key)).clicked() {
                    act = Some(RowAct::Edit);
                }
                if v.secret {
                    if theme::icon_button(ui, icons::COPY, &format!("{} 값 복사", v.key)).clicked() {
                        act = Some(RowAct::Copy);
                    }
                    let (glyph, tip, a) = if shown.is_some() {
                        (icons::EYE_OFF, format!("{} 값 숨기기", v.key), RowAct::Hide)
                    } else {
                        (icons::EYE, format!("{} 값 보기", v.key), RowAct::Reveal)
                    };
                    if theme::icon_button(ui, glyph, &tip).clicked() {
                        act = Some(a);
                    }
                }
            });
        });
        if exposed {
            exposed_note(ui);
        }
    });
    act
}

fn fetch_value(app: &mut App, key: String, copy: bool, ctx: egui::Context) {
    app.hub.call::<EnvReveal>(HOST, VarKey { key: key.clone() }, move |app, res| match res {
        Ok(value) if copy => {
            ctx.copy_text(value);
            app.toast_info(&format!("{key} 값을 복사했어요"));
        }
        Ok(value) => {
            let r = &mut app.feat.settings.env.revealed;
            r.retain(|x| x.0 != key);
            r.push((key, value, Instant::now()));
            ctx.request_repaint_after(REVEAL_FOR);
        }
        Err(e) => app.toast_error(&format!("{key} 값을 읽지 못했어요: {}", e.message)),
    });
}

fn expire_reveals(app: &mut App, ctx: &egui::Context) {
    let r = &mut app.feat.settings.env.revealed;
    r.retain(|x| x.2.elapsed() < REVEAL_FOR);
    if let Some(oldest) = r.iter().map(|x| x.2).min() {
        ctx.request_repaint_after(REVEAL_FOR.saturating_sub(oldest.elapsed()));
    }
}

fn editor_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut ed) = app.feat.settings.env.editor.take() else {
        return;
    };
    let editing = ed.existing.is_some();
    let exposure = exposure(app);
    let mut save = false;
    let mut delete = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "env-editor", 520.0, |ui| {
        let title = if editing { "변수 편집" } else { "변수 추가" };
        dialog_title(ui, title, Some("저장해도 실행 중인 세션은 그대로예요. 필요한 세션만 새로고침하면 적용돼요."));
        field_label(ui, "이름");
        let key_problem = (!editing).then(|| key_problem(ed.key.trim())).flatten();
        ui.add_enabled(
            !editing,
            egui::TextEdit::singleline(&mut ed.key)
                .font(mono(12.5))
                .hint_text("예: HTTP_PROXY")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(10, 7)),
        );
        if let Some(p) = key_problem.as_ref().filter(|_| !ed.key.is_empty()) {
            ui.label(RichText::new(p).font(font(12.0)).color(c::ROSE));
        }
        field_label(ui, "값");
        let keep_hint = editing && ed.existing.as_ref().is_some_and(|v| v.secret);
        ui.horizontal(|ui| {
            let w = ui.available_width() - 32.0;
            ui.add(
                egui::TextEdit::singleline(&mut ed.value)
                    .font(mono(12.5))
                    .password(ed.secret && !ed.show_value)
                    .hint_text(if keep_hint { "바꾸지 않으려면 비워 두세요" } else { "값" })
                    .desired_width(w)
                    .margin(egui::Margin::symmetric(10, 7)),
            );
            if ed.secret {
                let (g, tip) = if ed.show_value {
                    (icons::EYE_OFF, "입력값 숨기기")
                } else {
                    (icons::EYE, "입력값 보기")
                };
                if theme::icon_button(ui, g, tip).clicked() {
                    ed.show_value = !ed.show_value;
                }
            }
        });
        field_label(ui, "설명 (선택)");
        ui.add(
            egui::TextEdit::singleline(&mut ed.description)
                .hint_text("예: 사내망 프록시")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(10, 7)),
        );
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            toggle(ui, &mut ed.secret, "비밀값");
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new("비밀값").font(medium(ty::BODY)).color(c::TEXT_STRONG));
                ui.label(
                    RichText::new(
                        "값을 가리고 보안 저장소에 보관해요. 비밀값으로 저장한 뒤로는 기록·로그·동기화에 남지 않아요.",
                    )
                    .font(font(12.0))
                    .color(c::TEXT_3),
                );
            });
        });
        if let Some(old) = &ed.existing {
            // The stored value stays when the field is left empty (secret) or unchanged (plain).
            let keeps = if old.secret { ed.value.is_empty() } else { Some(&ed.value) == old.value.as_ref() };
            let to_plain = !ed.secret;
            if !to_plain && exposure.leaked(&old.key, !old.secret, keeps, runwick_core::now()) {
                exposed_note(ui);
            }
        }
        if let Some(e) = &ed.error {
            ui.label(RichText::new(e).font(font(12.0)).color(c::ROSE));
        }
        ui.add_space(6.0);
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, c::LINE_STRONG));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if editing {
                if ed.confirm_delete {
                    ui.label(RichText::new("정말 삭제할까요?").font(font(12.5)).color(c::ROSE));
                    delete = theme::danger_button(ui, "삭제").clicked();
                    if theme::link_button(ui, "아니요").clicked() {
                        ed.confirm_delete = false;
                    }
                } else if theme::danger_button(ui, "삭제…").clicked() {
                    ed.confirm_delete = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = key_problem.is_none() && !ed.saving;
                let label = if ed.saving { "저장하는 중…" } else { "저장" };
                let b = ui.add_enabled(
                    ok,
                    egui::Button::new(RichText::new(label).font(medium(ty::BODY)).color(c::ON_EMBER))
                        .fill(c::EMBER)
                        .corner_radius(CornerRadius::same(7))
                        .min_size(Vec2::new(64.0, 30.0)),
                );
                save = b.clicked();
                cancel = theme::secondary_button(ui, "취소").clicked();
            });
        });
    });
    if save && !ed.saving {
        ed.saving = true;
        ed.error = None;
        let was_secret = ed.existing.as_ref().is_some_and(|v| v.secret);
        let value = if editing && was_secret && ed.value.is_empty() { None } else { Some(ed.value.clone()) };
        let p =
            SetVar { key: ed.key.trim().to_string(), value, description: ed.description.clone(), secret: ed.secret };
        app.hub.call::<EnvSet>(HOST, p, |app, res| {
            let st = &mut app.feat.settings.env;
            match res {
                Ok(v) => {
                    st.editor = None;
                    st.revealed.retain(|r| r.0 != v.key);
                    app.toast_info(&format!("{} 저장했어요. 실행 중인 세션은 새로고침해야 적용돼요", v.key));
                }
                Err(e) => match st.editor.as_mut() {
                    Some(ed) => {
                        ed.saving = false;
                        ed.error = Some(e.message);
                    }
                    None => app.toast_error(&format!("저장하지 못했어요: {}", e.message)),
                },
            }
        });
    }
    if delete {
        let key = ed.key.clone();
        app.hub.call::<EnvRemove>(HOST, VarKey { key: key.clone() }, move |app, res| match res {
            Ok(()) => {
                app.feat.settings.env.editor = None;
                app.feat.settings.env.revealed.retain(|r| r.0 != key);
                app.toast_info(&format!("{key} 삭제했어요. 실행 중인 세션은 새로고침해야 빠져요"));
            }
            Err(e) => match app.feat.settings.env.editor.as_mut() {
                Some(ed) => ed.error = Some(e.message),
                None => app.toast_error(&format!("삭제하지 못했어요: {}", e.message)),
            },
        });
    }
    if !(close || cancel) {
        app.feat.settings.env.editor = Some(ed);
    }
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(medium(12.0)).color(c::TEXT_3));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn at(min: i64) -> Ts {
        Utc::now() - Duration::minutes(100) + Duration::minutes(min)
    }

    fn period(key: &str, since: i64, until: Option<i64>) -> PlainPeriod {
        PlainPeriod { key: key.into(), since: at(since), until: until.map(at) }
    }

    #[test]
    fn warns_only_when_a_kept_plain_value_was_synced_or_backed_up_while_plain() {
        let now = at(100);
        let e = Exposure {
            synced: vec!["SYNCED".into()],
            periods: vec![
                period("SYNCED", 0, Some(10)),
                period("BACKED_UP", 0, Some(10)),
                period("BACKUP_LATER", 20, Some(30)),
                period("STILL_PLAIN", 50, None),
            ],
            backups: vec![at(5), at(40), at(60)],
        };
        assert!(e.leaked("SYNCED", false, true, now));
        assert!(e.leaked("BACKED_UP", false, true, now), "backup at 5 is inside 0..10");
        assert!(!e.leaked("BACKUP_LATER", false, true, now), "no backup between 20 and 30");
        // A new value replaces the old one: nothing to warn about.
        assert!(!e.leaked("SYNCED", false, false, now));
        // A plain variable: no warning until it is being turned secret (backup at 60 had it).
        assert!(!e.leaked("STILL_PLAIN", false, true, now));
        assert!(e.leaked("STILL_PLAIN", true, true, now));
        // Never plain (no period) or unknown evidence: nothing is claimed.
        assert!(!e.leaked("BORN_SECRET", true, true, now));
        let unknown = Exposure { synced: vec![], periods: e.periods.clone(), backups: vec![] };
        assert!(!unknown.leaked("BACKED_UP", false, true, now));
    }
}
