//! MCP server definitions read from the providers' own files (read only):
//!
//! - Claude Code: `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json`) — `mcpServers` (user
//!   scope) and `projects[<folder>].mcpServers` (local scope); `<folder>/.mcp.json` (project).
//! - Codex: `~/.codex/config.toml` (`$CODEX_HOME/config.toml`) — `[mcp_servers.<name>]`.
//!
//! Each definition gets a fingerprint of everything that configures it (environment values
//! included, never shown), so a run can remember what it started with and a later change is
//! detected without keeping any value.

use runwick_core::api::mcp::*;
use runwick_core::api::sessions::Tool;
use runwick_core::paths;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Provider configuration files on this computer.
#[derive(Debug, Clone)]
pub struct Files {
    pub claude_json: PathBuf,
    pub codex_toml: PathBuf,
}

impl Files {
    /// From the person's login environment (what their CLIs see).
    pub fn from_login_env() -> Self {
        let env = crate::sessions::env::login_env();
        let home = paths::user_home();
        let dir =
            |var: &str, default: PathBuf| env.get(var).filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or(default);
        Self {
            claude_json: dir("CLAUDE_CONFIG_DIR", home.clone()).join(".claude.json"),
            codex_toml: dir("CODEX_HOME", home.join(".codex")).join("config.toml"),
        }
    }
}

/// A server and the fingerprint of its definition.
#[derive(Debug, Clone)]
pub struct Def {
    pub server: McpServer,
    pub fingerprint: String,
}

/// What each server's definition looked like, by `provider:scope:name`.
pub type Snapshot = BTreeMap<String, String>;

/// Definitions and the files read, for the given providers.
pub fn read(files: &Files, cwd: Option<&str>, providers: &[Tool]) -> (Vec<Def>, Vec<McpSource>) {
    let mut defs = Vec::new();
    let mut sources = Vec::new();
    if providers.contains(&Tool::Claude) {
        read_claude(files, cwd, &mut defs, &mut sources);
    }
    if providers.contains(&Tool::Codex) {
        read_codex(files, &mut defs, &mut sources);
    }
    (defs, sources)
}

/// Fingerprints of the servers a run of `tool` in `cwd` uses (empty for shells).
pub fn snapshot(files: &Files, tool: Tool, cwd: &str) -> Snapshot {
    if !tool.is_agent() {
        return Snapshot::new();
    }
    let (defs, _) = read(files, Some(cwd), &[tool]);
    defs.into_iter().map(|d| (key(&d.server), d.fingerprint)).collect()
}

fn key(s: &McpServer) -> String {
    format!("{}:{}:{}", provider_tag(s.provider), scope_tag(s.scope), s.name)
}

fn provider_tag(t: Tool) -> &'static str {
    match t {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
        Tool::Shell => "shell",
    }
}

fn scope_tag(s: McpScope) -> &'static str {
    match s {
        McpScope::User => "user",
        McpScope::Local => "local",
        McpScope::Project => "project",
        McpScope::Other => "other",
    }
}

fn parse_key(k: &str) -> Option<(Tool, McpScope, String)> {
    let mut it = k.splitn(3, ':');
    let tool = match it.next()? {
        "claude" => Tool::Claude,
        "codex" => Tool::Codex,
        _ => return None,
    };
    let scope = match it.next()? {
        "user" => McpScope::User,
        "local" => McpScope::Local,
        "project" => McpScope::Project,
        _ => McpScope::Other,
    };
    Some((tool, scope, it.next()?.to_string()))
}

/// Servers added, changed or removed between two snapshots.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Vec<McpChange> {
    let mut out = Vec::new();
    let mut push = |k: &str, kind| {
        if let Some((provider, scope, name)) = parse_key(k) {
            out.push(McpChange { provider, scope, name, kind });
        }
    };
    for (k, fp) in new {
        match old.get(k) {
            None => push(k, McpChangeKind::Added),
            Some(o) if o != fp => push(k, McpChangeKind::Changed),
            Some(_) => {}
        }
    }
    for k in old.keys().filter(|k| !new.contains_key(*k)) {
        push(k, McpChangeKind::Removed);
    }
    out
}

fn source(provider: Tool, scope: McpScope, path: &Path) -> McpSource {
    McpSource {
        provider,
        scope,
        path: path.to_string_lossy().into_owned(),
        display: paths::display_path(path),
        exists: path.is_file(),
        error: None,
    }
}

/// Parsed JSON of a large file (`~/.claude.json` keeps history), reused while it is unchanged.
fn cached_json(path: &Path) -> Result<Option<Arc<Value>>, String> {
    type Entry = (PathBuf, SystemTime, u64, Arc<Value>);
    static CACHE: Mutex<Option<Entry>> = Mutex::new(None);
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("읽을 수 없어요: {e}")),
    };
    let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
    if let Ok(c) = CACHE.lock()
        && let Some((p, m, l, v)) = c.as_ref()
        && p == path
        && (*m, *l) == stamp
    {
        return Ok(Some(v.clone()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("읽을 수 없어요: {e}"))?;
    let v: Arc<Value> = Arc::new(serde_json::from_slice(&bytes).map_err(|_| "JSON 형식이 아니에요".to_string())?);
    if let Ok(mut c) = CACHE.lock() {
        *c = Some((path.to_path_buf(), stamp.0, stamp.1, v.clone()));
    }
    Ok(Some(v))
}

fn read_claude(files: &Files, cwd: Option<&str>, defs: &mut Vec<Def>, sources: &mut Vec<McpSource>) {
    let mut user = source(Tool::Claude, McpScope::User, &files.claude_json);
    let config = match cached_json(&files.claude_json) {
        Ok(v) => v,
        Err(e) => {
            user.error = Some(e);
            None
        }
    };
    sources.push(user);
    let display = paths::display_path(&files.claude_json);
    let project = cwd.and_then(|c| config.as_ref().and_then(|v| v.get("projects")?.get(c)));
    let names = |field: &str| -> Vec<String> {
        project
            .and_then(|p| p.get(field)?.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let disabled = names("disabledMcpServers");
    let disabled_project = names("disabledMcpjsonServers");
    if let Some(servers) = config.as_ref().and_then(|v| v.get("mcpServers")?.as_object()) {
        for (name, def) in servers {
            let enabled = !disabled.contains(name);
            defs.push(claude_def(name, def, McpScope::User, &display, enabled));
        }
    }
    let Some(cwd) = cwd else { return };
    if let Some(servers) = project.and_then(|p| p.get("mcpServers")?.as_object()) {
        sources.push(source(Tool::Claude, McpScope::Local, &files.claude_json));
        let location = format!("{display} · {}", paths::display_path(Path::new(cwd)));
        for (name, def) in servers {
            let enabled = !disabled.contains(name);
            defs.push(claude_def(name, def, McpScope::Local, &location, enabled));
        }
    }
    let mcp_json = Path::new(cwd).join(".mcp.json");
    let mut src = source(Tool::Claude, McpScope::Project, &mcp_json);
    if src.exists {
        match std::fs::read(&mcp_json)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|_| "JSON 형식이 아니에요".to_string()))
        {
            Ok(v) => {
                if let Some(servers) = v.get("mcpServers").and_then(|s| s.as_object()) {
                    for (name, def) in servers {
                        let enabled = !disabled_project.contains(name);
                        defs.push(claude_def(name, def, McpScope::Project, &src.display, enabled));
                    }
                }
            }
            Err(e) => src.error = Some(e),
        }
    }
    sources.push(src);
}

fn claude_def(name: &str, def: &Value, scope: McpScope, location: &str, enabled: bool) -> Def {
    let url = def.get("url").and_then(|u| u.as_str());
    let transport = match def.get("type").and_then(|t| t.as_str()) {
        Some("stdio") => McpTransport::Stdio,
        Some("http") => McpTransport::Http,
        Some("sse") => McpTransport::Sse,
        Some(_) => McpTransport::Unknown,
        None if url.is_some() => McpTransport::Http,
        None => McpTransport::Stdio,
    };
    let mut secret_keys = object_keys(def.get("env"));
    secret_keys.extend(object_keys(def.get("headers")));
    let server = McpServer {
        provider: Tool::Claude,
        name: name.to_string(),
        scope,
        location: Some(location.to_string()),
        transport,
        target: target(def, url),
        secret_keys,
        enabled,
    };
    let fingerprint = fingerprint(&server, def);
    Def { server, fingerprint }
}

fn read_codex(files: &Files, defs: &mut Vec<Def>, sources: &mut Vec<McpSource>) {
    let mut src = source(Tool::Codex, McpScope::User, &files.codex_toml);
    if src.exists {
        match std::fs::read_to_string(&files.codex_toml)
            .map_err(|e| format!("읽을 수 없어요: {e}"))
            .and_then(|s| super::toml::parse(&s).map_err(|e| format!("TOML을 읽지 못했어요: {e}")))
        {
            Ok(v) => {
                if let Some(servers) = v.get("mcp_servers").and_then(|s| s.as_object()) {
                    for (name, def) in servers {
                        defs.push(codex_def(name, def, &src.display));
                    }
                }
            }
            Err(e) => src.error = Some(e),
        }
    }
    sources.push(src);
}

fn codex_def(name: &str, def: &Value, location: &str) -> Def {
    let url = def.get("url").and_then(|u| u.as_str());
    let mut secret_keys = object_keys(def.get("env"));
    // Variables forwarded from Codex's own environment.
    if let Some(a) = def.get("env_vars").and_then(|v| v.as_array()) {
        secret_keys.extend(a.iter().filter_map(|v| v.as_str().map(str::to_string)));
    }
    if let Some(v) = def.get("bearer_token_env_var").and_then(|v| v.as_str()) {
        secret_keys.push(v.to_string());
    }
    secret_keys.extend(object_keys(def.get("http_headers")));
    secret_keys.extend(object_keys(def.get("env_http_headers")));
    let server = McpServer {
        provider: Tool::Codex,
        name: name.to_string(),
        scope: McpScope::User,
        location: Some(location.to_string()),
        transport: if url.is_some() { McpTransport::Http } else { McpTransport::Stdio },
        target: target(def, url),
        secret_keys,
        enabled: def.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true),
    };
    let fingerprint = fingerprint(&server, def);
    Def { server, fingerprint }
}

fn object_keys(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_object()).map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

fn fingerprint(server: &McpServer, def: &Value) -> String {
    // serde_json maps are ordered by key, so the text is canonical.
    let mut h = Sha256::new();
    h.update(key(server).as_bytes());
    h.update([0, server.enabled as u8, 0]);
    h.update(def.to_string().as_bytes());
    h.finalize().iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// Command line or URL for display, with secret-looking parts hidden.
fn target(def: &Value, url: Option<&str>) -> String {
    if let Some(u) = url {
        return mask_url(u);
    }
    let command = def.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let args: Vec<String> = def
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut parts = vec![quote(command)];
    parts.extend(mask_args(&args).iter().map(|a| quote(a)));
    parts.join(" ").trim().to_string()
}

const HIDDEN: &str = "••••";

fn sensitive(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["key", "token", "secret", "password", "passwd", "auth", "bearer", "credential", "cookie"]
        .iter()
        .any(|w| n.contains(w))
}

fn mask_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for a in args {
        if hide_next {
            out.push(HIDDEN.to_string());
            hide_next = false;
            continue;
        }
        if let Some(flag) = a.strip_prefix('-')
            && !a.contains('=')
            && sensitive(flag)
        {
            hide_next = true;
            out.push(a.clone());
            continue;
        }
        out.push(mask_word(a));
    }
    out
}

fn mask_word(a: &str) -> String {
    if a.contains("://") {
        return mask_url(a);
    }
    if a.to_ascii_lowercase().starts_with("bearer ") {
        return format!("Bearer {HIDDEN}");
    }
    if let Some((k, _)) = a.split_once([':', '='])
        && sensitive(k)
    {
        let sep = &a[k.len()..k.len() + 1];
        return format!("{k}{sep}{HIDDEN}");
    }
    a.to_string()
}

fn mask_url(u: &str) -> String {
    let (base, query) = match u.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (u, None),
    };
    // user:password@host
    let base = match base.split_once("://") {
        Some((scheme, rest)) => {
            let authority = &rest[..rest.find('/').unwrap_or(rest.len())];
            match authority.rfind('@') {
                Some(at) => format!("{scheme}://{HIDDEN}@{}", &rest[at + 1..]),
                None => base.to_string(),
            }
        }
        None => base.to_string(),
    };
    match query {
        None => base,
        Some(q) => {
            let q: Vec<String> = q
                .split('&')
                .map(|p| match p.split_once('=') {
                    Some((k, _)) if sensitive(k) => format!("{k}={HIDDEN}"),
                    _ => p.to_string(),
                })
                .collect();
            format!("{base}?{}", q.join("&"))
        }
    }
}

fn quote(s: &str) -> String {
    if s.contains(' ') { format!("\"{s}\"") } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn files(dir: &Path) -> Files {
        Files { claude_json: dir.join(".claude.json"), codex_toml: dir.join("codex").join("config.toml") }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn reads_every_claude_scope_and_codex_with_masked_secrets() {
        let d = tempfile::tempdir().unwrap();
        let f = files(d.path());
        let proj = d.path().join("proj");
        let cwd = proj.to_string_lossy().into_owned();
        let claude = json!({
            "oauthAccount": {"x": 1},
            "mcpServers": {
                "github": {"type": "http", "url": "https://api.github.com/mcp?api_key=QK7abc&x=1", "headers": {"Authorization": "Bearer QK7zzz"}},
                "memory": {"command": "npx", "args": ["-y", "server-memory", "--token", "QK7tok123", "API_KEY=QK7sk-1"], "env": {"MEM_SECRET": "QK7v"}}
            },
            "projects": { cwd.clone(): {
                "mcpServers": {"local-db": {"command": "pg-mcp", "args": ["postgres://u:QK7pw@localhost/db"]}},
                "disabledMcpServers": ["memory"],
                "disabledMcpjsonServers": ["shared"]
            }}
        });
        write(&f.claude_json, &claude.to_string());
        write(
            &proj.join(".mcp.json"),
            r#"{"mcpServers":{"shared":{"type":"sse","url":"https://s.example/sse"},"linear":{"url":"https://mcp.linear.app/mcp"}}}"#,
        );
        write(
            &f.codex_toml,
            "[mcp_servers.notion]\nurl = \"https://mcp.notion.com/mcp\"\nbearer_token_env_var = \"NOTION_TOKEN\"\n\n[mcp_servers.off]\ncommand = \"x\"\nenabled = false\n[mcp_servers.off.env]\nK = \"QK7secret-v\"\n",
        );
        let (defs, sources) = read(&f, Some(&cwd), &[Tool::Claude, Tool::Codex]);
        let by = |n: &str| defs.iter().find(|d| d.server.name == n).map(|d| &d.server).unwrap();
        assert_eq!(by("github").scope, McpScope::User);
        assert_eq!(by("github").target, "https://api.github.com/mcp?api_key=••••&x=1");
        assert_eq!(by("github").secret_keys, vec!["Authorization".to_string()]);
        assert_eq!(by("memory").target, "npx -y server-memory --token •••• API_KEY=••••");
        assert!(!by("memory").enabled, "disabled in this project");
        assert_eq!(by("local-db").scope, McpScope::Local);
        assert_eq!(by("local-db").target, "pg-mcp postgres://••••@localhost/db");
        assert_eq!(by("shared").scope, McpScope::Project);
        assert_eq!(by("shared").transport, McpTransport::Sse);
        assert!(!by("shared").enabled);
        assert!(by("linear").enabled);
        assert_eq!(by("notion").provider, Tool::Codex);
        assert_eq!(by("notion").secret_keys, vec!["NOTION_TOKEN".to_string()]);
        assert!(!by("off").enabled);
        assert_eq!(by("off").secret_keys, vec!["K".to_string()]);
        let all = serde_json::to_string(&defs.iter().map(|d| &d.server).collect::<Vec<_>>()).unwrap();
        // Distinctive values: the temporary folder in each location must not match by chance.
        for secret in ["QK7abc", "QK7zzz", "QK7tok123", "QK7sk-1", "QK7pw", "QK7secret-v", "QK7v"] {
            assert!(!all.contains(secret), "{secret} leaked: {all}");
        }
        assert_eq!(sources.len(), 4);
        assert!(sources.iter().all(|s| s.exists && s.error.is_none()));
    }

    #[test]
    fn snapshots_detect_added_changed_and_removed_servers_per_tool() {
        let d = tempfile::tempdir().unwrap();
        let f = files(d.path());
        let cwd = d.path().to_string_lossy().into_owned();
        write(&f.claude_json, r#"{"mcpServers":{"a":{"command":"x","env":{"T":"1"}},"b":{"command":"y"}}}"#);
        let before = snapshot(&f, Tool::Claude, &cwd);
        assert_eq!(before.len(), 2);
        assert!(snapshot(&f, Tool::Shell, &cwd).is_empty());
        // Only an environment value changes: still a change, and no value is kept.
        write(&f.claude_json, r#"{"mcpServers":{"a":{"command":"x","env":{"T":"2"}},"c":{"url":"https://c"}}}"#);
        let after = snapshot(&f, Tool::Claude, &cwd);
        assert!(!format!("{after:?}").contains("\"2\""));
        let mut changes = diff(&before, &after);
        changes.sort_by(|x, y| x.name.cmp(&y.name));
        let got: Vec<(&str, McpChangeKind)> = changes.iter().map(|c| (c.name.as_str(), c.kind)).collect();
        assert_eq!(
            got,
            vec![("a", McpChangeKind::Changed), ("b", McpChangeKind::Removed), ("c", McpChangeKind::Added)]
        );
        assert!(changes.iter().all(|c| c.provider == Tool::Claude && c.scope == McpScope::User));
        assert!(diff(&after, &after).is_empty());
        // Codex runs do not see Claude's servers.
        assert!(snapshot(&f, Tool::Codex, &cwd).is_empty());
    }

    #[test]
    fn broken_files_are_reported_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        let f = files(d.path());
        write(&f.claude_json, "{not json");
        write(&f.codex_toml, "[mcp_servers.x\n");
        let (defs, sources) = read(&f, None, &[Tool::Claude, Tool::Codex]);
        assert!(defs.is_empty());
        assert!(sources.iter().all(|s| s.error.is_some()), "{sources:?}");
    }
}
