//! Store-level tests: search, pins, context and the reads other features use.

use super::*;
use crate::store::ts_to_sql;
use runwick_core::api::sessions::Tool;
use uuid::Uuid;

struct Data {
    store: Store,
    project: ProjectId,
    other_project: ProjectId,
    session: SessionId,
    codex: SessionId,
    ids: Vec<MessageId>,
}

/// Two projects; a Claude session with four messages and a Codex session with one.
fn data() -> Data {
    let store = Store::open_in_memory().unwrap();
    let (p, q, w, wq) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (s, x) = (Uuid::now_v7(), Uuid::now_v7());
    let mut ids = Vec::new();
    store
        .with(|c| {
            for (id, name, ws) in [(p, "payments-api", w), (q, "docs-site", wq)] {
                c.execute(
                    "INSERT INTO projects (id, name, root, created_at) VALUES (?1, ?2, ?3, '2026-11-01T00:00:00.000Z')",
                    rusqlite::params![id, name, format!("/w/{name}")],
                )?;
                c.execute(
                    "INSERT INTO workspaces (id, project_id, name, path, kind, created_at)
                     VALUES (?1, ?2, '기본 공간', '/w', 'default', '2026-11-01T00:00:00.000Z')",
                    rusqlite::params![ws, id],
                )?;
            }
            c.execute(
                "INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at)
                 VALUES (?1, ?2, ?3, '로그인 오류 수정', 'claude', '/w', '2026-11-05T02:00:00.000Z')",
                rusqlite::params![s, p, w],
            )?;
            c.execute(
                "INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at)
                 VALUES (?1, ?2, ?3, '회귀 검토', 'codex', '/w', '2026-11-05T02:00:00.000Z')",
                rusqlite::params![x, q, wq],
            )?;
            let rows: [(SessionId, ProjectId, Uuid, &str, &str, &str); 5] = [
                (s, p, w, "user", "로그인 직후 세션이 만료되는 오류 고쳐줘", "2026-11-05T02:01:00.000Z"),
                (s, p, w, "assistant", "원인을 찾았습니다. 만료 시각은 초 단위인데 밀리초 값과 그대로 비교하고 있습니다.", "2026-11-05T02:04:00.000Z"),
                (s, p, w, "tool", "도구 사용: Bash", "2026-11-05T02:04:30.000Z"),
                (s, p, w, "assistant", "src/auth/session.ts 41번 줄의 비교식을 고치면 해결됩니다. 50%_done", "2026-11-05T02:05:00.000Z"),
                (x, q, wq, "assistant", "refresh.ts는 이미 초 단위로 맞습니다. 토큰 만료 시각 계산에는 문제가 없어요.", "2026-11-05T02:12:00.000Z"),
            ];
            for (i, (sess, proj, ws, role, text, at)) in rows.iter().enumerate() {
                let id = Uuid::now_v7();
                ids.push(id);
                c.execute(
                    "INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                    rusqlite::params![id, sess, proj, ws, format!("k{i}"), i as i64, role, text, at],
                )?;
            }
            Ok(())
        })
        .unwrap();
    Data { store, project: p, other_project: q, session: s, codex: x, ids }
}

fn find(d: &Data, text: &str, scope: Scope) -> SearchResult {
    search::search(&d.store, &SearchQuery { text: text.into(), scope, ..Default::default() }).unwrap()
}

#[test]
fn korean_substrings_and_code_fragments_are_found_newest_first() {
    let d = data();
    let r = find(&d, "만료 시각", Scope::All);
    assert!(r.scanned, "two-character words are scanned");
    assert_eq!(r.total, 2);
    assert_eq!(r.hits[0].session_title, "회귀 검토", "newest first");
    assert_eq!(r.hits[0].project_name, "docs-site");
    assert_eq!(r.hits[0].tool, Tool::Codex);
    // Substring inside a word, through the trigram index.
    let r = find(&d, "밀리초", Scope::All);
    assert!(!r.scanned);
    assert_eq!(r.total, 1);
    let hit = &r.hits[0];
    let marked: Vec<&str> = hit.highlights.iter().map(|(a, b)| &hit.snippet[*a as usize..*b as usize]).collect();
    assert_eq!(marked, vec!["밀리초"]);
    assert_eq!(find(&d, "session.ts", Scope::All).total, 1);
    assert_eq!(find(&d, "SESSION.TS", Scope::All).total, 1, "case-insensitive");
    assert_eq!(find(&d, "\"만료 시각은\"", Scope::All).total, 1, "quoted phrase");
    // Mixed: one indexed word and one short word.
    assert_eq!(find(&d, "초 단위인데", Scope::All).total, 1);
    // LIKE wildcards in the query are literal.
    assert_eq!(find(&d, "%_", Scope::All).total, 1);
    assert_eq!(find(&d, "없는말입니다", Scope::All).total, 0);
    assert_eq!(find(&d, "  ", Scope::All).hits.len(), 0);
}

#[test]
fn scope_tool_date_and_pinned_filters() {
    let d = data();
    assert_eq!(find(&d, "만료", Scope::Project { id: d.project }).total, 2);
    assert_eq!(find(&d, "만료", Scope::Project { id: d.other_project }).total, 1);
    assert_eq!(find(&d, "만료", Scope::Session { id: d.codex }).total, 1);
    let by_tool = SearchQuery { text: "만료".into(), tool: Some(Tool::Codex), ..Default::default() };
    assert_eq!(search::search(&d.store, &by_tool).unwrap().total, 1);
    let to = chrono::DateTime::parse_from_rfc3339("2026-11-05T02:03:00Z").unwrap().with_timezone(&chrono::Utc);
    let early = SearchQuery { text: "만료".into(), to: Some(to), ..Default::default() };
    assert_eq!(search::search(&d.store, &early).unwrap().total, 1);
    let limited = SearchQuery { text: "만료".into(), limit: 1, ..Default::default() };
    let r = search::search(&d.store, &limited).unwrap();
    assert_eq!((r.hits.len(), r.total), (1, 3));

    pins::create(&d.store, &NewPin { source: PinSource::Message { message_id: d.ids[1] }, title: None, note: None })
        .unwrap();
    let pinned = SearchQuery { text: "만료".into(), pinned_only: true, ..Default::default() };
    let r = search::search(&d.store, &pinned).unwrap();
    assert_eq!(r.total, 1);
    assert!(r.hits[0].pinned);
    // "핀만 보기" without words lists pinned messages.
    let all_pinned = SearchQuery { pinned_only: true, ..Default::default() };
    assert_eq!(search::search(&d.store, &all_pinned).unwrap().total, 1);
}

#[test]
fn pins_keep_their_snapshot_when_the_source_is_gone() {
    let d = data();
    let new = |source| NewPin { source, title: None, note: Some(" 배포 전에 확인 ".into()) };
    let (pin, created) = pins::create(&d.store, &new(PinSource::Message { message_id: d.ids[1] })).unwrap();
    assert!(created);
    assert_eq!(pin.title, "원인을 찾았습니다. 만료 시각은 초 단위인데 밀리초 값과 그대로 비교하…");
    assert_eq!(pin.note, "배포 전에 확인");
    assert_eq!(pin.session_title, "로그인 오류 수정");
    assert!(pin.source_available);
    // Pinning the same message again returns the same pin.
    let (again, created) = pins::create(&d.store, &new(PinSource::Message { message_id: d.ids[1] })).unwrap();
    assert_eq!((again.id, created), (pin.id, false));
    // Excerpt and terminal pins.
    let (ex, _) = pins::create(
        &d.store,
        &NewPin {
            source: PinSource::Excerpt { message_id: d.ids[1], text: "만료 시각은 초 단위".into() },
            title: Some("단위".into()),
            note: None,
        },
    )
    .unwrap();
    assert_eq!((ex.kind, ex.text.as_str(), ex.title.as_str()), (PinKind::Excerpt, "만료 시각은 초 단위", "단위"));
    let (term, _) = pins::create(
        &d.store,
        &new(PinSource::Terminal { session_id: d.session, text: "\nFAIL src/auth/session.test.ts\n   ".into() }),
    )
    .unwrap();
    assert_eq!(
        (term.kind, term.text.as_str(), term.source_available),
        (PinKind::Terminal, "FAIL src/auth/session.test.ts", false)
    );
    assert!(pins::create(&d.store, &new(PinSource::Terminal { session_id: d.session, text: "  \n ".into() })).is_err());
    let (latest, _) = pins::create(&d.store, &new(PinSource::LatestAnswer { session_id: d.session })).unwrap();
    assert_eq!(latest.message_id, Some(d.ids[3]), "newest assistant answer, not the tool row");

    // The message disappears: the pin stays readable with its snapshot.
    d.store.with(|c| c.execute("DELETE FROM messages WHERE id = ?1", [d.ids[1]])).unwrap();
    let list =
        d.store.with(|c| pins::list(c, &PinQuery { project_id: Some(d.project), session_id: None }, None)).unwrap();
    let gone = list.iter().find(|p| p.id == pin.id).unwrap();
    assert!(!gone.source_available);
    assert!(gone.text.contains("밀리초"));
    // Unpinning removes only the pin.
    assert!(pins::remove(&d.store, latest.id).unwrap());
    let still: i64 =
        d.store.with(|c| c.query_row("SELECT COUNT(*) FROM messages WHERE id = ?1", [d.ids[3]], |r| r.get(0))).unwrap();
    assert_eq!(still, 1);
    let updated =
        pins::update(&d.store, &PinUpdate { id: pin.id, title: Some("세션 만료 원인".into()), note: None }).unwrap();
    assert_eq!((updated.title.as_str(), updated.note.as_str()), ("세션 만료 원인", "배포 전에 확인"));
    assert!(pins::update(&d.store, &PinUpdate { id: pin.id, title: Some(" ".into()), note: None }).is_err());
}

#[test]
fn context_reads_around_a_message_and_at_the_session_end() {
    let d = data();
    let q = ContextQuery { anchor: Anchor::Message { id: d.ids[1] }, before: 1, after: 1 };
    let cx = d.store.with(|c| db::context(c, &q)).unwrap().unwrap();
    assert_eq!(cx.messages.iter().map(|m| m.id).collect::<Vec<_>>(), d.ids[0..3].to_vec());
    assert_eq!((cx.has_before, cx.has_after), (false, true));
    assert_eq!(cx.anchor, Some(d.ids[1]));
    let q = ContextQuery { anchor: Anchor::SessionEnd { id: d.session }, before: 2, after: 5 };
    let cx = d.store.with(|c| db::context(c, &q)).unwrap().unwrap();
    assert_eq!(cx.messages.iter().map(|m| m.id).collect::<Vec<_>>(), d.ids[1..4].to_vec());
    assert_eq!((cx.has_before, cx.has_after), (true, false));
    let missing = ContextQuery { anchor: Anchor::Message { id: Uuid::now_v7() }, before: 1, after: 1 };
    assert!(d.store.with(|c| db::context(c, &missing)).unwrap().is_none());
}

#[test]
fn reads_for_other_features() {
    let d = data();
    let since = chrono::DateTime::parse_from_rfc3339("2026-11-05T02:02:00Z").unwrap().with_timezone(&chrono::Utc);
    let m = messages_since(&d.store, d.project, since, 10).unwrap();
    assert_eq!(m.iter().map(|m| m.id).collect::<Vec<_>>(), d.ids[1..4].to_vec());
    assert_eq!(messages_since(&d.store, d.project, since, 1).unwrap().len(), 1);
    let tail = session_messages(&d.store, d.session, 2).unwrap();
    assert_eq!(tail.iter().map(|m| m.id).collect::<Vec<_>>(), d.ids[2..4].to_vec(), "newest two, oldest first");
    assert_eq!(session_messages(&d.store, d.session, 100).unwrap().len(), 4);
    let t = turn_info(&d.store, d.session).unwrap();
    assert_eq!(t.last_answer.map(|m| m.id), Some(d.ids[3]));
    assert_eq!(t.turn_ended_at, None);
    let _ = ts_to_sql(&since);
}

#[test]
fn rebuilding_the_index_keeps_search_working() {
    let d = data();
    assert!(search::begin_reindex());
    assert!(!search::begin_reindex(), "one rebuild at a time");
    search::reindex(&d.store).unwrap();
    assert!(search::begin_reindex(), "flag released");
    search::reindex(&d.store).unwrap();
    assert_eq!(find(&d, "밀리초", Scope::All).total, 1);
    // Triggers are back: new messages are indexed.
    d.store
        .with(|c| {
            c.execute(
                "INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at)
                 VALUES (?1, ?2, ?3, ?3, 'k9', 9, 'user', '재시도 상한은 60초', '2026-11-06T00:00:00.000Z', '2026-11-06T00:00:00.000Z')",
                rusqlite::params![Uuid::now_v7(), d.session, d.project],
            )
        })
        .unwrap();
    assert_eq!(find(&d, "재시도 상한", Scope::All).total, 1);
    assert_eq!(find(&d, "상한은", Scope::All).total, 1);
}

#[test]
fn session_titles_are_shown_without_invisible_direction_marks() {
    let d = data();
    d.store
        .with(|c| c.execute("UPDATE sessions SET title = '회귀\u{202e}토검\u{200b}' WHERE id = ?1", [d.codex]))
        .unwrap();
    let r = find(&d, "만료 시각", Scope::All);
    assert_eq!(r.hits[0].session_title, "회귀토검");
}
