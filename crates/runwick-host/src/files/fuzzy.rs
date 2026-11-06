//! Fuzzy file-name matching: the query's characters must appear in order (a subsequence);
//! the best alignment wins, preferring the file name, word starts and consecutive runs.
//! Korean works by syllable, and a typed initial consonant (`ㅎ`) matches syllables that start
//! with it (`회`, `한`), so `ㅎㅇ` finds `회의`.

const MATCH: i64 = 16;
const BOUNDARY: i64 = 10;
const NAME_START: i64 = 14;
const CAMEL: i64 = 8;
const CONSECUTIVE: i64 = 7;
const IN_NAME: i64 = 6;
const GAP_START: i64 = 3;
const GAP_EXT: i64 = 1;
/// Longest target the full alignment runs on; longer paths use their last part.
const MAX_TARGET: usize = 512;
const NEG: i64 = i64::MIN / 4;

/// Initial consonants in syllable order (compatibility jamo, as typed).
const CHOSEONG: [char; 19] =
    ['ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ'];

fn initial_of(c: char) -> Option<char> {
    let u = c as u32;
    (0xAC00..0xAC00 + 11172).contains(&u).then(|| CHOSEONG[((u - 0xAC00) / 588) as usize])
}

fn same(q: char, t: char) -> bool {
    q == t || (CHOSEONG.contains(&q) && initial_of(t) == Some(q))
}

/// Prepared target: lowercased characters plus where words and the file name start.
pub struct Target {
    lower: Vec<char>,
    bonus: Vec<i64>,
    name_start: usize,
}

impl Target {
    pub fn new(display_path: &str) -> Self {
        let chars: Vec<char> = display_path.chars().collect();
        let name_start = chars.iter().rposition(|c| *c == '/').map(|i| i + 1).unwrap_or(0);
        let bonus = (0..chars.len())
            .map(|i| {
                let prev = if i == 0 { '/' } else { chars[i - 1] };
                let cur = chars[i];
                if i == name_start {
                    NAME_START
                } else if matches!(prev, '/' | '_' | '-' | '.' | ' ') {
                    BOUNDARY
                } else if prev.is_lowercase() && cur.is_uppercase() {
                    CAMEL
                } else {
                    0
                }
            })
            .collect();
        let lower = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
        Self { lower, bonus, name_start }
    }
}

pub struct Match {
    pub score: i64,
    /// Matched character positions in the display path.
    pub positions: Vec<u32>,
}

/// Lowercased query characters (spaces are ignored).
pub fn prepare_query(q: &str) -> Vec<char> {
    let composed = runwick_core::api::files::compose_hangul(q);
    composed.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

pub fn score(query: &[char], t: &Target) -> Option<Match> {
    let n_all = t.lower.len();
    let m = query.len();
    if m == 0 || m > n_all {
        return None;
    }
    // Cheap rejection: must be a subsequence at all.
    let mut qi = 0;
    for c in &t.lower {
        if qi < m && same(query[qi], *c) {
            qi += 1;
        }
    }
    if qi < m {
        return None;
    }
    let off = n_all.saturating_sub(MAX_TARGET);
    let tl = &t.lower[off..];
    let n = tl.len();
    // h[i][j]: best score with query[..=i] aligned and query[i] at tl[j]; from[i][j]: previous j.
    let mut h = vec![NEG; m * n];
    let mut from = vec![usize::MAX; m * n];
    for i in 0..m {
        // Best of row i-1 over k <= j-2, already charged the gap up to j.
        let mut gap_best = NEG;
        let mut gap_from = usize::MAX;
        for j in 0..n {
            if i > 0 && j >= 2 {
                let cand = h[(i - 1) * n + j - 2];
                let carried = gap_best.saturating_sub(GAP_EXT);
                if cand > NEG && cand - GAP_START >= carried {
                    gap_best = cand - GAP_START;
                    gap_from = j - 2;
                } else {
                    gap_best = carried;
                }
            }
            if !same(query[i], tl[j]) {
                continue;
            }
            let pos = off + j;
            let gain = MATCH + t.bonus[pos] + if pos >= t.name_start { IN_NAME } else { 0 };
            let (best, prev) = if i == 0 {
                (gain, usize::MAX)
            } else {
                let mut best = (NEG, usize::MAX);
                if j >= 1 && h[(i - 1) * n + j - 1] > NEG {
                    best = (h[(i - 1) * n + j - 1] + CONSECUTIVE, j - 1);
                }
                if gap_best > NEG && gap_best > best.0 {
                    best = (gap_best, gap_from);
                }
                if best.0 == NEG {
                    continue;
                }
                (best.0 + gain, best.1)
            };
            h[i * n + j] = best;
            from[i * n + j] = prev;
        }
    }
    let last = m - 1;
    let (end, best) = (0..n).map(|j| (j, h[last * n + j])).max_by_key(|(j, s)| (*s, *j))?;
    if best == NEG {
        return None;
    }
    let mut positions = vec![0u32; m];
    let mut j = end;
    for i in (0..m).rev() {
        positions[i] = (off + j) as u32;
        j = from[i * n + j];
        if j == usize::MAX {
            break;
        }
    }
    // Shorter paths win ties; a match entirely inside the file name wins more.
    let all_in_name = positions.first().is_some_and(|p| *p as usize >= t.name_start);
    let score = best + if all_in_name { 20 } else { 0 } - (n_all as i64 / 16);
    Some(Match { score, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(q: &str, path: &str) -> Option<i64> {
        score(&prepare_query(q), &Target::new(path)).map(|m| m.score)
    }

    #[test]
    fn subsequence_required() {
        assert!(s("xyz", "src/main.rs").is_none());
        assert!(s("smr", "src/main.rs").is_some());
        assert!(s("", "a").is_none());
    }

    #[test]
    fn prefers_name_word_starts_and_runs() {
        // The file name beats a scattered path match.
        assert!(s("auth", "src/auth.ts") > s("auth", "src/a/u/t/h.ts"));
        // Consecutive beats scattered.
        assert!(s("main", "src/main.rs") > s("main", "src/my_aim_net.rs"));
        // Shorter path wins a tie.
        assert!(s("readme", "README.md") > s("readme", "docs/old/README.md"));
        // Word starts in names: "sts" -> session_test.ts
        assert!(s("st", "session_test.ts") > s("st", "aslast.ts"));
    }

    #[test]
    fn positions_point_at_matched_chars() {
        let m = score(&prepare_query("mr"), &Target::new("src/main.rs")).unwrap();
        let path: Vec<char> = "src/main.rs".chars().collect();
        let got: String = m.positions.iter().map(|p| path[*p as usize]).collect();
        assert_eq!(got.to_lowercase(), "mr");
        assert_eq!(m.positions[0], 4);
    }

    #[test]
    fn korean_by_syllable_and_initial_consonant() {
        assert!(s("회의", "docs/회의 메모.md").is_some());
        assert!(s("ㅎㅇ", "docs/회의 메모.md").is_some());
        assert!(s("메모", "docs/회의 메모.md").is_some());
        assert!(s("회메", "docs/회의 메모.md").is_some());
        assert!(s("ㅁㅁ", "docs/회의 메모.md").is_some());
        assert!(s("ㄱ", "docs/회의 메모.md").is_none());
        // Decomposed (NFD) query text is composed first.
        assert!(s("\u{1112}\u{116c}\u{110b}\u{1174}", "회의.md").is_some());
    }
}
