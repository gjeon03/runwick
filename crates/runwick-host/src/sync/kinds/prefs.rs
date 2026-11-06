//! `settings/prefs.json`: the general preferences that mean the same on any computer. Screen
//! scale and everything else stay per computer.

use crate::store::ts_to_sql;
use runwick_core::api::prefs::Prefs;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const PATH: &str = "settings/prefs.json";
/// Key of the preferences in the host's key/value table (see `crate::prefs`).
const KV_KEY: &str = "prefs";
/// Preferences that travel.
pub const KEYS: [&str; 4] = ["confirm_stop", "option_as_meta", "reduce_motion", "terminal_font_size"];

fn stored(c: &Connection) -> rusqlite::Result<Map<String, Value>> {
    let text: Option<String> = c.query_row("SELECT value FROM kv WHERE key = ?1", [KV_KEY], |r| r.get(0)).optional()?;
    Ok(text
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default())
}

pub fn render(c: &Connection, out: &mut BTreeMap<String, Value>) -> rusqlite::Result<()> {
    let prefs: Prefs = serde_json::from_value(Value::Object(stored(c)?)).unwrap_or_default();
    let all = serde_json::to_value(prefs.sanitized()).unwrap_or(Value::Null);
    let mut m = Map::new();
    for k in KEYS {
        if let Some(v) = all.get(k) {
            m.insert(k.to_string(), v.clone());
        }
    }
    out.insert(PATH.to_string(), Value::Object(m));
    Ok(())
}

pub fn apply(c: &Connection, merged: Option<&Value>) -> Result<(), String> {
    let Some(v) = merged.and_then(Value::as_object) else {
        return Ok(());
    };
    let db = |e: rusqlite::Error| e.to_string();
    let mut all = stored(c).map_err(db)?;
    for k in KEYS {
        if let Some(x) = v.get(k) {
            all.insert(k.to_string(), x.clone());
        }
    }
    let prefs: Prefs =
        serde_json::from_value(Value::Object(all.clone())).map_err(|_| "설정 값 형식이 아니에요".to_string())?;
    // Keep keys this version does not know, with the travelled ones sanitized.
    if let Value::Object(clean) = serde_json::to_value(prefs.sanitized()).unwrap_or(Value::Null) {
        for k in KEYS {
            if let Some(x) = clean.get(k) {
                all.insert(k.to_string(), x.clone());
            }
        }
    }
    c.execute(
        "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![KV_KEY, Value::Object(all).to_string(), ts_to_sql(&runwick_core::now())],
    )
    .map_err(db)?;
    Ok(())
}
