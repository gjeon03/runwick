//! Tombstones: `tombstones/<kind>/<key>.json` says a record was removed, when and where. They
//! let a computer that joins later (or restores an older state) remove what others removed,
//! unless it changed the record after the removal.

use super::format::{self, canonical};
use super::kinds::{pins, roles, wiki};
use runwick_core::Ts;
use serde_json::json;
use uuid::Uuid;

pub const DIR: &str = format::TOMBSTONES;

/// Tombstone path for a record path (only kinds that can be removed have one).
pub fn for_entity(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [pins::DIR, file] | [roles::DIR, file] if file.ends_with(".json") => Some(format!("{DIR}/{path}")),
        [wiki::DIR, project, file] => {
            file.strip_suffix(".md").map(|slug| format!("{DIR}/{}/{project}/{slug}.json", wiki::DIR))
        }
        _ => None,
    }
}

/// Record path of a tombstone path.
pub fn entity_of(path: &str) -> Option<String> {
    let rest = path.strip_prefix(DIR)?.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [pins::DIR, file] | [roles::DIR, file] if file.ends_with(".json") => Some(rest.to_string()),
        [wiki::DIR, project, file] => {
            file.strip_suffix(".json").map(|slug| format!("{}/{project}/{slug}.md", wiki::DIR))
        }
        _ => None,
    }
}

/// Record path for a `sync_deleted` row (`kind`, `key`).
pub fn entity_of_deleted(kind: &str, key: &str) -> Option<String> {
    match kind {
        "pins" => Some(pins::path_of(Uuid::parse_str(key).ok()?)),
        "roles" => Some(roles::path_of(Uuid::parse_str(key).ok()?)),
        "wiki" => {
            let (project, slug) = key.split_once('/')?;
            Some(wiki::path_of(Uuid::parse_str(project).ok()?, slug))
        }
        _ => None,
    }
}

pub fn render(deleted_at: &str, device: &str) -> String {
    canonical(&json!({ "deleted_at": deleted_at, "device": device }))
}

pub fn deleted_at(bytes: &[u8]) -> Option<Ts> {
    format::parse(bytes).and_then(|v| format::ts_field(&v, "deleted_at"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstone_paths_round_trip() {
        let id = Uuid::new_v4();
        let p = pins::path_of(id);
        let t = for_entity(&p).unwrap();
        assert_eq!(t, format!("tombstones/pins/{id}.json"));
        assert_eq!(entity_of(&t).as_deref(), Some(p.as_str()));
        let w = wiki::path_of(id, "배포-절차");
        let t = for_entity(&w).unwrap();
        assert_eq!(entity_of(&t).as_deref(), Some(w.as_str()));
        assert_eq!(entity_of_deleted("wiki", &format!("{}/배포-절차", id.simple())).as_deref(), Some(w.as_str()));
        assert!(for_entity("projects/x.json").is_none());
    }
}
