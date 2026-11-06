//! The only place the knowledge features read conversation records and write to the
//! timeline. Both belong to the RECORDS track (`crate::records`, `crate::timeline`).

use crate::host::Host;
use runwick_core::Ts;
use runwick_core::api::records::Message;
use runwick_core::api::timeline::{NewEvent, Summary};
use runwick_core::rpc::topic;
use std::collections::HashMap;
use uuid::Uuid;

/// Messages of a project written after `since`, oldest first, at most `limit`.
pub fn messages_since(host: &Host, project: Uuid, since: &Ts, limit: usize) -> Result<Vec<Message>, String> {
    crate::records::messages_since(&host.store, project, *since, limit).map_err(|e| e.message)
}

/// Pages of a project's messages read to find one session's newest messages.
const PAGE: usize = 2_000;
const MAX_SCAN: usize = 40_000;

/// Newest messages (up to `limit` each) of the given sessions of a project, oldest first.
pub fn session_messages(
    host: &Host,
    project: Uuid,
    sessions: &[Uuid],
    since: Ts,
    limit: usize,
) -> Result<Vec<Message>, String> {
    let mut kept: HashMap<Uuid, Vec<Message>> = HashMap::new();
    let mut cursor = since;
    let mut scanned = 0;
    loop {
        let page = messages_since(host, project, &cursor, PAGE)?;
        let n = page.len();
        scanned += n;
        for m in page {
            cursor = cursor.max(m.at);
            if sessions.contains(&m.session_id) {
                let v = kept.entry(m.session_id).or_default();
                v.push(m);
                if v.len() > limit {
                    v.remove(0);
                }
            }
        }
        if n < PAGE || scanned >= MAX_SCAN {
            break;
        }
    }
    let mut out: Vec<Message> = kept.into_values().flatten().collect();
    out.sort_by_key(|a| a.at);
    Ok(out)
}

/// Observed records of one project day.
pub struct Day {
    /// One line per record row ("14:20 · 대화 · 로그인 오류 수정 (메시지 12개)").
    pub lines: Vec<String>,
    pub count: u32,
    /// Changes whenever the day's records change.
    pub fingerprint: String,
}

pub fn local_offset_min() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

pub fn day(host: &Host, project: Uuid, day: chrono::NaiveDate) -> Result<Day, String> {
    let d = crate::timeline::observed_day(&host.store, project, day, local_offset_min()).map_err(|e| e.message)?;
    let mut events = d.events;
    events.sort_by_key(|a| a.at);
    let mut print = format!("{}", d.count);
    let lines = events
        .iter()
        .map(|e| {
            print.push_str(&format!("|{}:{}:{}", e.key, e.title, e.count));
            let at = e.at.with_timezone(&chrono::Local).format("%H:%M");
            let n = if e.count > 1 { format!(" ({}건)", e.count) } else { String::new() };
            format!("{at} · {} · {}{n}", e.kind.label(), e.title)
        })
        .collect();
    Ok(Day { lines, count: d.count, fingerprint: crate::wiki::files::hash(print.as_bytes()) })
}

pub fn store_summary(host: &Host, summary: &Summary) -> Result<(), String> {
    crate::timeline::store_summary(&host.store, summary).map_err(|e| e.message)?;
    host.bus.changed(&[topic::TIMELINE]);
    Ok(())
}

/// Best effort: a timeline failure never fails the wiki change that caused it.
pub fn record_event(host: &Host, event: NewEvent) {
    match crate::timeline::record_event(&host.store, &event) {
        Ok(()) => host.bus.changed(&[topic::TIMELINE]),
        Err(e) => tracing::warn!("timeline: {}", e.message),
    }
}
