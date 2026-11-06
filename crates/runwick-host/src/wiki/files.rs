//! Wiki pages on disk: `<wiki>/<project>/<slug>.md` with a small frontmatter.
//!
//! The frontmatter is a YAML subset: `key: value` lines where values are written as JSON
//! (a JSON string or array is also valid YAML). Hand-edited plain values (`title: 메모`,
//! `tags: [a, b]`, block lists) are read too.

use runwick_core::Ts;
use runwick_core::api::wiki::{Author, PageStatus};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Meta {
    pub title: Option<String>,
    pub created_by: Option<Author>,
    pub status: Option<PageStatus>,
    pub tags: Vec<String>,
    pub sources: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub meta: Meta,
    pub body: String,
    /// The file starts with a frontmatter block (absent fields then mean "empty").
    pub has_frontmatter: bool,
}

pub fn project_dir(root: &Path, project: Uuid) -> PathBuf {
    root.join(project.to_string())
}

pub fn page_path(root: &Path, project: Uuid, slug: &str) -> PathBuf {
    project_dir(root, project).join(format!("{slug}.md"))
}

/// File name stem → slug when the file follows our naming; any stem is accepted as-is.
pub fn slug_of(path: &Path) -> Option<String> {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    // Composed (NFC): Finder and HFS+ may hand back decomposed Korean names for the same page.
    (!stem.is_empty() && !stem.starts_with('.')).then(|| runwick_core::nfc::hangul(stem))
}

pub use runwick_core::api::wiki::slugify;

pub fn hash(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Modification time (ns) and size, for a cheap "did it change" check before hashing.
pub fn stamp(path: &Path) -> Option<(i64, i64)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Some((mtime, m.len() as i64))
}

pub fn render(meta: &Meta, updated: &Ts, body: &str) -> String {
    let mut s = String::from("---\n");
    let js = |v: &str| serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into());
    s.push_str(&format!("title: {}\n", js(meta.title.as_deref().unwrap_or(""))));
    let author = match meta.created_by.unwrap_or_default() {
        Author::User => "user",
        Author::Ai => "ai",
        Author::Import => "import",
    };
    s.push_str(&format!("created_by: {author}\n"));
    let status = match meta.status.unwrap_or_default() {
        PageStatus::Draft => "draft",
        PageStatus::Reviewed => "reviewed",
    };
    s.push_str(&format!("status: {status}\n"));
    s.push_str(&format!("tags: {}\n", serde_json::to_string(&meta.tags).unwrap_or_else(|_| "[]".into())));
    let sources: Vec<String> = meta.sources.iter().map(Uuid::to_string).collect();
    s.push_str(&format!("sources: {}\n", serde_json::to_string(&sources).unwrap_or_else(|_| "[]".into())));
    s.push_str(&format!("updated: {}\n", js(&crate::store::ts_to_sql(updated))));
    s.push_str("---\n\n");
    s.push_str(body.trim_end_matches('\n'));
    s.push('\n');
    s
}

pub fn parse(text: &str) -> Doc {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end() != "---" {
        return Doc { meta: Meta::default(), body: text.to_string(), has_frontmatter: false };
    }
    let mut fm: Vec<&str> = Vec::new();
    let mut consumed = first.len();
    let mut closed = false;
    for line in lines.by_ref() {
        consumed += line.len();
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        fm.push(line.trim_end_matches(['\n', '\r']));
    }
    if !closed {
        return Doc { meta: Meta::default(), body: text.to_string(), has_frontmatter: false };
    }
    let body = text[consumed..].trim_start_matches(['\n', '\r']).to_string();
    Doc { meta: parse_meta(&fm), body, has_frontmatter: true }
}

fn parse_meta(lines: &[&str]) -> Meta {
    let mut meta = Meta::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.starts_with(' ') || key.starts_with('\t') {
            continue;
        }
        let key = key.trim();
        let mut value = value.trim().to_string();
        // Block list: following "- item" lines.
        if value.is_empty() {
            let mut items = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with("- ") {
                items.push(unquote(lines[i].trim_start()[2..].trim()));
                i += 1;
            }
            if !items.is_empty() {
                value = serde_json::to_string(&items).unwrap_or_default();
            }
        }
        match key {
            "title" => meta.title = Some(scalar(&value)).filter(|t| !t.is_empty()),
            "created_by" => {
                meta.created_by = match scalar(&value).as_str() {
                    "ai" => Some(Author::Ai),
                    "import" => Some(Author::Import),
                    "user" => Some(Author::User),
                    _ => None,
                }
            }
            "status" => {
                meta.status = match scalar(&value).as_str() {
                    "draft" => Some(PageStatus::Draft),
                    "reviewed" => Some(PageStatus::Reviewed),
                    _ => None,
                }
            }
            "tags" => meta.tags = list(&value).into_iter().filter(|t| !t.is_empty()).collect(),
            "sources" => meta.sources = list(&value).iter().filter_map(|s| s.parse().ok()).collect(),
            _ => {}
        }
    }
    meta
}

fn scalar(v: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => unquote(v),
    }
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

fn list(v: &str) -> Vec<String> {
    if let Ok(items) = serde_json::from_str::<Vec<String>>(v) {
        return items;
    }
    let inner = v.trim().trim_start_matches('[').trim_end_matches(']');
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner.split(',').map(unquote).collect()
}

/// Write through a temporary file and rename, so a crash never leaves half a page.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", Uuid::new_v4()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// First non-heading, non-empty line (Markdown markers stripped), for list excerpts.
pub fn excerpt(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```") && !l.starts_with("---"))
        .unwrap_or("");
    let line = line.trim_start_matches(['-', '*', '>', ' ']).replace("**", "").replace('`', "");
    plain_links(&line).chars().take(140).collect()
}

/// Links as the words they show: `[[slug|글]]` → 글, `[[제목#절]]` → 제목, `[글](url)` → 글,
/// `![그림](src)` → 그림.
fn plain_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("[[")
            && let Some(end) = after.find("]]")
        {
            let inner = &after[..end];
            let shown = match inner.split_once('|') {
                Some((_, label)) => label,
                None => inner.split('#').next().unwrap_or(inner),
            };
            out.push_str(shown.trim());
            rest = &after[end + 2..];
            continue;
        }
        let image = rest.starts_with("![");
        if let Some(after) = rest.strip_prefix(if image { "![" } else { "[" })
            && let Some(close) = after.find("](")
            && let Some(paren) = after[close + 2..].find(')')
        {
            out.push_str(&after[..close]);
            rest = &after[close + 2 + paren + 1..];
            continue;
        }
        let ch = rest.chars().next().unwrap_or_default();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Title from the first `# ` heading, else the file name.
pub fn title_from(body: &str, fallback: &str) -> String {
    body.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_roundtrips_korean() {
        let meta = Meta {
            title: Some("로그인과 세션: \"만료\" 처리".into()),
            created_by: Some(Author::Ai),
            status: Some(PageStatus::Draft),
            tags: vec!["인증".into(), "세션".into()],
            sources: vec![Uuid::new_v4()],
        };
        let body = "# 로그인과 세션\n\n세션은 서버 만료 시각을 기준으로 한다.\n";
        let text = render(&meta, &runwick_core::now(), body);
        let doc = parse(&text);
        assert_eq!(doc.meta, meta);
        assert_eq!(doc.body, body);
    }

    #[test]
    fn hand_written_frontmatter_is_read() {
        let doc = parse("---\ntitle: 배포 절차\ntags:\n  - 배포\n  - '규칙'\nstatus: draft\n---\n본문\n");
        assert_eq!(doc.meta.title.as_deref(), Some("배포 절차"));
        assert_eq!(doc.meta.tags, vec!["배포", "규칙"]);
        assert_eq!(doc.meta.status, Some(PageStatus::Draft));
        assert_eq!(doc.body, "본문\n");
        let doc = parse("---\ntags: [a, \"b\"]\n---\nx");
        assert_eq!(doc.meta.tags, vec!["a", "b"]);
        // No frontmatter or an unclosed one: everything is body.
        assert_eq!(parse("# 제목\n본문").body, "# 제목\n본문");
        assert_eq!(parse("---\ntitle: x\n본문").meta, Meta::default());
    }

    #[test]
    fn slugs_keep_korean_and_drop_punctuation() {
        assert_eq!(slugify("로그인과 세션"), "로그인과-세션");
        assert_eq!(slugify("  Deploy: Steps / v2 "), "deploy-steps-v2");
        assert_eq!(slugify("!!!"), "page");
        assert_eq!(excerpt("# 제목\n\n- **서버** 만료 시각 `exp`"), "서버 만료 시각 exp");
        assert_eq!(
            excerpt("원인은 [[세션 만료 원인]]과 [[auth#쿠키|쿠키 규칙]], [배포](deploy.md) ![그림](a.png) [괄호]"),
            "원인은 세션 만료 원인과 쿠키 규칙, 배포 그림 [괄호]"
        );
        assert_eq!(title_from("앞\n# 제목 하나\n", "file"), "제목 하나");
    }
}
