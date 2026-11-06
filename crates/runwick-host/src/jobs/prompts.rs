//! Prompts for AI jobs and defensive reading of their answers.
//!
//! Prompts carry only the project's own records, newest first, cut to a fixed budget. Answers
//! are asked for as JSON; a reply wrapped in prose or code fences is still read, and anything
//! that does not fit is reported as a failure without touching existing pages.

use runwick_core::api::records::{Message, Role};
use serde_json::Value;

/// Characters of conversation records and of existing pages put into one prompt.
pub const RECORDS_BUDGET: usize = 48_000;
pub const PAGES_BUDGET: usize = 36_000;
const MESSAGE_MAX: usize = 1_500;
const PAGE_MAX: usize = 6_000;
pub const MAX_PROPOSALS: usize = 4;

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push_str(" …(생략)");
    t
}

fn role(r: Role) -> &'static str {
    match r {
        Role::User => "사람",
        Role::Assistant => "AI",
        Role::Tool => "도구",
    }
}

/// Messages newest first within the budget, as `[id] 시각 · 세션 · 역할: 본문` lines.
pub fn records_block(messages: &[Message], titles: &dyn Fn(&Message) -> String) -> (String, usize) {
    let mut out = String::new();
    let mut used = 0;
    let mut sorted: Vec<&Message> = messages.iter().filter(|m| !m.text.trim().is_empty()).collect();
    sorted.sort_by_key(|m| std::cmp::Reverse(m.at));
    for m in sorted {
        let text = cut(m.text.trim(), if m.role == Role::Tool { 200 } else { MESSAGE_MAX });
        let line = format!(
            "[{}] {} · {} · {}: {}\n",
            m.id,
            m.at.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
            titles(m),
            role(m.role),
            text.replace('\n', "\n    ")
        );
        if out.len() + line.len() > RECORDS_BUDGET {
            break;
        }
        out.push_str(&line);
        used += 1;
    }
    (out, used)
}

pub fn pages_block(pages: &[(String, String, String)]) -> String {
    let mut out = String::new();
    for (slug, title, body) in pages {
        let block =
            format!("<page slug=\"{slug}\" title=\"{}\">\n{}\n</page>\n", title.replace('"', "'"), cut(body, PAGE_MAX));
        if out.len() + block.len() > PAGES_BUDGET {
            out.push_str("<!-- 나머지 문서는 길이 제한으로 생략 -->\n");
            break;
        }
        out.push_str(&block);
    }
    out
}

pub fn wiki_prompt(project: &str, pages: &str, records: &str) -> String {
    format!(
        "You keep the project wiki of \"{project}\" up to date from the team's recent AI coding conversations.\n\
         Nothing you write is applied automatically: a person reviews every proposal.\n\n\
         Existing pages (Markdown):\n{pages}\n\
         Recent conversation records of this project, newest first. Each line starts with its message id:\n{records}\n\
         Task: propose at most {MAX_PROPOSALS} wiki changes that the records clearly support (decisions, procedures, \
         causes of bugs, conventions). Change an existing page only where the records add or correct facts; keep the \
         rest of its text exactly as it is. Prefer updating an existing page over creating a near-duplicate.\n\
         Write page text and rationale in Korean (keep code, identifiers and commands as they are).\n\
         If a change contradicts a recorded decision, say so in decision_conflict instead of silently replacing it.\n\
         If nothing is worth changing, return an empty list.\n\n\
         Reply with only this JSON object, no other text:\n\
         {{\"proposals\":[{{\"page\":\"<existing slug, or null for a new page>\",\"title\":\"<page title>\",\
         \"content\":\"<the complete new Markdown body of the page>\",\"rationale\":\"<1-2 sentences in Korean>\",\
         \"sources\":[\"<message id>\"],\"decision_conflict\":null}}]}}\n"
    )
}

pub fn draft_prompt(project: &str, records: &str, pages: &str) -> String {
    format!(
        "Write one new wiki page for the project \"{project}\" from the conversations below (newest first; each line \
         starts with its message id). A person asked for this draft and will review it.\n\n\
         Existing page titles and text, to avoid duplicating them:\n{pages}\n\
         Conversations:\n{records}\n\
         Write in Korean (keep code, identifiers and commands as they are). Include only what the conversations \
         support: what was decided, how to do it, and why. Use Markdown headings and lists.\n\n\
         Reply with only this JSON object, no other text:\n\
         {{\"title\":\"<page title>\",\"content\":\"<Markdown body>\",\"rationale\":\"<1 sentence in Korean>\",\
         \"sources\":[\"<message id>\"]}}\n"
    )
}

pub fn summary_prompt(project: &str, day: &str, events: &str) -> String {
    format!(
        "Summarize one day of work in the project \"{project}\" ({day}) from the observed records below.\n\n\
         Records:\n{events}\n\
         Write 2-5 short Korean sentences: what was worked on, what was decided or finished, and what was left open. \
         Use only facts in the records; do not guess.\n\n\
         Reply with only this JSON object, no other text:\n{{\"summary\":\"<Korean text>\"}}\n"
    )
}

/// The first JSON object in a reply: the whole text, a fenced block, or a balanced `{…}`.
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t)
        && v.is_object()
    {
        return Some(v);
    }
    let mut rest = t;
    while let Some(i) = rest.find("```") {
        let after = &rest[i + 3..];
        let body_start = after.find('\n').map(|n| n + 1).unwrap_or(0);
        let Some(end) = after[body_start..].find("```") else {
            break;
        };
        if let Ok(v) = serde_json::from_str::<Value>(after[body_start..body_start + end].trim())
            && v.is_object()
        {
            return Some(v);
        }
        rest = &after[body_start + end + 3..];
    }
    for (start, _) in t.match_indices('{') {
        if let Some(end) = balanced_end(&t[start..])
            && let Ok(v) = serde_json::from_str::<Value>(&t[start..start + end])
            && v.is_object()
        {
            return Some(v);
        }
    }
    None
}

/// Length of the balanced `{…}` at the start of `s`, honoring JSON strings.
fn balanced_end(s: &str) -> Option<usize> {
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (i, ch) in s.char_indices() {
        if in_str {
            match (esc, ch) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

fn ids(v: &Value) -> Vec<String> {
    v.get("sources")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.trim().trim_matches(['[', ']']).to_string())).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawProposal {
    pub page: Option<String>,
    pub title: String,
    pub content: String,
    pub rationale: String,
    pub sources: Vec<String>,
    pub decision_conflict: Option<String>,
}

pub fn parse_proposals(text: &str) -> Result<Vec<RawProposal>, String> {
    let v = extract_json(text).ok_or("AI 응답에서 JSON을 찾지 못했어요")?;
    let list = v.get("proposals").and_then(Value::as_array).ok_or("AI 응답에 proposals 목록이 없어요")?;
    let mut out = Vec::new();
    for p in list.iter().take(MAX_PROPOSALS) {
        let Some(content) = s(p, "content") else {
            continue;
        };
        let page = s(p, "page").filter(|x| x != "null");
        let title = s(p, "title").unwrap_or_default();
        if page.is_none() && title.is_empty() {
            continue;
        }
        out.push(RawProposal {
            page,
            title: title.chars().take(200).collect(),
            content: cut(&content, 60_000),
            rationale: s(p, "rationale").unwrap_or_else(|| "근거 설명 없음".into()).chars().take(600).collect(),
            sources: ids(p),
            decision_conflict: s(p, "decision_conflict").filter(|x| x != "null"),
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawDraft {
    pub title: String,
    pub content: String,
    pub rationale: String,
    pub sources: Vec<String>,
}

pub fn parse_draft(text: &str) -> Result<RawDraft, String> {
    let v = extract_json(text).ok_or("AI 응답에서 JSON을 찾지 못했어요")?;
    let title = s(&v, "title").ok_or("AI 응답에 제목이 없어요")?;
    let content = s(&v, "content").ok_or("AI 응답에 본문이 없어요")?;
    Ok(RawDraft {
        title: title.chars().take(200).collect(),
        content: cut(&content, 60_000),
        rationale: s(&v, "rationale").unwrap_or_default(),
        sources: ids(&v),
    })
}

pub fn parse_summary(text: &str) -> Result<String, String> {
    let v = extract_json(text).ok_or("AI 응답에서 JSON을 찾지 못했어요")?;
    s(&v, "summary").map(|t| cut(&t, 4_000)).ok_or_else(|| "AI 응답에 요약이 없어요".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_in_prose_and_fences() {
        let plain = r#"{"summary":"오늘은 로그인 오류를 고쳤다."}"#;
        assert_eq!(parse_summary(plain).unwrap(), "오늘은 로그인 오류를 고쳤다.");
        let fenced = "다음과 같습니다:\n```json\n{\"summary\": \"웹훅 재시도 구현\"}\n```\n끝.";
        assert_eq!(parse_summary(fenced).unwrap(), "웹훅 재시도 구현");
        let prose = "Sure! {\"summary\": \"중괄호 } 가 든 \\\"문장\\\"\"} 이상입니다";
        assert_eq!(parse_summary(prose).unwrap(), "중괄호 } 가 든 \"문장\"");
        assert!(parse_summary("JSON이 없어요").is_err());
        assert!(parse_summary("{\"other\": 1}").is_err());
    }

    #[test]
    fn proposals_are_validated_field_by_field() {
        let text = r#"{"proposals":[
            {"page":"로그인과-세션","title":"로그인과 세션","content":"본문","rationale":"근거","sources":["a","[b]"],"decision_conflict":null},
            {"page":null,"title":"","content":"제목 없는 새 문서는 버림"},
            {"page":"x","title":"내용 없음"},
            {"page":"null","title":"새 문서","content":"새 본문","decision_conflict":"결정과 다름"},
            "문자열", 42
        ]}"#;
        let ps = parse_proposals(text).unwrap();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].page.as_deref(), Some("로그인과-세션"));
        assert_eq!(ps[0].sources, vec!["a", "b"]);
        assert_eq!(ps[0].decision_conflict, None);
        assert_eq!(ps[1].page, None);
        assert_eq!(ps[1].rationale, "근거 설명 없음");
        assert_eq!(ps[1].decision_conflict.as_deref(), Some("결정과 다름"));
        assert!(parse_proposals("{\"proposals\": \"없음\"}").is_err());
        assert_eq!(parse_proposals("{\"proposals\": []}").unwrap(), vec![]);
    }

    #[test]
    fn draft_needs_title_and_content() {
        assert!(parse_draft("{\"title\":\"제목\"}").is_err());
        let d = parse_draft("{\"title\":\"웹훅 재시도\",\"content\":\"# 웹훅\",\"sources\":[\"m1\"]}").unwrap();
        assert_eq!((d.title.as_str(), d.sources.len()), ("웹훅 재시도", 1));
    }
}
