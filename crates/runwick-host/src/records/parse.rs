//! Transcript lines → records. Pure functions over one JSONL line each.
//!
//! Claude Code (`~/.claude/projects/<cwd>/<id>.jsonl`): `user`/`assistant` lines carry `uuid`,
//! `timestamp`, `isSidechain` and `message.content` (a string or blocks: text, tool_use,
//! tool_result, thinking, image). Lines the CLI injects (`isMeta`, compaction summaries, command
//! output, task notifications) are not conversation and are skipped. A `system` line with
//! subtype `turn_duration`, or an answer with `stop_reason: end_turn`, ends a turn.
//!
//! Codex (`rollout-*-<thread>.jsonl`): `response_item` lines with `payload.type` `message`
//! (role user/assistant; developer is configuration) and tool calls; the user items Codex adds
//! itself (environment, AGENTS.md, image wrappers) are skipped. `event_msg` `task_complete` ends
//! a turn.

use runwick_core::Ts;
use runwick_core::api::records::Role;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// One message read from a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// Identity inside the transcript; stable when the file is read again.
    pub key: String,
    pub role: Role,
    pub text: String,
    pub at: Ts,
    pub sidechain: bool,
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Message(Parsed),
    TurnEnd(Ts),
}

const IMAGE: &str = "[이미지]";

fn ts(v: &Value) -> Option<Ts> {
    let s = v.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&chrono::Utc))
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Short stable id for lines without one.
fn line_hash(line: &str) -> String {
    let d = Sha256::digest(line.as_bytes());
    format!("h:{}", d.iter().take(12).map(|b| format!("{b:02x}")).collect::<String>())
}

fn tool_row(key: String, name: &str, at: Ts, sidechain: bool) -> Item {
    Item::Message(Parsed {
        key,
        role: Role::Tool,
        text: format!("도구 사용: {name}"),
        at,
        sidechain,
        tool_name: Some(name.to_string()),
    })
}

// ---------------------------------------------------------------------------------------------
// Claude Code

/// Text of a user message as the person sees it, or None for text the CLI injected.
fn claude_user_text(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if !s.starts_with('<') {
        return Some(s.to_string());
    }
    if let Some(name) = between(s, "<command-name>", "</command-name>") {
        let name = name.trim();
        let args = between(s, "<command-args>", "</command-args>").map(str::trim).unwrap_or("");
        let name = if name.starts_with('/') { name.to_string() } else { format!("/{name}") };
        return Some(if args.is_empty() { name } else { format!("{name} {args}") });
    }
    if let Some(cmd) = between(s, "<bash-input>", "</bash-input>") {
        return Some(format!("! {}", cmd.trim()));
    }
    const INJECTED: &[&str] = &[
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<command-message>",
        "<system-reminder>",
        "<task-notification>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<user-prompt-submit-hook>",
    ];
    if INJECTED.iter().any(|p| s.starts_with(p)) {
        return None;
    }
    Some(s.to_string())
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

pub fn claude_line(line: &str) -> Vec<Item> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    let Some(at) = v.get("timestamp").and_then(ts) else {
        return Vec::new();
    };
    match kind {
        "system" => {
            if v.get("subtype").and_then(Value::as_str) == Some("turn_duration") {
                vec![Item::TurnEnd(at)]
            } else {
                Vec::new()
            }
        }
        "user" | "assistant" => {
            if flag(&v, "isMeta") || flag(&v, "isCompactSummary") || flag(&v, "isVisibleInTranscriptOnly") {
                return Vec::new();
            }
            let Some(uuid) = v.get("uuid").and_then(Value::as_str) else {
                return Vec::new();
            };
            let sidechain = flag(&v, "isSidechain");
            let msg = v.get("message").unwrap_or(&Value::Null);
            let content = msg.get("content").unwrap_or(&Value::Null);
            let mut out = if kind == "user" {
                claude_user(uuid, content, at, sidechain)
            } else {
                claude_assistant(uuid, content, at, sidechain)
            };
            if kind == "assistant" && msg.get("stop_reason").and_then(Value::as_str) == Some("end_turn") {
                out.push(Item::TurnEnd(at));
            }
            out
        }
        _ => Vec::new(),
    }
}

fn claude_user(uuid: &str, content: &Value, at: Ts, sidechain: bool) -> Vec<Item> {
    let text = match content {
        Value::String(s) => claude_user_text(s),
        Value::Array(blocks) => {
            let parts: Vec<String> = blocks
                .iter()
                .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                    Some("text") => b.get("text").and_then(Value::as_str).and_then(claude_user_text),
                    Some("image") => Some(IMAGE.to_string()),
                    // Tool results are the CLI talking to itself; the call is recorded instead.
                    _ => None,
                })
                .collect();
            // A message with only images came from the person too, but only with real text is
            // it worth a record.
            (!parts.is_empty() && parts.iter().any(|p| p != IMAGE)).then(|| parts.join("\n"))
        }
        _ => None,
    };
    text.map(|text| {
        Item::Message(Parsed { key: uuid.to_string(), role: Role::User, text, at, sidechain, tool_name: None })
    })
    .into_iter()
    .collect()
}

fn claude_assistant(uuid: &str, content: &Value, at: Ts, sidechain: bool) -> Vec<Item> {
    let mut out = Vec::new();
    match content {
        Value::String(s) if !s.trim().is_empty() => out.push(Item::Message(Parsed {
            key: uuid.to_string(),
            role: Role::Assistant,
            text: s.trim().to_string(),
            at,
            sidechain,
            tool_name: None,
        })),
        Value::Array(blocks) => {
            for (i, b) in blocks.iter().enumerate() {
                let key = if i == 0 { uuid.to_string() } else { format!("{uuid}#{i}") };
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let t = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
                        if !t.is_empty() {
                            out.push(Item::Message(Parsed {
                                key,
                                role: Role::Assistant,
                                text: t.to_string(),
                                at,
                                sidechain,
                                tool_name: None,
                            }));
                        }
                    }
                    Some("tool_use") => {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("도구");
                        out.push(tool_row(key, name, at, sidechain));
                    }
                    _ => {} // thinking and anything new: not part of the record
                }
            }
        }
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Codex

/// User content items Codex adds on its own.
fn codex_injected(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "<environment_context>",
        "<user_instructions>",
        "# AGENTS.md instructions",
        "<permissions instructions>",
        "<collaboration_mode>",
        "<turn_aborted>",
        "<image",
        "</image>",
    ];
    let t = text.trim_start();
    PREFIXES.iter().any(|p| t.starts_with(p))
}

pub fn codex_line(line: &str) -> Vec<Item> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(at) = v.get("timestamp").and_then(ts) else {
        return Vec::new();
    };
    let payload = v.get("payload").unwrap_or(&Value::Null);
    let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "event_msg" if ptype == "task_complete" => vec![Item::TurnEnd(at)],
        "response_item" => {
            let key = payload
                .get("id")
                .or_else(|| payload.get("call_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| line_hash(line));
            match ptype {
                "message" => codex_message(key, payload, at),
                "function_call" | "custom_tool_call" => {
                    let name = payload.get("name").and_then(Value::as_str).unwrap_or("도구");
                    vec![tool_row(key, name, at, false)]
                }
                "local_shell_call" => vec![tool_row(key, "shell", at, false)],
                "web_search_call" => vec![tool_row(key, "web_search", at, false)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn codex_message(key: String, payload: &Value, at: Ts) -> Vec<Item> {
    let role = match payload.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return Vec::new(), // developer/system: configuration, not conversation
    };
    let items = payload.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    for it in &items {
        match it.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "text") => {
                let t = it.get("text").and_then(Value::as_str).unwrap_or("");
                if role == Role::User && codex_injected(t) {
                    continue;
                }
                let t = t.trim();
                if !t.is_empty() {
                    parts.push(t.to_string());
                }
            }
            Some("input_image") if role == Role::User => parts.push(IMAGE.to_string()),
            _ => {}
        }
    }
    if parts.iter().all(|p| p == IMAGE) {
        return Vec::new();
    }
    vec![Item::Message(Parsed { key, role, text: parts.join("\n\n"), at, sidechain: false, tool_name: None })]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msgs(items: Vec<Item>) -> Vec<Parsed> {
        items
            .into_iter()
            .filter_map(|i| match i {
                Item::Message(m) => Some(m),
                Item::TurnEnd(_) => None,
            })
            .collect()
    }

    const T: &str = "2026-11-05T02:01:00.000Z";

    #[test]
    fn claude_user_and_assistant_blocks() {
        let user = json!({"type":"user","uuid":"u1","timestamp":T,"isSidechain":false,
            "message":{"role":"user","content":"로그인 직후 세션이 만료되는 오류 고쳐줘"}});
        let m = msgs(claude_line(&user.to_string()));
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].key.as_str(), m[0].role), ("u1", Role::User));
        assert_eq!(m[0].text, "로그인 직후 세션이 만료되는 오류 고쳐줘");

        let asst = json!({"type":"assistant","uuid":"a1","timestamp":T,"isSidechain":true,
            "message":{"id":"msg_1","stop_reason":"end_turn","content":[
                {"type":"thinking","thinking":"secret"},
                {"type":"text","text":"원인을 찾았습니다."},
                {"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}});
        let items = claude_line(&asst.to_string());
        assert!(items.iter().any(|i| matches!(i, Item::TurnEnd(_))));
        let m = msgs(items);
        assert_eq!(m.len(), 2, "thinking is skipped");
        assert_eq!((m[0].key.as_str(), m[0].role, m[0].sidechain), ("a1#1", Role::Assistant, true));
        assert_eq!((m[1].key.as_str(), m[1].role, m[1].text.as_str()), ("a1#2", Role::Tool, "도구 사용: Bash"));
        assert_eq!(m[1].tool_name.as_deref(), Some("Bash"));
    }

    #[test]
    fn claude_skips_injected_lines() {
        let cases = [
            json!({"type":"user","uuid":"m","timestamp":T,"isMeta":true,"message":{"content":"Caveat"}}),
            json!({"type":"user","uuid":"c","timestamp":T,"isCompactSummary":true,"message":{"content":"요약"}}),
            json!({"type":"user","uuid":"o","timestamp":T,"message":{"content":"<local-command-stdout>x</local-command-stdout>"}}),
            json!({"type":"user","uuid":"n","timestamp":T,"message":{"content":"<task-notification>done</task-notification>"}}),
            json!({"type":"user","uuid":"r","timestamp":T,"message":{"content":[{"type":"tool_result","content":"ok"}]}}),
            json!({"type":"attachment","uuid":"x","timestamp":T}),
            json!({"type":"user","uuid":"nots","message":{"content":"no time"}}),
        ];
        for c in cases {
            assert!(msgs(claude_line(&c.to_string())).is_empty(), "{c}");
        }
        assert!(claude_line("{not json").is_empty());
        let turn = json!({"type":"system","subtype":"turn_duration","timestamp":T});
        assert_eq!(claude_line(&turn.to_string()).len(), 1);
    }

    #[test]
    fn claude_commands_become_readable() {
        let cmd = json!({"type":"user","uuid":"k","timestamp":T,"message":{"content":
            "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args>짧게</command-args>"}});
        assert_eq!(msgs(claude_line(&cmd.to_string()))[0].text, "/compact 짧게");
        let bash =
            json!({"type":"user","uuid":"b","timestamp":T,"message":{"content":"<bash-input>git status</bash-input>"}});
        assert_eq!(msgs(claude_line(&bash.to_string()))[0].text, "! git status");
        let img = json!({"type":"user","uuid":"i","timestamp":T,"message":{"content":[
            {"type":"image","source":{}},{"type":"text","text":"이 화면 봐줘"}]}});
        assert_eq!(msgs(claude_line(&img.to_string()))[0].text, "[이미지]\n이 화면 봐줘");
    }

    #[test]
    fn codex_messages_tools_and_turns() {
        let env = json!({"timestamp":T,"type":"response_item","payload":{"type":"message","role":"user","id":"m0",
            "content":[{"type":"input_text","text":"<environment_context>\n<cwd>/x</cwd>"},
                       {"type":"input_text","text":"# AGENTS.md instructions for /x"}]}});
        assert!(msgs(codex_line(&env.to_string())).is_empty());
        let dev = json!({"timestamp":T,"type":"response_item","payload":{"type":"message","role":"developer",
            "content":[{"type":"input_text","text":"rules"}]}});
        assert!(msgs(codex_line(&dev.to_string())).is_empty());
        let user = json!({"timestamp":T,"type":"response_item","payload":{"type":"message","role":"user","id":"m1",
            "content":[{"type":"input_text","text":"재시도 간격을 늘려줘"}]}});
        let m = msgs(codex_line(&user.to_string()));
        assert_eq!((m[0].key.as_str(), m[0].role, m[0].text.as_str()), ("m1", Role::User, "재시도 간격을 늘려줘"));
        let call = json!({"timestamp":T,"type":"response_item","payload":{"type":"custom_tool_call","call_id":"c1","name":"apply_patch"}});
        assert_eq!(msgs(codex_line(&call.to_string()))[0].text, "도구 사용: apply_patch");
        let reasoning = json!({"timestamp":T,"type":"response_item","payload":{"type":"reasoning","summary":[]}});
        assert!(codex_line(&reasoning.to_string()).is_empty());
        let done =
            json!({"timestamp":T,"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"끝"}});
        assert!(matches!(codex_line(&done.to_string())[..], [Item::TurnEnd(_)]));
        // No id: the key is a hash of the line, the same when read again.
        let anon = json!({"timestamp":T,"type":"response_item","payload":{"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"완료했어요"}]}});
        let a = msgs(codex_line(&anon.to_string()));
        let b = msgs(codex_line(&anon.to_string()));
        assert_eq!(a[0].key, b[0].key);
        assert!(a[0].key.starts_with("h:"));
    }
}
