//! Line diffs for history and proposals, and re-comparing a proposal with a page that changed
//! after the proposal was written.
//!
//! A proposal is the change `base → proposed`. When the page is now `current ≠ base`, each
//! proposed change is carried onto `current` only where the lines it touches are still exactly
//! the base lines; a change that overlaps the person's later edit becomes a `Conflict` hunk,
//! shown for reading and never applied.

use runwick_core::api::wiki::{DiffLine, Hunk, HunkKind, LineKind};
use similar::{Algorithm, DiffOp, capture_diff_slices};
use std::ops::Range;

pub fn split(text: &str) -> Vec<&str> {
    text.lines().collect()
}

pub fn join(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

fn ops(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    capture_diff_slices(Algorithm::Myers, old, new)
}

pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let (a, b) = (split(old), split(new));
    let mut out = Vec::new();
    let push = |out: &mut Vec<DiffLine>, kind, lines: &[&str]| {
        out.extend(lines.iter().map(|l| DiffLine { kind, text: l.to_string() }));
    };
    for op in ops(&a, &b) {
        let (_, o, n) = op.as_tag_tuple();
        match op {
            DiffOp::Equal { .. } => push(&mut out, LineKind::Same, &a[o]),
            DiffOp::Delete { .. } => push(&mut out, LineKind::Removed, &a[o]),
            DiffOp::Insert { .. } => push(&mut out, LineKind::Added, &b[n]),
            DiffOp::Replace { .. } => {
                push(&mut out, LineKind::Removed, &a[o]);
                push(&mut out, LineKind::Added, &b[n]);
            }
        }
    }
    out
}

/// (added, removed) line counts.
pub fn counts(old: &str, new: &str) -> (u32, u32) {
    let (a, b) = (split(old), split(new));
    ops(&a, &b).iter().fold((0, 0), |(add, rem), op| {
        let (_, o, n) = op.as_tag_tuple();
        match op {
            DiffOp::Equal { .. } => (add, rem),
            _ => (add + n.len() as u32, rem + o.len() as u32),
        }
    })
}

/// A hunk with where it lands in the current text.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub hunk: Hunk,
    /// Lines of the current text replaced by `hunk.new` (empty range = insertion point).
    pub at: Range<usize>,
}

/// Hunks that turn `current` into the proposal, re-based when `current` moved on from `base`.
pub fn rebase(base: &str, current: &str, proposed: &str) -> Vec<Placed> {
    let (b, c, p) = (split(base), split(current), split(proposed));
    // base line → current line when the person left it untouched.
    let mut map: Vec<Option<usize>> = vec![None; b.len()];
    for op in ops(&b, &c) {
        if let DiffOp::Equal { old_index, new_index, len } = op {
            for k in 0..len {
                map[old_index + k] = Some(new_index + k);
            }
        }
    }
    let mut out = Vec::new();
    for op in ops(&b, &p) {
        let (_, br, pr) = op.as_tag_tuple();
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        let new: Vec<String> = p[pr].iter().map(|s| s.to_string()).collect();
        let (kind, at) = match place(&map, &br, c.len()) {
            Some(at) => (HunkKind::Clean, at),
            None => (HunkKind::Conflict, region(&map, &br, c.len())),
        };
        let old: Vec<String> = c[at.clone()].iter().map(|s| s.to_string()).collect();
        if old == new {
            continue; // the page already says this
        }
        let ctx_from = at.start.saturating_sub(2);
        let context_before = c[ctx_from..at.start].iter().map(|s| s.to_string()).collect();
        let index = out.len() as u32;
        out.push(Placed { hunk: Hunk { index, kind, old, new, context_before }, at });
    }
    out
}

/// Where a change of base lines `br` lands in the current text, if every line it touches
/// (and, for an insertion, a neighbour) is unchanged.
fn place(map: &[Option<usize>], br: &Range<usize>, cur_len: usize) -> Option<Range<usize>> {
    if br.is_empty() {
        let s = br.start;
        if s == 0 {
            return Some(0..0);
        }
        if let Some(prev) = map[s - 1] {
            return Some(prev + 1..prev + 1);
        }
        if s == map.len() {
            return None;
        }
        return map[s].map(|n| n..n);
    }
    let first = map[br.start]?;
    let mut last = first;
    for m in &map[br.start + 1..br.end] {
        let m = (*m)?;
        if m != last + 1 {
            return None; // the person inserted lines inside the range
        }
        last = m;
    }
    debug_assert!(last < cur_len);
    Some(first..last + 1)
}

/// Current lines between the nearest untouched base lines around `br` (the person's version
/// of the region a conflicting change wanted to edit).
fn region(map: &[Option<usize>], br: &Range<usize>, cur_len: usize) -> Range<usize> {
    let start = map[..br.start].iter().rev().find_map(|m| *m).map(|m| m + 1).unwrap_or(0);
    let end = map[br.end..].iter().find_map(|m| *m).unwrap_or(cur_len);
    start..end.max(start)
}

/// Apply the chosen clean hunks to `current`. Conflict hunks are never applied.
pub fn apply(current: &str, placed: &[Placed], chosen: &[u32]) -> (String, usize) {
    let mut lines: Vec<String> = split(current).iter().map(|s| s.to_string()).collect();
    let mut picked: Vec<&Placed> =
        placed.iter().filter(|p| p.hunk.kind == HunkKind::Clean && chosen.contains(&p.hunk.index)).collect();
    picked.sort_by_key(|p| std::cmp::Reverse(p.at.start));
    for p in &picked {
        lines.splice(p.at.clone(), p.hunk.new.iter().cloned());
    }
    (join(&lines), picked.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_all(placed: &[Placed]) -> Vec<u32> {
        placed.iter().filter(|p| p.hunk.kind == HunkKind::Clean).map(|p| p.hunk.index).collect()
    }

    #[test]
    fn unchanged_page_takes_the_whole_proposal() {
        let base = "# 로그인\n만료는 Date.now() 기준\n갱신은 만료 전\n";
        let proposed = "# 로그인\n만료는 서버 시각 기준\n갱신은 만료 전\n실패하면 로그인 화면\n";
        let placed = rebase(base, base, proposed);
        assert_eq!(placed.len(), 2);
        assert!(placed.iter().all(|p| p.hunk.kind == HunkKind::Clean));
        let (out, n) = apply(base, &placed, &clean_all(&placed));
        assert_eq!(n, 2);
        assert_eq!(out, proposed);
    }

    #[test]
    fn later_edits_are_kept_and_overlaps_become_conflicts() {
        let base = "a\nb\nc\nd\ne\n";
        // The person edited `b` and added a line after `d` after the proposal was made.
        let current = "a\nB (내가 고침)\nc\nd\nd2\ne\n";
        // The proposal rewrote `b` (overlaps the person's edit) and `e` (untouched).
        let proposed = "a\nb 제안\nc\nd\ne 제안\n";
        let placed = rebase(base, current, proposed);
        let conflict: Vec<_> = placed.iter().filter(|p| p.hunk.kind == HunkKind::Conflict).collect();
        let clean: Vec<_> = placed.iter().filter(|p| p.hunk.kind == HunkKind::Clean).collect();
        assert_eq!(conflict.len(), 1);
        assert_eq!(conflict[0].hunk.old, vec!["B (내가 고침)"]);
        assert_eq!(conflict[0].hunk.new, vec!["b 제안"]);
        assert_eq!(clean.len(), 1);
        assert_eq!(clean[0].hunk.old, vec!["e"]);
        // Choosing everything still never touches the person's line.
        let all: Vec<u32> = placed.iter().map(|p| p.hunk.index).collect();
        let (out, n) = apply(current, &placed, &all);
        assert_eq!(n, 1);
        assert_eq!(out, "a\nB (내가 고침)\nc\nd\nd2\ne 제안\n");
    }

    #[test]
    fn partial_apply_and_already_present_changes() {
        let base = "1\n2\n3\n";
        let current = "1\n2 이미 반영\n3\n";
        let proposed = "0\n1\n2 이미 반영\n3\n4\n";
        let placed = rebase(base, current, proposed);
        // The change to line 2 is already in the page, so only the two insertions remain.
        assert_eq!(placed.len(), 2);
        assert!(placed.iter().all(|p| p.hunk.kind == HunkKind::Clean));
        let (out, _) = apply(current, &placed, &[placed[1].hunk.index]);
        assert_eq!(out, "1\n2 이미 반영\n3\n4\n");
    }

    #[test]
    fn history_diff_marks_lines() {
        let d = line_diff("가\n나\n", "가\n다\n");
        let kinds: Vec<_> = d.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![LineKind::Same, LineKind::Removed, LineKind::Added]);
        assert_eq!(counts("가\n나\n", "가\n다\n라\n"), (2, 1));
    }
}
