//! Links between pages and "내용이 비슷한 문서" suggestions.
//!
//! Links are read from `[[wikilinks]]` (`[[제목]]`, `[[slug|보이는 글]]`, `[[slug#절]]`) and
//! Markdown links to `.md` files. Similarity is local keyword overlap (TF-IDF cosine over words
//! and Hangul syllable pairs): no AI and nothing leaves the computer.

use super::files::slugify;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Link targets as written, in order of appearance (duplicates removed).
pub fn extract(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |t: String| {
        let t = t.trim().to_string();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    };
    let mut in_code = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let mut rest = line;
        while let Some(i) = rest.find("[[") {
            let after = &rest[i + 2..];
            let Some(j) = after.find("]]") else { break };
            let inner = &after[..j];
            let target = inner.split('|').next().unwrap_or("").split('#').next().unwrap_or("");
            push(target.to_string());
            rest = &after[j + 2..];
        }
        let mut rest = line;
        while let Some(i) = rest.find("](") {
            let after = &rest[i + 2..];
            let Some(j) = after.find(')') else { break };
            let href = after[..j].trim().trim_start_matches('<').trim_end_matches('>');
            rest = &after[j + 1..];
            if href.contains("://") || href.starts_with("mailto:") || href.starts_with('#') {
                continue;
            }
            let path = href.split('#').next().unwrap_or("");
            let Some(stem) = path.strip_suffix(".md") else {
                continue;
            };
            let name = stem.rsplit('/').next().unwrap_or(stem);
            push(percent_decode(name));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve a written target to a page slug: exact slug, slug of the text, or title.
pub fn resolve<'a>(target: &str, pages: &'a [(String, String)]) -> Option<&'a str> {
    let lower = target.to_lowercase();
    let as_slug = slugify(target);
    pages
        .iter()
        .find(|(slug, _)| *slug == target)
        .or_else(|| pages.iter().find(|(slug, _)| *slug == as_slug))
        .or_else(|| pages.iter().find(|(_, title)| title.to_lowercase() == lower))
        .map(|(slug, _)| slug.as_str())
}

fn is_hangul(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "not", "you", "can", "will", "have", "has",
    "but", "use", "into", "when", "then", "than", "also", "its", "our", "out", "all", "any", "one", "two",
];

/// Korean words too common to say anything about a page.
const STOP_KO: &[&str] = &[
    "한다",
    "했다",
    "된다",
    "됐다",
    "있다",
    "없다",
    "있는",
    "없는",
    "그리고",
    "하지만",
    "그래서",
    "위해",
    "때문",
    "경우",
    "이것",
    "그것",
    "저것",
    "여기",
    "거기",
    "다시",
    "모든",
    "같은",
    "하는",
    "되는",
];

/// Common particles and verb endings, longest first, stripped so "토큰은" and "토큰을" meet.
const ENDINGS: &[&str] = &[
    "에서는",
    "으로는",
    "에게서",
    "까지는",
    "부터는",
    "이라는",
    "하면서",
    "되어서",
    "해야",
    "으로",
    "에서",
    "에게",
    "까지",
    "부터",
    "처럼",
    "보다",
    "하고",
    "이다",
    "한다",
    "된다",
    "하면",
    "되면",
    "하는",
    "되는",
    "했다",
    "됐다",
    "해서",
    "되어",
    "이고",
    "라는",
    "하여",
    "은",
    "는",
    "이",
    "가",
    "을",
    "를",
    "에",
    "의",
    "로",
    "과",
    "와",
    "도",
    "만",
    "들",
    "한",
    "된",
    "할",
    "될",
    "면",
    "고",
    "며",
];

fn stem(word: &str) -> &str {
    for e in ENDINGS {
        if let Some(s) = word.strip_suffix(e)
            && s.chars().count() >= 2
        {
            return s;
        }
    }
    word
}

/// Words of 3+ letters and Korean stems of 2+ syllables (particles and common endings
/// stripped). Long Korean compounds also give their syllable pairs.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let w = word.to_lowercase();
        if w.chars().any(is_hangul) {
            let st = stem(&w);
            let s: Vec<char> = st.chars().collect();
            if s.len() < 2 || STOP_KO.contains(&st) {
                continue;
            }
            out.push(s.iter().collect());
            if s.len() >= 4 {
                out.extend(
                    s.windows(2).filter(|p| p.iter().all(|c| is_hangul(*c))).map(|p| p.iter().collect::<String>()),
                );
            }
        } else if w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()) && !STOP.contains(&w.as_str()) {
            out.push(w);
        }
    }
    out
}

pub struct Similar {
    pub a: usize,
    pub b: usize,
    pub score: f32,
    /// Most telling shared words.
    pub shared: Vec<String>,
}

/// Distinct words two documents must share before they are called similar.
const MIN_SHARED: usize = 3;

/// Pairs of documents whose TF-IDF cosine is at least `threshold` and that share at least
/// [`MIN_SHARED`] words, best first.
pub fn similar(docs: &[String], threshold: f32, max_pairs: usize) -> Vec<Similar> {
    let n = docs.len();
    if n < 2 {
        return Vec::new();
    }
    let tfs: Vec<HashMap<String, f32>> = docs
        .iter()
        .map(|d| {
            let mut tf: HashMap<String, f32> = HashMap::new();
            for t in tokens(d) {
                *tf.entry(t).or_default() += 1.0;
            }
            tf
        })
        .collect();
    let mut df: HashMap<&str, f32> = HashMap::new();
    for tf in &tfs {
        for t in tf.keys() {
            *df.entry(t.as_str()).or_default() += 1.0;
        }
    }
    // Smoothed idf; a word in every document carries no signal.
    let idf = |t: &str| ((n as f32 + 1.0) / (df.get(t).copied().unwrap_or(0.0) + 1.0)).ln();
    let vecs: Vec<BTreeMap<String, f32>> = tfs
        .iter()
        .map(|tf| {
            let mut v: BTreeMap<String, f32> =
                tf.iter().map(|(t, c)| (t.clone(), (1.0 + c.ln()) * idf(t))).filter(|(_, w)| *w > 0.0).collect();
            let norm = v.values().map(|w| w * w).sum::<f32>().sqrt();
            if norm > 0.0 {
                v.values_mut().for_each(|w| *w /= norm);
            }
            v
        })
        .collect();
    let mut out = Vec::new();
    for a in 0..n {
        for b in a + 1..n {
            let (small, big) = if vecs[a].len() <= vecs[b].len() { (&vecs[a], &vecs[b]) } else { (&vecs[b], &vecs[a]) };
            let mut shared: Vec<(f32, &String)> = Vec::new();
            let mut score = 0.0;
            for (t, w) in small {
                if let Some(w2) = big.get(t) {
                    score += w * w2;
                    shared.push((w * w2, t));
                }
            }
            if score >= threshold && shared.len() >= MIN_SHARED {
                shared.sort_by(|x, y| y.0.total_cmp(&x.0));
                let mut seen = HashSet::new();
                let shared =
                    shared.into_iter().map(|(_, t)| t.clone()).filter(|t| seen.insert(t.clone())).take(5).collect();
                out.push(Similar { a, b, score, shared });
            }
        }
    }
    out.sort_by(|x, y| y.score.total_cmp(&x.score));
    out.truncate(max_pairs);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_wikilinks_and_markdown_links() {
        let body = "본문 [[로그인과 세션]] 그리고 [[deploy|배포 절차]], [[결정#쿠키]]\n\
                    [가이드](./guide.md) [외부](https://x.dev/a.md) [절](#제목)\n\
                    [한글](%EB%B0%B0%ED%8F%AC.md)\n```\n[[코드 안은 무시]]\n```\n[[로그인과 세션]]";
        assert_eq!(extract(body), vec!["로그인과 세션", "deploy", "결정", "guide", "배포"]);
    }

    #[test]
    fn resolves_by_slug_slugified_text_or_title() {
        let pages = vec![
            ("로그인과-세션".to_string(), "로그인과 세션".to_string()),
            ("deploy".to_string(), "Deploy Steps".to_string()),
        ];
        assert_eq!(resolve("로그인과 세션", &pages), Some("로그인과-세션"));
        assert_eq!(resolve("deploy steps", &pages), Some("deploy"));
        assert_eq!(resolve("deploy", &pages), Some("deploy"));
        assert_eq!(resolve("없는 문서", &pages), None);
    }

    #[test]
    fn similar_pages_share_korean_stems() {
        let docs = vec![
            "토큰 만료는 서버 시각을 기준으로 한다. 토큰을 갱신할 때 만료 전에만 갱신한다.".to_string(),
            "토큰이 만료되면 갱신을 시도하고 실패하면 로그인 화면으로 보낸다. 서버 시각 기준.".to_string(),
            "배포 절차: 스테이징 확인, 태그, 롤백 순서".to_string(),
            "커밋 메시지 규칙과 브랜치 이름".to_string(),
        ];
        let pairs = similar(&docs, 0.2, 10);
        assert_eq!(pairs.len(), 1, "only the two token pages are alike");
        assert_eq!((pairs[0].a, pairs[0].b), (0, 1));
        assert!(pairs[0].shared.iter().any(|t| t == "토큰"));
        assert!(pairs[0].shared.iter().any(|t| t == "만료"));
    }
}
