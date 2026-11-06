//! Relationship graph (WikiGraph.dc.html): pages as nodes, real links solid, the selected
//! page's source sessions dotted, and "내용이 비슷한 문서" suggestions dashed. Suggestions come
//! from local word overlap (not AI) and stay suggestions until `링크로 연결` writes a real
//! link through a normal save, or `관련 없음` hides them for good.
//!
//! The layout is a small force simulation that runs only until it settles, so an idle graph
//! costs no frames. Drag the background to pan, scroll to move, pinch or ⌘-scroll to zoom.

use super::{Ctx, page_state, when};
use crate::App;
use crate::icons;
use crate::shell::widgets;
use crate::state::TabKind;
use crate::theme::{self, Seg, Tone, c, font, icon_font, medium, semibold, ty};
use crate::views::history::fmt;
use egui::{Align2, CornerRadius, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};
use runwick_core::api::wiki::*;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

const ASIDE_W: f32 = 300.0;
const STEPS_PER_FRAME: usize = 4;
const MAX_STEPS: u32 = 900;

pub(super) struct GraphState {
    nodes: Vec<Node>,
    key: u64,
    selected: Option<String>,
    show_links: bool,
    show_sources: bool,
    show_similar: bool,
    pan: Vec2,
    zoom: f32,
    find: String,
    steps: u32,
    settled: bool,
    dragging: Option<usize>,
    /// The view was fitted to the layout once it first settled.
    fitted: bool,
    /// Suggestions hidden or linked here while the host catches up.
    decided: HashSet<(String, String)>,
}

impl Default for GraphState {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            key: 0,
            selected: None,
            show_links: true,
            show_sources: true,
            show_similar: true,
            pan: Vec2::ZERO,
            zoom: 1.0,
            find: String::new(),
            steps: 0,
            settled: false,
            dragging: None,
            fitted: false,
            decided: HashSet::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Page,
    Session,
}

struct Node {
    /// `p:<slug>` or `s:<session id>`.
    id: String,
    kind: Kind,
    label: String,
    pos: Vec2,
    vel: Vec2,
    pinned: bool,
}

fn page_id(slug: &str) -> String {
    format!("p:{slug}")
}

fn seed(id: &str) -> Vec2 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    let v = h.finish();
    let angle = (v % 3600) as f32 / 3600.0 * std::f32::consts::TAU;
    let r = 140.0 + ((v >> 16) % 160) as f32;
    Vec2::new(angle.cos() * r, angle.sin() * r)
}

fn pair(a: &str, b: &str) -> (String, String) {
    if a <= b { (a.to_string(), b.to_string()) } else { (b.to_string(), a.to_string()) }
}

/// Edges shown with the current filters (sources only for the selected page). Two pages that
/// link to each other are one connection (the host lists each direction).
fn visible_edges<'a>(g: &'a Graph, gs: &GraphState) -> Vec<&'a Edge> {
    let mut seen = HashSet::new();
    g.edges
        .iter()
        .filter(|e| match e.kind {
            EdgeKind::Link => gs.show_links && seen.insert(pair(&e.from, &e.to)),
            EdgeKind::Source => gs.show_sources && gs.selected.as_deref() == Some(page_id(&e.from).as_str()),
            EdgeKind::Similar => gs.show_similar && !gs.decided.contains(&pair(&e.from, &e.to)),
        })
        .collect()
}

/// Connected page pairs, whichever page links to which.
fn link_pairs(g: &Graph) -> usize {
    g.edges.iter().filter(|e| e.kind == EdgeKind::Link).map(|e| pair(&e.from, &e.to)).collect::<HashSet<_>>().len()
}

/// How a link connects the selected page `sel` (`p:<slug>`) with the other one.
fn link_direction(g: &Graph, e: &Edge, sel: &str) -> (&'static str, &'static str) {
    let both = g.edges.iter().any(|x| x.kind == EdgeKind::Link && x.from == e.to && x.to == e.from);
    if both {
        ("서로 링크", "두 문서가 본문에서 서로 링크해요")
    } else if sel.strip_prefix("p:") == Some(e.from.as_str()) {
        ("링크", "이 문서 본문에서 링크해요")
    } else {
        ("받은 링크", "저 문서 본문이 이 문서를 링크해요")
    }
}

fn edge_ends(e: &Edge) -> (String, String) {
    match e.kind {
        EdgeKind::Source => (page_id(&e.from), format!("s:{}", e.to)),
        _ => (page_id(&e.from), page_id(&e.to)),
    }
}

/// Keep node positions for ids that stay; add new ones around the center (or their page).
fn sync_nodes(gs: &mut GraphState, g: &Graph, edges: &[&Edge]) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in &g.pages {
        p.slug.hash(&mut h);
        p.version.hash(&mut h);
    }
    for e in edges {
        edge_ends(e).hash(&mut h);
    }
    gs.selected.hash(&mut h);
    let key = h.finish();
    if key == gs.key {
        return;
    }
    gs.key = key;
    let old: HashMap<String, (Vec2, bool)> = gs.nodes.drain(..).map(|n| (n.id, (n.pos, n.pinned))).collect();
    let mut nodes: Vec<Node> = Vec::new();
    for p in &g.pages {
        let id = page_id(&p.slug);
        let (pos, pinned) = old.get(&id).copied().unwrap_or((seed(&id), false));
        nodes.push(Node { id, kind: Kind::Page, label: p.title.clone(), pos, vel: Vec2::ZERO, pinned });
    }
    for e in edges.iter().filter(|e| e.kind == EdgeKind::Source) {
        let id = format!("s:{}", e.to);
        if nodes.iter().any(|n| n.id == id) {
            continue;
        }
        let near = nodes.iter().find(|n| n.id == page_id(&e.from)).map(|n| n.pos).unwrap_or_default();
        let (pos, pinned) = old.get(&id).copied().unwrap_or((near + seed(&id) * 0.4, false));
        let label = g
            .sessions
            .iter()
            .find(|s| s.id.to_string() == e.to)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "세션".into());
        nodes.push(Node { id, kind: Kind::Session, label, pos, vel: Vec2::ZERO, pinned });
    }
    gs.nodes = nodes;
    gs.steps = 0;
    gs.settled = false;
}

/// One simulation step. Returns the kinetic energy.
fn step(gs: &mut GraphState, links: &[(usize, usize, f32, f32)]) -> f32 {
    let n = gs.nodes.len();
    let mut force = vec![Vec2::ZERO; n];
    for i in 0..n {
        for j in i + 1..n {
            let d = gs.nodes[i].pos - gs.nodes[j].pos;
            let dist2 = d.length_sq().max(25.0);
            let f = d.normalized() * (9000.0 / dist2);
            force[i] += f;
            force[j] -= f;
        }
    }
    for &(a, b, len, k) in links {
        let d = gs.nodes[b].pos - gs.nodes[a].pos;
        let dist = d.length().max(1.0);
        let f = d / dist * ((dist - len) * k);
        force[a] += f;
        force[b] -= f;
    }
    let mut energy = 0.0;
    for (i, node) in gs.nodes.iter_mut().enumerate() {
        if node.pinned {
            node.vel = Vec2::ZERO;
            continue;
        }
        let f = force[i] - node.pos * 0.012;
        node.vel = (node.vel + f) * 0.82;
        let speed = node.vel.length();
        if speed > 30.0 {
            node.vel *= 30.0 / speed;
        }
        node.pos += node.vel;
        energy += node.vel.length_sq();
    }
    energy
}

pub(super) fn show(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, gs: &mut GraphState) {
    let q = app.hub.query::<WikiGraph>(cx.host, &WikiQuery { project_id: Some(cx.project) });
    egui::ScrollArea::vertical().id_salt(("wiki-graph", cx.tab)).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 28, top: 22, bottom: 28 }).show(ui, |ui| {
            let w = ui.available_width().min(1120.0);
            let pad = ((ui.available_width() - w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    header(app, ui, cx, gs);
                    ui.add_space(12.0);
                    super::offline_banner(ui, cx);
                    match (&q.data, &q.error) {
                        (Some(g), _) => body(app, ui, cx, gs, g),
                        (None, Some(e)) => {
                            super::home::error_text(ui, &format!("관계를 불러오지 못했어요: {}", e.message))
                        }
                        (None, None) => super::home::loading(ui, "관계를 계산하는 중…"),
                    }
                });
            });
        });
    });
}

fn header(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, gs: &mut GraphState) {
    let wide = ui.available_width() >= 1000.0;
    let title = |ui: &mut egui::Ui| {
        ui.label(RichText::new("위키").font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
        ui.add(
            egui::Label::new(
                RichText::new(
                    "링크와 근거는 실제 기록이에요. ‘내용이 비슷한 문서’는 확인하기 전까지 제안으로만 보여요.",
                )
                .font(font(ty::BODY))
                .color(c::TEXT_2),
            )
            .wrap(),
        );
    };
    let actions = |app: &mut App, ui: &mut egui::Ui, gs: &mut GraphState, rtl: bool| {
        for i in if rtl { [2, 1, 0] } else { [0, 1, 2] } {
            match i {
                0 => {
                    let id = egui::Id::unique(("graph-find", cx.tab));
                    ui.add_sized(Vec2::new(230.0, 30.0), |ui: &mut egui::Ui| {
                        theme::search_field(ui, &mut gs.find, "문서 찾아 가운데 두기", id, 30.0)
                    });
                }
                1 => {
                    if widgets::pill_button(ui, Some(icons::PLUS), "새 문서", 30.0, c::CONTROL).clicked() {
                        super::dialogs::open_new(app, cx);
                    }
                }
                _ => {
                    if widgets::pill_button(ui, Some(icons::SPARKLES), "세션으로 초안", 30.0, c::CONTROL)
                        .on_hover_text("구독 사용량을 씁니다")
                        .clicked()
                    {
                        super::dialogs::open_draft(app, cx);
                    }
                }
            }
        }
    };
    if wide {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 520.0);
                title(ui);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| actions(app, ui, gs, true));
        });
    } else {
        title(ui);
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| actions(app, ui, gs, false));
    }
}

fn body(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, gs: &mut GraphState, g: &Graph) {
    let count = |k: EdgeKind| match k {
        EdgeKind::Link => link_pairs(g),
        _ => g
            .edges
            .iter()
            .filter(|e| e.kind == k && !(k == EdgeKind::Similar && gs.decided.contains(&pair(&e.from, &e.to))))
            .count(),
    };
    let linked: HashSet<&str> =
        g.edges.iter().filter(|e| e.kind == EdgeKind::Link).flat_map(|e| [e.from.as_str(), e.to.as_str()]).collect();
    let lonely = g.pages.iter().filter(|p| !linked.contains(p.slug.as_str())).count();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let views = [
            Seg { text: Some("목록"), glyph: None, name: "목록" },
            Seg { text: Some("관계"), glyph: None, name: "관계" },
        ];
        if theme::segmented(ui, &views, 1, 26.0) == Some(0) {
            crate::shell::actions::open_center_tab(app, TabKind::Wiki { project: cx.project, page: None });
        }
        ui.add_space(8.0);
        if fmt::chip_button(ui, &format!("링크·참조 {}", count(EdgeKind::Link)), gs.show_links, false).clicked() {
            gs.show_links = !gs.show_links;
        }
        if fmt::chip_button(ui, &format!("근거 기록 {}", count(EdgeKind::Source)), gs.show_sources, false)
            .on_hover_text("고른 문서의 근거 세션만 펼쳐요")
            .clicked()
        {
            gs.show_sources = !gs.show_sources;
        }
        if fmt::chip_button(ui, &format!("내용이 비슷한 문서 {}", count(EdgeKind::Similar)), gs.show_similar, false)
            .on_hover_text("AI가 아니라 이 컴퓨터에서 낱말이 얼마나 겹치는지로 찾아요")
            .clicked()
        {
            gs.show_similar = !gs.show_similar;
        }
        if lonely > 0 {
            ui.label(RichText::new(format!("링크 없는 문서 {lonely}개")).font(font(ty::SMALL)).color(c::TEXT_3));
        }
    });
    ui.add_space(10.0);
    if g.pages.is_empty() {
        theme::card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("아직 문서가 없어 관계를 그릴 수 없어요.").font(medium(13.0)).color(c::TEXT_STRONG));
            if theme::secondary_button(ui, "새 문서").clicked() {
                super::dialogs::open_new(app, cx);
            }
        });
        return;
    }
    if gs.selected.as_ref().is_none_or(|s| !g.pages.iter().any(|p| page_id(&p.slug) == *s) && !s.starts_with("s:")) {
        let degree = |slug: &str| {
            g.edges.iter().filter(|e| e.kind != EdgeKind::Source && (e.from == slug || e.to == slug)).count()
        };
        gs.selected = g.pages.iter().rev().max_by_key(|p| degree(&p.slug)).map(|p| page_id(&p.slug));
    }
    find(gs, g);
    let edges = visible_edges(g, gs);
    sync_nodes(gs, g, &edges);
    let wide = ui.available_width() >= 860.0;
    let canvas_w = if wide { ui.available_width() - ASIDE_W - 20.0 } else { ui.available_width() };
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(canvas_w);
            canvas(app, ui, cx, gs, g, &edges, canvas_w);
            legend(ui);
        });
        if wide {
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.set_width(ASIDE_W);
                aside(app, ui, cx, gs, g, &edges);
            });
        }
    });
    if !wide {
        ui.add_space(12.0);
        aside(app, ui, cx, gs, g, &edges);
    }
}

fn find(gs: &mut GraphState, g: &Graph) {
    let q = gs.find.trim().to_lowercase();
    if q.is_empty() {
        return;
    }
    if let Some(p) = g.pages.iter().find(|p| p.title.to_lowercase().contains(&q)) {
        let id = page_id(&p.slug);
        if gs.selected.as_deref() != Some(id.as_str()) {
            gs.selected = Some(id.clone());
            if let Some(n) = gs.nodes.iter().find(|n| n.id == id) {
                gs.pan = -n.pos;
            }
        }
    }
}

struct Laid {
    rect: Rect,
    galley: std::sync::Arc<egui::Galley>,
}

fn canvas(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, gs: &mut GraphState, g: &Graph, edges: &[&Edge], w: f32) {
    let h = (w * 0.6).clamp(360.0, 640.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click_and_drag());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Group, true, "위키 관계 그래프"));
    let index: HashMap<String, usize> = gs.nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
    let links: Vec<(usize, usize, f32, f32)> = edges
        .iter()
        .filter_map(|e| {
            let (a, b) = edge_ends(e);
            let (len, k) = match e.kind {
                EdgeKind::Link => (150.0, 0.02),
                EdgeKind::Source => (95.0, 0.03),
                EdgeKind::Similar => (190.0, 0.008),
            };
            Some((*index.get(&a)?, *index.get(&b)?, len, k))
        })
        .collect();
    if !gs.settled {
        for _ in 0..STEPS_PER_FRAME {
            let e = step(gs, &links);
            gs.steps += 1;
            if e < 0.05 * gs.nodes.len() as f32 || gs.steps >= MAX_STEPS {
                gs.settled = true;
                break;
            }
        }
        ui.ctx().request_repaint();
    }
    let center = rect.center();
    let to_screen = |p: Vec2, gs: &GraphState| center + (p + gs.pan) * gs.zoom;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(12), c::WINDOW);
    painter.rect_stroke(rect, CornerRadius::same(12), Stroke::new(1.0, c::LINE), egui::StrokeKind::Inside);
    // Layout of node boxes.
    let laid: Vec<Laid> = gs
        .nodes
        .iter()
        .map(|n| {
            let mut job = egui::text::LayoutJob::simple_singleline(n.label.clone(), font(12.0), c::TEXT);
            job.wrap = egui::text::TextWrapping::truncate_at_width(150.0);
            let galley = painter.layout_job(job);
            let size = Vec2::new(galley.size().x + 34.0, if n.kind == Kind::Page { 28.0 } else { 26.0 });
            Laid { rect: Rect::from_center_size(to_screen(n.pos, gs), size), galley }
        })
        .collect();
    let sel = gs.selected.clone();
    let touches = |e: &Edge| {
        let (a, b) = edge_ends(e);
        sel.as_deref() == Some(a.as_str()) || sel.as_deref() == Some(b.as_str())
    };
    let near: HashSet<String> = edges
        .iter()
        .filter(|e| touches(e))
        .flat_map(|e| {
            let (a, b) = edge_ends(e);
            [a, b]
        })
        .chain(sel.clone())
        .collect();
    for e in edges {
        let (a, b) = edge_ends(e);
        let (Some(&ia), Some(&ib)) = (index.get(&a), index.get(&b)) else {
            continue;
        };
        let (pa, pb) = (laid[ia].rect.center(), laid[ib].rect.center());
        let on = touches(e);
        match e.kind {
            EdgeKind::Link => {
                painter.line_segment(
                    [pa, pb],
                    Stroke::new(if on { 1.6 } else { 1.3 }, if on { c::TEXT_2 } else { c::LINE_STRONG }),
                );
            }
            EdgeKind::Source => {
                let col = if on { c::TEXT_2 } else { c::LINE_STRONG };
                painter.extend(Shape::dotted_line(&[pa, pb], col, 5.0, 1.1));
            }
            EdgeKind::Similar => {
                let col = if on { c::SKY } else { c::SKY.gamma_multiply(0.3) };
                painter.extend(Shape::dashed_line(&[pa, pb], Stroke::new(1.5, col), 6.0, 5.0));
                if on && let Some(s) = e.score {
                    let mid = pa + (pb - pa) / 2.0;
                    let t = painter.layout_no_wrap(format!("{:.0}%", s * 100.0), font(11.0), c::SKY);
                    let r = Rect::from_center_size(mid, t.size() + Vec2::new(12.0, 4.0));
                    painter.rect_filled(r, CornerRadius::same(9), c::NAV);
                    painter.rect_stroke(r, CornerRadius::same(9), Stroke::new(1.0, c::SKY), egui::StrokeKind::Inside);
                    painter.galley(r.center() - t.size() / 2.0, t, c::SKY);
                }
            }
        }
    }
    let states: HashMap<&str, (Tone, &'static str)> = g
        .pages
        .iter()
        .map(|p| {
            (p.slug.as_str(), {
                let (t, i, _) = page_state(p);
                (t, i)
            })
        })
        .collect();
    let pointer = resp.hover_pos();
    let mut hovered: Option<usize> = None;
    for (i, n) in gs.nodes.iter().enumerate() {
        let l = &laid[i];
        let on = sel.as_deref() == Some(n.id.as_str());
        let dim = !near.contains(&n.id);
        let alpha = if dim { 0.38 } else { 1.0 };
        let r = l.rect;
        if pointer.is_some_and(|p| r.contains(p)) {
            hovered = Some(i);
        }
        let (radius, bg, stroke) = match n.kind {
            Kind::Page => (7, if on { c::CONTROL_HOVER } else { c::CONTROL }, Stroke::NONE),
            Kind::Session => (13, if on { c::CONTROL_HOVER } else { c::NAV }, Stroke::new(1.0, c::LINE_STRONG)),
        };
        painter.rect(r, CornerRadius::same(radius), bg.gamma_multiply(alpha), stroke, egui::StrokeKind::Inside);
        if on {
            painter.rect_stroke(
                r.expand(3.0),
                CornerRadius::same(radius + 3),
                Stroke::new(1.5, c::TEXT),
                egui::StrokeKind::Inside,
            );
        }
        let (glyph, ic) = match n.kind {
            Kind::Page => states
                .get(n.id.trim_start_matches("p:"))
                .map(|(t, g)| (*g, if *t == Tone::Neutral { c::TEXT_3 } else { t.fg() }))
                .unwrap_or((icons::FILE_TEXT, c::TEXT_3)),
            Kind::Session => (icons::SQUARE_TERMINAL, c::TEXT_3),
        };
        painter.text(
            r.left_center() + Vec2::new(14.0, 0.0),
            Align2::CENTER_CENTER,
            glyph,
            icon_font(12.0),
            ic.gamma_multiply(alpha),
        );
        let fg = if n.kind == Kind::Page || on { c::TEXT_STRONG } else { c::TEXT_2 };
        painter.galley(
            Pos2::new(r.left() + 24.0, r.center().y - l.galley.size().y / 2.0),
            l.galley.clone(),
            fg.gamma_multiply(alpha),
        );
    }
    // Interaction.
    let resp = match hovered {
        Some(i) if gs.dragging.is_none() => {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            resp.on_hover_text_at_pointer(gs.nodes[i].label.clone())
        }
        _ => resp,
    };
    if resp.drag_started() {
        gs.dragging = hovered;
    }
    if resp.dragged() {
        let d = resp.drag_delta() / gs.zoom;
        match gs.dragging {
            Some(i) => {
                gs.nodes[i].pos += d;
                gs.nodes[i].pinned = true;
                gs.settled = false;
                gs.steps = gs.steps.min(MAX_STEPS - 120);
            }
            None => gs.pan += d,
        }
    }
    if resp.drag_stopped() {
        gs.dragging = None;
    }
    if resp.clicked()
        && let Some(i) = hovered
    {
        gs.selected = Some(gs.nodes[i].id.clone());
    }
    if resp.double_clicked()
        && let Some(i) = hovered
        && let Some(slug) = gs.nodes[i].id.strip_prefix("p:")
    {
        super::open_page(app, cx.project, slug);
    }
    if resp.hovered() {
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if zoom != 1.0 {
            let p = pointer.unwrap_or(center) - center;
            let world = p / gs.zoom - gs.pan;
            gs.zoom = (gs.zoom * zoom).clamp(0.3, 3.0);
            gs.pan = p / gs.zoom - world;
        } else if scroll != Vec2::ZERO {
            gs.pan += scroll / gs.zoom;
        }
    }
    // View controls.
    let btn = Rect::from_min_size(rect.right_top() + Vec2::new(-36.0, 8.0), Vec2::splat(26.0));
    let r = ui.interact(btn, ui.scope_id().with(("graph-fit", cx.tab)), Sense::click()).on_hover_text("전체 보기");
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "전체 보기"));
    if r.hovered() {
        painter.rect_filled(btn, CornerRadius::same(5), c::CONTROL);
    }
    painter.text(
        btn.center(),
        Align2::CENTER_CENTER,
        icons::MAXIMIZE_2,
        icon_font(14.0),
        if r.hovered() { c::TEXT_STRONG } else { c::TEXT_3 },
    );
    if r.clicked() || (gs.settled && !gs.fitted) {
        gs.fitted = true;
        fit(gs, rect);
    }
}

/// Pan and zoom so every node fits the canvas.
fn fit(gs: &mut GraphState, rect: Rect) {
    if gs.nodes.is_empty() {
        return;
    }
    let mut bounds = Rect::NOTHING;
    for n in &gs.nodes {
        bounds.extend_with(n.pos.to_pos2());
    }
    let size = bounds.size() + Vec2::new(220.0, 80.0);
    gs.zoom = (rect.width() / size.x).min(rect.height() / size.y).clamp(0.3, 1.6);
    gs.pan = -bounds.center().to_vec2();
}

fn legend(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let item = |ui: &mut egui::Ui, draw: &dyn Fn(&egui::Painter, Rect), text: &str| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::new(18.0, 12.0), Sense::hover());
                draw(ui.painter(), r);
                ui.label(RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_3));
            });
        };
        item(
            ui,
            &|p, r| {
                p.rect_filled(r.shrink(1.0), CornerRadius::same(3), c::CONTROL);
            },
            "문서",
        );
        item(
            ui,
            &|p, r| {
                p.rect_stroke(
                    r.shrink(1.0),
                    CornerRadius::same(6),
                    Stroke::new(1.0, c::LINE_STRONG),
                    egui::StrokeKind::Inside,
                );
            },
            "근거 세션",
        );
        item(
            ui,
            &|p, r| {
                p.line_segment([r.left_center(), r.right_center()], Stroke::new(1.6, c::TEXT_2));
            },
            "링크",
        );
        item(
            ui,
            &|p, r| {
                p.extend(Shape::dashed_line(&[r.left_center(), r.right_center()], Stroke::new(1.5, c::SKY), 4.0, 3.0));
            },
            "내용이 비슷함 (낱말 겹침, AI 아님)",
        );
        ui.label(RichText::new("더블클릭하면 문서를 열어요").font(font(ty::SMALL)).color(c::TEXT_3));
    });
}

fn aside(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, gs: &mut GraphState, g: &Graph, edges: &[&Edge]) {
    let Some(sel) = gs.selected.clone() else {
        return;
    };
    theme::card_frame().inner_margin(egui::Margin::same(0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new().inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            selected_card(app, ui, cx, g, &sel);
        });
        let conns: Vec<&&Edge> = edges
            .iter()
            .filter(|e| {
                let (a, b) = edge_ends(e);
                a == sel || b == sel
            })
            .collect();
        ui.separator();
        egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("연결").font(semibold(12.5)).color(c::TEXT_STRONG));
                ui.label(RichText::new(format!("{}개", conns.len())).font(font(12.0)).color(c::TEXT_3));
            });
        });
        if conns.is_empty() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.label(RichText::new("켜 둔 연결 종류로는 이어진 곳이 없어요.").font(font(12.5)).color(c::TEXT_3));
            });
        }
        for e in conns {
            ui.separator();
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                connection(app, ui, cx, gs, g, e, &sel);
            });
        }
    });
    ui.add_space(8.0);
    ui.label(
        RichText::new("‘내용이 비슷한 문서’는 AI가 아니라 이 컴퓨터에서 문서 낱말이 얼마나 겹치는지로 찾아요. 확인하기 전까지 제안일 뿐이에요.")
            .font(font(ty::SMALL))
            .color(c::TEXT_3),
    );
}

fn selected_card(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, g: &Graph, sel: &str) {
    if let Some(slug) = sel.strip_prefix("p:") {
        let Some(p) = g.pages.iter().find(|p| p.slug == slug) else {
            return;
        };
        let (tone, glyph, state) = page_state(p);
        ui.horizontal(|ui| {
            ui.label(theme::icon(icons::FILE_TEXT, 12.0, c::TEXT_3));
            ui.label(RichText::new(format!("문서 · {}", cx.project_name)).font(font(11.5)).color(c::TEXT_3));
        });
        ui.add(egui::Label::new(RichText::new(&p.title).font(semibold(16.0)).color(c::TEXT_STRONG)).wrap());
        let fg = if tone == Tone::Neutral { c::TEXT_3 } else { tone.fg() };
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::icon(glyph, 12.0, fg));
            ui.label(RichText::new(format!("{state} · {} 수정", when(p.updated_at))).font(font(12.0)).color(fg));
        });
        ui.add_space(4.0);
        let review = p.open_proposals > 0 || p.status == PageStatus::Draft;
        let label = if p.open_proposals > 0 {
            "제안 검토"
        } else if p.status == PageStatus::Draft {
            "초안 검토"
        } else {
            "문서 열기"
        };
        let clicked = if review {
            theme::primary_button(ui, label).clicked()
        } else {
            theme::secondary_button(ui, label).clicked()
        };
        if clicked {
            super::open_page(app, cx.project, slug);
        }
    } else if let Some(id) = sel.strip_prefix("s:") {
        let s = g.sessions.iter().find(|s| s.id.to_string() == id);
        ui.horizontal(|ui| {
            ui.label(theme::icon(icons::SQUARE_TERMINAL, 12.0, c::TEXT_3));
            ui.label(RichText::new(format!("대화 · {}", cx.project_name)).font(font(11.5)).color(c::TEXT_3));
        });
        let title = s.map(|s| s.title.clone()).unwrap_or_else(|| "세션".into());
        ui.add(egui::Label::new(RichText::new(&title).font(semibold(16.0)).color(c::TEXT_STRONG)).wrap());
        match s {
            Some(s) if s.exists => {
                if let Some(at) = s.created_at {
                    ui.label(RichText::new(format!("{} 시작", when(at))).font(font(12.0)).color(c::TEXT_3));
                }
                if let Ok(uuid) = id.parse::<uuid::Uuid>()
                    && theme::secondary_button(ui, "원문 보기").clicked()
                {
                    crate::views::history::open_session_records(app, cx.host, uuid);
                }
            }
            _ => {
                ui.label(
                    RichText::new("이 컴퓨터에 세션 기록이 없어요 (다른 컴퓨터에서 가져온 근거일 수 있어요).")
                        .font(font(12.0))
                        .color(c::TEXT_3),
                );
            }
        }
    }
}

fn node_title(g: &Graph, id: &str) -> String {
    if let Some(slug) = id.strip_prefix("p:") {
        return g.pages.iter().find(|p| p.slug == slug).map(|p| p.title.clone()).unwrap_or_else(|| slug.to_string());
    }
    let sid = id.trim_start_matches("s:");
    g.sessions.iter().find(|s| s.id.to_string() == sid).map(|s| s.title.clone()).unwrap_or_else(|| "세션".into())
}

fn connection(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, gs: &mut GraphState, g: &Graph, e: &Edge, sel: &str) {
    let (a, b) = edge_ends(e);
    let other = if a == sel { b } else { a };
    let (glyph, label, why, col) = match e.kind {
        EdgeKind::Link => {
            let (label, why) = link_direction(g, e, sel);
            (icons::LINK, label, why.to_string(), c::TEXT_3)
        }
        EdgeKind::Source => (icons::SQUARE_TERMINAL, "근거", "이 문서가 근거로 삼은 대화예요".to_string(), c::TEXT_3),
        EdgeKind::Similar => {
            let pct = e.score.map(|s| format!(" {:.0}%", s * 100.0)).unwrap_or_default();
            let words =
                if e.shared.is_empty() { String::new() } else { format!("겹치는 낱말: {}", e.shared.join(", ")) };
            (icons::FILE_SEARCH, "내용이 비슷함", format!("{pct} · 확인 전 · {words}"), c::SKY)
        }
    };
    ui.horizontal_top(|ui| {
        ui.label(theme::icon(glyph, 13.0, c::TEXT_3));
        ui.vertical(|ui| {
            let title = node_title(g, &other);
            if ui
                .add(
                    egui::Label::new(RichText::new(&title).font(medium(13.0)).color(c::TEXT_STRONG))
                        .sense(Sense::click()),
                )
                .clicked()
            {
                gs.selected = Some(other.clone());
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(RichText::new(label).font(font(12.0)).color(col));
                ui.label(RichText::new(format!(" · {why}")).font(font(12.0)).color(c::TEXT_2));
            });
            if e.kind == EdgeKind::Similar {
                ui.horizontal(|ui| {
                    let key = pair(&e.from, &e.to);
                    let enabled = cx.connected;
                    if ui
                        .add_enabled_ui(enabled, |ui| fmt::small_button(ui, "링크로 연결"))
                        .inner
                        .on_hover_text("고른 문서 끝에 이 문서로 가는 링크를 넣어 저장해요 (새 버전으로 남아요)")
                        .clicked()
                    {
                        gs.decided.insert(key.clone());
                        let from = sel.trim_start_matches("p:").to_string();
                        let to = other.trim_start_matches("p:").to_string();
                        let req = PagePair { project_id: cx.project, from, to };
                        app.hub.call::<WikiLink>(cx.host, req, |app, res| match res {
                            Ok(p) => app.toast(
                                crate::state::ToastKind::Success,
                                format!("“{}”에 링크를 넣었어요", p.summary.title),
                            ),
                            Err(e) => app.toast_error(&e.message),
                        });
                    }
                    if ui.add_enabled_ui(enabled, |ui| fmt::quiet_link(ui, "관련 없음")).inner.clicked() {
                        gs.decided.insert(key);
                        let req = PagePair { project_id: cx.project, from: e.from.clone(), to: e.to.clone() };
                        app.hub.call::<WikiDismiss>(cx.host, req, |app, res| {
                            if let Err(e) = res {
                                app.toast_error(&e.message);
                            }
                        });
                    }
                });
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(from: &str, to: &str) -> Edge {
        Edge { kind: EdgeKind::Link, from: from.into(), to: to.into(), score: None, shared: vec![] }
    }

    #[test]
    fn mutual_links_are_one_connection() {
        let g = Graph {
            pages: vec![],
            sessions: vec![],
            edges: vec![link("auth", "expiry"), link("expiry", "auth"), link("auth", "deploy")],
            broken: vec![],
        };
        let gs = GraphState::default();
        assert_eq!(visible_edges(&g, &gs).len(), 2);
        assert_eq!(link_pairs(&g), 2);
        assert_eq!(link_direction(&g, &g.edges[0], "p:auth").0, "서로 링크");
        assert_eq!(link_direction(&g, &g.edges[2], "p:auth").0, "링크");
        assert_eq!(link_direction(&g, &g.edges[2], "p:deploy").0, "받은 링크");
    }
}
