//! Project wiki: Markdown pages kept as files under `RUNWICK_HOME/wiki/<project>/<slug>.md`
//! with a small frontmatter, indexed by the host with every saved version.
//!
//! Pages are addressed by `(project_id, slug)`. Saves name the version they started from;
//! a stale base returns [`SaveOutcome::Conflict`] with both texts and never overwrites.
//! AI work never edits a page directly: it leaves [`Proposal`]s (or, when the person asked for
//! a draft, a new page marked as an AI draft). Proposals made against an older version are
//! re-compared with the current text before anything is applied.

use crate::Ts;
use crate::api::projects::ProjectId;
use crate::api::sessions::SessionId;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type ProposalId = Uuid;

/// Who wrote the page first.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    #[default]
    User,
    /// Created by an AI job (a draft until the person reviews it).
    Ai,
    /// Brought in from existing Markdown files.
    Import,
}

/// Review state of the page text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    /// AI draft nobody has reviewed yet.
    Draft,
    #[default]
    Reviewed,
}

/// Who produced a saved version.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SavedBy {
    User,
    /// The file changed outside Runwick (treated as the person's edit).
    External,
    Ai,
    Import,
    /// A proposal (or part of it) the person applied.
    Proposal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PageSummary {
    pub project_id: ProjectId,
    pub slug: String,
    pub title: String,
    pub created_by: Author,
    pub status: PageStatus,
    pub tags: Vec<String>,
    /// Sessions the page is based on.
    pub sources: Vec<SessionId>,
    pub version: u32,
    pub updated_at: Ts,
    /// First meaningful line of the body, for lists.
    pub excerpt: String,
    pub open_proposals: u32,
    /// Open proposals flagged "결정 확인 필요".
    pub flagged_proposals: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Page {
    pub summary: PageSummary,
    /// Markdown body without the frontmatter.
    pub body: String,
    /// The file on the host, for "폴더 열기" and display.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WikiQuery {
    /// None lists every project's pages on this host.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PageRef {
    pub project_id: ProjectId,
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreatePage {
    pub project_id: ProjectId,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Sessions the page is based on (a page started from a message or pin).
    #[serde(default)]
    pub sources: Vec<SessionId>,
}

/// One Markdown file read on the client (works for remote hosts too).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImportFile {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImportPages {
    pub project_id: ProjectId,
    pub files: Vec<ImportFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ImportReport {
    pub created: Vec<String>,
    /// File names that were skipped with the reason.
    pub skipped: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavePage {
    pub project_id: ProjectId,
    pub slug: String,
    /// The version the edit started from.
    pub base_version: u32,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Mark an AI draft as reviewed with this save.
    #[serde(default)]
    pub mark_reviewed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SaveOutcome {
    Saved {
        page: Page,
    },
    /// Someone saved (or the file changed) after `base_version`. Nothing was written.
    Conflict {
        current: Page,
        yours_title: String,
        yours_body: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    pub version: u32,
    pub title: String,
    pub saved_by: SavedBy,
    pub note: Option<String>,
    pub saved_at: Ts,
    /// Lines added/removed compared with the previous version.
    pub added: u32,
    pub removed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VersionRef {
    pub project_id: ProjectId,
    pub slug: String,
    pub version: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Same,
    Added,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionText {
    pub info: VersionInfo,
    pub body: String,
    /// Compared with the previous version (all `Added` for the first one).
    pub diff: Vec<DiffLine>,
}

// ---------------------------------------------------------------------------------------------
// Links and graph

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// `[[wikilink]]` or a Markdown link to another page.
    Link,
    /// Page → session it is based on.
    Source,
    /// Pages whose words overlap (local keyword similarity, not AI). A suggestion only.
    Similar,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionNode {
    pub id: SessionId,
    pub title: String,
    pub created_at: Option<Ts>,
    /// The session record still exists on this host.
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub kind: EdgeKind,
    /// Page slug.
    pub from: String,
    /// Page slug, or the session id for `Source`.
    pub to: String,
    /// Similarity in 0..1 for `Similar`.
    #[serde(default)]
    pub score: Option<f32>,
    /// Words both pages share (for `Similar`), for an honest explanation.
    #[serde(default)]
    pub shared: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Graph {
    pub pages: Vec<PageSummary>,
    pub sessions: Vec<SessionNode>,
    pub edges: Vec<Edge>,
    /// Links whose target page does not exist (`[[없는 문서]]`).
    pub broken: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PagePair {
    pub project_id: ProjectId,
    pub from: String,
    pub to: String,
}

// ---------------------------------------------------------------------------------------------
// Proposals

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Open,
    /// Kept for later ("보류").
    Held,
    Applied,
    /// Some hunks were applied.
    Partial,
    Rejected,
}

impl ProposalStatus {
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Applied | Self::Partial | Self::Rejected)
    }
}

/// A conversation message a proposal is based on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceRef {
    pub session_id: SessionId,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub at: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    pub id: ProposalId,
    pub project_id: ProjectId,
    /// Target page; None proposes a new page.
    pub slug: Option<String>,
    pub title: String,
    pub rationale: String,
    pub sources: Vec<SourceRef>,
    /// Why it may contradict a recorded decision ("결정 확인 필요"), if so.
    pub decision_flag: Option<String>,
    pub status: ProposalStatus,
    /// Page version the proposal was written against.
    pub base_version: Option<u32>,
    /// The page changed after the proposal was made: re-compared with the current text.
    pub needs_rebase: bool,
    pub created_at: Ts,
    pub decided_at: Option<Ts>,
    /// Job that produced it.
    pub job_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HunkKind {
    /// Applies cleanly to the current text.
    Clean,
    /// Touches lines the person changed after the proposal was made. Never applied.
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hunk {
    pub index: u32,
    pub kind: HunkKind,
    /// Lines of the current text the hunk replaces (for `Conflict`: the person's current lines).
    pub old: Vec<String>,
    pub new: Vec<String>,
    /// Up to two unchanged lines before the change, for orientation.
    pub context_before: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProposalDetail {
    pub proposal: Proposal,
    /// The page's current version the hunks were computed against (None for a new page).
    pub against_version: Option<u32>,
    pub hunks: Vec<Hunk>,
    /// Full proposed body (for new pages and for reading).
    pub proposed_body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProposalQuery {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub slug: Option<String>,
    /// Include applied/rejected ones.
    #[serde(default)]
    pub include_closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApplyProposal {
    pub id: ProposalId,
    /// The version the person reviewed the hunks against. A newer page refuses the apply.
    #[serde(default)]
    pub against_version: Option<u32>,
    /// Hunks to apply; None applies every clean hunk.
    #[serde(default)]
    pub hunks: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProposalAction {
    pub id: ProposalId,
}

/// Readable file-name slug: letters and digits (Korean included) joined by `-`.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in crate::nfc::hangul(title.trim()).chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(ch);
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '.' | '/' | '·') {
            dash = true;
        }
        if out.chars().count() >= 64 {
            break;
        }
    }
    if out.is_empty() { "page".to_string() } else { out }
}

// ---------------------------------------------------------------------------------------------
// Methods

method!(WikiList, "wiki.list", WikiQuery => Vec<PageSummary>, [topic::WIKI]);
method!(WikiGet, "wiki.get", PageRef => Page, [topic::WIKI]);
method!(WikiCreate, "wiki.create", CreatePage => Page);
method!(WikiImport, "wiki.import", ImportPages => ImportReport);
method!(WikiSave, "wiki.save", SavePage => SaveOutcome);
method!(WikiHistory, "wiki.history", PageRef => Vec<VersionInfo>, [topic::WIKI]);
method!(WikiVersion, "wiki.version", VersionRef => VersionText, [topic::WIKI]);
method!(WikiGraph, "wiki.graph", WikiQuery => Graph, [topic::WIKI, topic::SESSIONS]);
// Insert `[[to]]` into `from` through a normal save (new version).
method!(WikiLink, "wiki.link", PagePair => Page);
// Remember that a similarity suggestion is not related.
method!(WikiDismiss, "wiki.dismiss", PagePair => ());
method!(WikiProposals, "wiki.proposals", ProposalQuery => Vec<Proposal>, [topic::WIKI]);
method!(WikiProposalGet, "wiki.proposal", ProposalAction => ProposalDetail, [topic::WIKI]);
method!(WikiApply, "wiki.apply", ApplyProposal => Page);
method!(WikiHold, "wiki.hold", ProposalAction => Proposal);
method!(WikiReject, "wiki.reject", ProposalAction => Proposal);
