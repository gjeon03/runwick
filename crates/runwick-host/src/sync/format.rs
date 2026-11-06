//! The data folder format (v1): canonical JSON files, the manifest, and the three-way merge of
//! JSON records.
//!
//! Files are written deterministically (sorted keys, two-space indent, trailing newline) so the
//! same records always produce the same bytes and git only sees real changes.

use runwick_core::Ts;
use runwick_core::api::sync::{FORMAT, FORMAT_VERSION, RepoContent};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MANIFEST: &str = "manifest.json";
pub const README: &str = "README.md";

/// Folder of tombstones: `tombstones/<kind>/<key>.json`.
pub const TOMBSTONES: &str = "tombstones";

/// Canonical text of a JSON value: keys sorted (serde_json maps are ordered), pretty, newline.
pub fn canonical(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(&sorted(v)).unwrap_or_else(|_| "null".into());
    s.push('\n');
    s
}

fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let b: BTreeMap<&String, Value> = m.iter().map(|(k, v)| (k, sorted(v))).collect();
            Value::Object(b.into_iter().map(|(k, v)| (k.clone(), v)).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

pub fn parse(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

/// Which side wins a field changed on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefer {
    /// The record (or nested record) with the newer `updated_at`; this computer's on a tie.
    Newer,
    /// This computer's (after a backup restore, the restored records win once).
    Local,
}

/// Three-way merge of one record. `base` is the version both sides last agreed on (None when
/// unknown or absent). Fields changed on one side win; a field changed on both sides is
/// decided by `prefer`. A record deleted on one side and changed on the other comes back.
pub fn merge3(base: Option<&Value>, local: Option<&Value>, remote: Option<&Value>, prefer: Prefer) -> Option<Value> {
    if local == remote {
        return local.cloned();
    }
    if base == local {
        return remote.cloned();
    }
    if base == remote {
        return local.cloned();
    }
    match (local, remote) {
        (None, Some(r)) => Some(r.clone()),
        (Some(l), None) => Some(l.clone()),
        (Some(l), Some(r)) => {
            let prefer_local = prefer == Prefer::Local || !newer(r, l);
            Some(merge_value(base, l, r, prefer_local, prefer))
        }
        (None, None) => None,
    }
}

fn merge_value(base: Option<&Value>, l: &Value, r: &Value, prefer_local: bool, prefer: Prefer) -> Value {
    if l == r {
        return l.clone();
    }
    if base == Some(l) {
        return r.clone();
    }
    if base == Some(r) {
        return l.clone();
    }
    match (l, r) {
        (Value::Object(lm), Value::Object(rm)) => {
            // A nested record with its own time (one variable of the environment) decides
            // its own conflicts.
            let prefer_local =
                if prefer == Prefer::Newer && stamped(l) && stamped(r) { !newer(r, l) } else { prefer_local };
            let bm = base.and_then(Value::as_object);
            let mut keys: Vec<&String> = lm.keys().chain(rm.keys()).collect();
            keys.sort();
            keys.dedup();
            let mut out = Map::new();
            for k in keys {
                let b = bm.and_then(|m| m.get(k));
                let merged = match (lm.get(k), rm.get(k)) {
                    (Some(lv), Some(rv)) => Some(merge_value(b, lv, rv, prefer_local, prefer)),
                    (Some(lv), None) => (b != Some(lv) || bm.is_none()).then(|| lv.clone()),
                    (None, Some(rv)) => (b != Some(rv) || bm.is_none()).then(|| rv.clone()),
                    (None, None) => None,
                };
                if let Some(v) = merged {
                    out.insert(k.clone(), v);
                }
            }
            Value::Object(out)
        }
        _ if prefer_local => l.clone(),
        _ => r.clone(),
    }
}

/// Fields that only say when something changed (their loss is not a loss of content).
const TIME_FIELDS: [&str; 2] = ["updated_at", "generated_at"];

/// Whether the merge dropped content changed on this computer / on the other: a field changed
/// since `base` on that side whose value did not make it into `merged` (nested records are
/// compared field by field; time stamps do not count).
pub fn lost_sides(base: Option<&Value>, local: &Value, remote: &Value, merged: &Value) -> (bool, bool) {
    lost_in(base, Some(local), Some(remote), Some(merged))
}

fn lost_in(b: Option<&Value>, l: Option<&Value>, r: Option<&Value>, m: Option<&Value>) -> (bool, bool) {
    if let (Some(Value::Object(lm)), Some(Value::Object(rm)), Some(Value::Object(mm))) = (l, r, m) {
        let bm = b.and_then(Value::as_object);
        let mut lost = (false, false);
        for k in lm.keys().chain(rm.keys()) {
            if TIME_FIELDS.contains(&k.as_str()) {
                continue;
            }
            let x = lost_in(bm.and_then(|x| x.get(k)), lm.get(k), rm.get(k), mm.get(k));
            lost = (lost.0 || x.0, lost.1 || x.1);
        }
        return lost;
    }
    (l != b && m != l, r != b && m != r)
}

/// `a` was changed later than `b` (`updated_at`, or `generated_at` for summaries).
pub fn newer(a: &Value, b: &Value) -> bool {
    let at = |v: &Value| ts_field(v, "updated_at").or_else(|| ts_field(v, "generated_at"));
    match (at(a), at(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

fn stamped(v: &Value) -> bool {
    ts_field(v, "updated_at").is_some() || ts_field(v, "generated_at").is_some()
}

pub fn ts_field(v: &Value, key: &str) -> Option<Ts> {
    v.get(key)?.as_str().and_then(parse_ts)
}

/// Change times from another computer are never later than now: a time in the future would win
/// every later comparison. Nested records (one variable of the environment) are clamped too.
pub fn clamp_future(v: &mut Value) {
    let now = runwick_core::now();
    if let Value::Object(m) = v {
        for (k, x) in m.iter_mut() {
            if TIME_FIELDS.contains(&k.as_str()) {
                if x.as_str().and_then(parse_ts).is_some_and(|t| t > now) {
                    *x = Value::String(crate::store::ts_to_sql(&now));
                }
            } else if x.is_object() {
                clamp_future(x);
            }
        }
    }
}

pub fn parse_ts(s: &str) -> Option<Ts> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&chrono::Utc))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceEntry {
    pub name: String,
    #[serde(default)]
    pub last_sync: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub devices: BTreeMap<Uuid, DeviceEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

impl Manifest {
    pub fn new() -> Self {
        Self {
            format: FORMAT.into(),
            version: FORMAT_VERSION,
            app_version: runwick_core::VERSION.into(),
            devices: BTreeMap::new(),
        }
    }

    pub fn render(&self) -> String {
        canonical(&serde_json::to_value(self).unwrap_or(Value::Null))
    }
}

/// What a repository tree holds, from its file list and manifest text.
pub fn content_of(files: &[String], manifest: Option<&[u8]>) -> (RepoContent, Option<Manifest>) {
    if files.is_empty() {
        return (RepoContent::Empty, None);
    }
    let Some(bytes) = manifest else {
        // Only a README (for example GitHub's "initialize with README") is still empty enough.
        let only_readme = files.iter().all(|f| f.eq_ignore_ascii_case(README) || f == ".gitignore" || f == "LICENSE");
        return (if only_readme { RepoContent::Empty } else { RepoContent::Other }, None);
    };
    let value = parse(bytes);
    let format = value.as_ref().and_then(|v| v.get("format")).and_then(Value::as_str);
    if format != Some(FORMAT) {
        return (RepoContent::Other, None);
    }
    let version = value.as_ref().and_then(|v| v.get("version")).and_then(Value::as_u64).unwrap_or(0);
    if version > FORMAT_VERSION as u64 {
        return (RepoContent::Newer, None);
    }
    match value.and_then(|v| serde_json::from_value::<Manifest>(v).ok()) {
        Some(m) => (RepoContent::Runwick, Some(m)),
        None => (RepoContent::Other, None),
    }
}

pub const README_TEXT: &str = "# Runwick 기록\n\n\
이 저장소는 Runwick이 컴퓨터 사이에서 기록을 이어 쓰려고 관리하는 데이터 폴더예요.\n\
파일을 직접 고치지 말고 Runwick의 설정 › 백업·동기화에서 동기화해 주세요.\n\n\
- 비밀값, CLI 원본 대화 파일, 터미널 화면 기록은 들어 있지 않아요.\n\
- 저장소는 비공개로 유지해 주세요. 대화 원문이 들어 있어요.\n";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_text_is_stable() {
        let a = canonical(&json!({"b": 1, "a": {"y": 2, "x": [3, {"d": 1, "c": 2}]}}));
        let b = canonical(&json!({"a": {"x": [3, {"c": 2, "d": 1}], "y": 2}, "b": 1}));
        assert_eq!(a, b);
        assert!(a.ends_with("}\n"));
    }

    #[test]
    fn merge_keeps_both_sides_fields_and_newer_wins_on_both_changed() {
        let n = Prefer::Newer;
        let base = json!({"name": "앱", "favorite": false, "roots": {"a": "/x"}, "updated_at": "2026-01-01T00:00:00Z"});
        let local = json!({"name": "앱", "favorite": true, "roots": {"a": "/x", "b": "/y"}, "updated_at": "2026-01-02T00:00:00Z"});
        let remote = json!({"name": "새 이름", "favorite": false, "roots": {"a": "/x", "c": "/z"}, "updated_at": "2026-01-03T00:00:00Z"});
        let m = merge3(Some(&base), Some(&local), Some(&remote), n).unwrap();
        assert_eq!(m["name"], "새 이름");
        assert_eq!(m["favorite"], true);
        assert_eq!(m["roots"], json!({"a": "/x", "b": "/y", "c": "/z"}));
        assert_eq!(m["updated_at"], "2026-01-03T00:00:00Z");
        // Different fields: nothing is lost on either side.
        assert_eq!(lost_sides(Some(&base), &local, &remote, &m), (false, false));
        // Same field changed on both sides: the newer record wins, the other side lost content.
        let l2 = json!({"title": "A", "updated_at": "2026-01-05T00:00:00Z"});
        let r2 = json!({"title": "B", "updated_at": "2026-01-04T00:00:00Z"});
        let b2 = json!({"title": "0", "updated_at": "2026-01-01T00:00:00Z"});
        let m2 = merge3(Some(&b2), Some(&l2), Some(&r2), n).unwrap();
        assert_eq!(m2["title"], "A");
        assert_eq!(lost_sides(Some(&b2), &l2, &r2, &m2), (false, true));
        assert_eq!(merge3(Some(&b2), Some(&r2), Some(&l2), n).unwrap()["title"], "A");
        // After a restore this computer's version wins even when older.
        assert_eq!(merge3(Some(&b2), Some(&r2), Some(&l2), Prefer::Local).unwrap()["title"], "B");
        // Deleted here, changed there: it comes back. Deleted there, unchanged here: gone.
        assert_eq!(merge3(Some(&b2), None, Some(&r2), n), Some(r2.clone()));
        assert_eq!(merge3(Some(&b2), Some(&b2), None, n), None);
        // Unknown base (first sync): union of keys, newer wins on differences.
        let m = merge3(None, Some(&json!({"k": {"A": 1}})), Some(&json!({"k": {"B": 2}})), n).unwrap();
        assert_eq!(m, json!({"k": {"A": 1, "B": 2}}));
    }

    #[test]
    fn repository_content_is_recognized() {
        assert_eq!(content_of(&[], None).0, RepoContent::Empty);
        assert_eq!(content_of(&["README.md".into()], None).0, RepoContent::Empty);
        assert_eq!(content_of(&["src/main.rs".into()], None).0, RepoContent::Other);
        let m = Manifest::new().render();
        assert_eq!(content_of(&["manifest.json".into()], Some(m.as_bytes())).0, RepoContent::Runwick);
        let newer = r#"{"format":"runwick-sync","version":99}"#;
        assert_eq!(content_of(&["manifest.json".into()], Some(newer.as_bytes())).0, RepoContent::Newer);
    }
}
