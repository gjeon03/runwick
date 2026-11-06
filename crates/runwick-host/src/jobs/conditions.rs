//! When automatic work may start. Pure functions over observed facts, so the policy can be
//! tested with a fake clock and power source.
//!
//! - Wiki proposals: at least [`MIN_NEW_MESSAGES`] new messages since the last proposal job,
//!   at least [`WIKI_GAP`] since that job, and no proposal left unreviewed.
//! - Timeline: a finished day with records that has no summary, or whose records changed since.
//! - Both wait while the project is busy (output within [`IDLE_AFTER`]) and while on battery.

use chrono::Duration;
use runwick_core::Ts;
use runwick_core::api::jobs::PowerSource;

pub const MIN_NEW_MESSAGES: u64 = 20;
pub const WIKI_GAP: Duration = Duration::hours(6);
pub const IDLE_AFTER: Duration = Duration::minutes(15);

pub const HELD_BATTERY: &str = "배터리 사용 중이라 보류";
pub const HELD_BUSY: &str = "작업 중이라 한가해질 때까지 기다려요";

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Nothing to do now.
    Skip,
    /// Worth doing, but a condition holds it back.
    Hold {
        reason: String,
        held: &'static str,
    },
    Run {
        reason: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct WikiFacts {
    pub new_messages: u64,
    pub last_job_at: Option<Ts>,
    pub open_proposals: u32,
    pub last_activity_at: Option<Ts>,
}

#[derive(Debug, Clone)]
pub struct DayFacts {
    pub day: String,
    pub records: u32,
    /// Fingerprint of the day's records now and when it was last summarized.
    pub fingerprint: String,
    pub summarized: Option<String>,
    pub last_activity_at: Option<Ts>,
}

fn gate(reason: String, last_activity: Option<Ts>, power: PowerSource, now: Ts) -> Decision {
    if last_activity.is_some_and(|t| now - t < IDLE_AFTER) {
        return Decision::Hold { reason, held: HELD_BUSY };
    }
    if power == PowerSource::Battery {
        return Decision::Hold { reason, held: HELD_BATTERY };
    }
    Decision::Run { reason }
}

pub fn wiki(f: &WikiFacts, power: PowerSource, now: Ts) -> Decision {
    if f.new_messages < MIN_NEW_MESSAGES || f.open_proposals > 0 {
        return Decision::Skip;
    }
    if f.last_job_at.is_some_and(|t| now - t < WIKI_GAP) {
        return Decision::Skip;
    }
    let reason = format!("자동 · 대화 기록 {}건이 새로 쌓였어요", f.new_messages);
    gate(reason, f.last_activity_at, power, now)
}

pub fn timeline(f: &DayFacts, power: PowerSource, now: Ts) -> Decision {
    if f.records == 0 || f.summarized.as_deref() == Some(f.fingerprint.as_str()) {
        return Decision::Skip;
    }
    let reason = match f.summarized {
        Some(_) => format!("자동 · {} 기록이 요약 뒤에 바뀌었어요 ({}건)", f.day, f.records),
        None => format!("자동 · {} 기록 {}건", f.day, f.records),
    };
    gate(reason, f.last_activity_at, power, now)
}

/// Delay before automatic retry `attempt` (1-based); None after the last attempt.
pub fn retry_delay(attempt: u32) -> Option<Duration> {
    match attempt {
        1 => Some(Duration::minutes(2)),
        2 => Some(Duration::minutes(10)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(min: i64) -> Ts {
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH + Duration::days(20_000) + Duration::minutes(min)
    }

    #[test]
    fn wiki_needs_enough_messages_gap_and_no_open_proposals() {
        let now = at(0);
        let base =
            WikiFacts { new_messages: 24, last_job_at: None, open_proposals: 0, last_activity_at: Some(at(-60)) };
        assert!(matches!(wiki(&base, PowerSource::Ac, now), Decision::Run { .. }));
        assert_eq!(wiki(&WikiFacts { new_messages: 19, ..base.clone() }, PowerSource::Ac, now), Decision::Skip);
        assert_eq!(wiki(&WikiFacts { open_proposals: 1, ..base.clone() }, PowerSource::Ac, now), Decision::Skip);
        assert_eq!(
            wiki(&WikiFacts { last_job_at: Some(at(-5 * 60)), ..base.clone() }, PowerSource::Ac, now),
            Decision::Skip
        );
        assert!(matches!(
            wiki(&WikiFacts { last_job_at: Some(at(-7 * 60)), ..base.clone() }, PowerSource::Ac, now),
            Decision::Run { .. }
        ));
    }

    #[test]
    fn battery_and_busy_projects_hold_automatic_work() {
        let now = at(0);
        let f = WikiFacts { new_messages: 30, last_job_at: None, open_proposals: 0, last_activity_at: Some(at(-60)) };
        match wiki(&f, PowerSource::Battery, now) {
            Decision::Hold { held, .. } => assert_eq!(held, HELD_BATTERY),
            d => panic!("{d:?}"),
        }
        // Unknown power is treated as AC.
        assert!(matches!(wiki(&f, PowerSource::Unknown, now), Decision::Run { .. }));
        let busy = WikiFacts { last_activity_at: Some(at(-3)), ..f };
        match wiki(&busy, PowerSource::Ac, now) {
            Decision::Hold { held, .. } => assert_eq!(held, HELD_BUSY),
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn timeline_summarizes_once_and_again_only_when_records_change() {
        let now = at(0);
        let f = DayFacts {
            day: "2026-11-05".into(),
            records: 42,
            fingerprint: "a".into(),
            summarized: None,
            last_activity_at: None,
        };
        assert!(matches!(timeline(&f, PowerSource::Ac, now), Decision::Run { .. }));
        assert_eq!(
            timeline(&DayFacts { summarized: Some("a".into()), ..f.clone() }, PowerSource::Ac, now),
            Decision::Skip
        );
        assert!(matches!(
            timeline(&DayFacts { summarized: Some("old".into()), ..f.clone() }, PowerSource::Ac, now),
            Decision::Run { .. }
        ));
        assert_eq!(timeline(&DayFacts { records: 0, ..f }, PowerSource::Ac, now), Decision::Skip);
    }

    #[test]
    fn retries_back_off_and_stop_after_three_attempts() {
        assert_eq!(retry_delay(1), Some(Duration::minutes(2)));
        assert_eq!(retry_delay(2), Some(Duration::minutes(10)));
        assert_eq!(retry_delay(3), None);
    }
}
