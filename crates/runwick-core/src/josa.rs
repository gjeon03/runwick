//! Korean particles (조사) that change with the sound the word before them ends in: 이/가,
//! 을/를, 은/는, 와/과 and (으)로 (`로` also after ㄹ). Hangul is read from its final consonant
//! (받침); digits, Latin letters and file names are read the way they are usually pronounced
//! (`README.md` → 엠디, `URL` → 유알엘, `GitHub` → 깃허브, `10` → 십). When the ending cannot be
//! read (an emoji, Chinese characters, an empty name) the paired form such as `이(가)` is used.

/// How a word sounds at its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Vowel,
    /// Final ㄹ: like a vowel for (으)로, like a consonant otherwise.
    Rieul,
    Consonant,
}

/// The particles that depend on the preceding word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Particle {
    /// 이 / 가
    IGa,
    /// 을 / 를
    EulReul,
    /// 은 / 는
    EunNeun,
    /// 과 / 와
    GwaWa,
    /// 으로 / 로
    EuroRo,
}

/// The particle that follows `word`.
pub fn particle(word: &str, p: Particle) -> &'static str {
    use Ending::*;
    use Particle::*;
    match (p, ending(word)) {
        (IGa, Some(Vowel)) => "가",
        (IGa, Some(_)) => "이",
        (IGa, None) => "이(가)",
        (EulReul, Some(Vowel)) => "를",
        (EulReul, Some(_)) => "을",
        (EulReul, None) => "을(를)",
        (EunNeun, Some(Vowel)) => "는",
        (EunNeun, Some(_)) => "은",
        (EunNeun, None) => "은(는)",
        (GwaWa, Some(Vowel)) => "와",
        (GwaWa, Some(_)) => "과",
        (GwaWa, None) => "와(과)",
        (EuroRo, Some(Consonant)) => "으로",
        (EuroRo, Some(_)) => "로",
        (EuroRo, None) => "(으)로",
    }
}

/// `word` followed by its particle.
pub fn with(word: &str, p: Particle) -> String {
    format!("{word}{}", particle(word, p))
}

/// `word` + 이/가.
pub fn i_ga(word: &str) -> String {
    with(word, Particle::IGa)
}

/// `word` + 을/를.
pub fn eul_reul(word: &str) -> String {
    with(word, Particle::EulReul)
}

/// `word` + 은/는.
pub fn eun_neun(word: &str) -> String {
    with(word, Particle::EunNeun)
}

/// `word` + 과/와.
pub fn gwa_wa(word: &str) -> String {
    with(word, Particle::GwaWa)
}

/// `word` + 으로/로.
pub fn euro_ro(word: &str) -> String {
    with(word, Particle::EuroRo)
}

/// The ending of the last letter or digit (closing quotes, brackets and dots are skipped).
fn ending(word: &str) -> Option<Ending> {
    let end = word.trim_end_matches(|c: char| !c.is_alphanumeric());
    let last = end.chars().last()?;
    match last {
        '가'..='힣' => Some(match (last as u32 - 0xAC00) % 28 {
            0 => Ending::Vowel,
            8 => Ending::Rieul,
            _ => Ending::Consonant,
        }),
        // Compatibility jamo: ㄱ…ㅎ are consonants (ㄹ is ㄹ), ㅏ…ㅣ vowels.
        'ㄹ' => Some(Ending::Rieul),
        'ㄱ'..='ㅎ' => Some(Ending::Consonant),
        'ㅏ'..='ㅣ' => Some(Ending::Vowel),
        '0'..='9' => Some(digit(last)),
        'a'..='z' | 'A'..='Z' => {
            let start = end.trim_end_matches(|c: char| c.is_ascii_alphabetic()).len();
            Some(latin(&end[start..]))
        }
        _ => None,
    }
}

/// The last digit as read in Sino-Korean (영, 일, 이, 삼 …); a final 0 reads 영/십/백/천/만.
fn digit(d: char) -> Ending {
    match d {
        '1' | '7' | '8' => Ending::Rieul,
        '0' | '3' | '6' => Ending::Consonant,
        _ => Ending::Vowel,
    }
}

/// A run of Latin letters. Words without vowels (`md`, `html`) and short capital acronyms
/// (`CLI`, `URL`) are spelled out letter by letter; other words follow common transcriptions.
fn latin(token: &str) -> Ending {
    let lower = token.to_ascii_lowercase();
    let b = lower.as_bytes();
    let n = b.len();
    let vowel = |c: u8| matches!(c, b'a' | b'e' | b'i' | b'o' | b'u');
    let spelled =
        !b.iter().any(|&c| vowel(c) || c == b'y') || (n <= 3 && token.bytes().all(|c| c.is_ascii_uppercase()));
    if spelled {
        // 엘, 알 / 엠, 엔 / every other letter name ends in a vowel (에이, 비, 씨 …).
        return match b[n - 1] {
            b'l' | b'r' => Ending::Rieul,
            b'm' | b'n' => Ending::Consonant,
            _ => Ending::Vowel,
        };
    }
    let at = |i: usize| if i < n { b[n - 1 - i] } else { 0 };
    let (last, prev, before) = (at(0), at(1), at(2));
    // A short closed syllable keeps its stop as 받침: git 깃, app 앱, web 웹, Mac 맥, book 북.
    let closed =
        (vowel(prev) && (!vowel(before) || matches!((before, prev), (b'o', b'o') | (b'o', b'u')))) || prev == last;
    match last {
        b'm' | b'n' => Ending::Consonant,
        b'g' if prev == b'n' => Ending::Consonant,
        b'l' => Ending::Rieul,
        b'e' if prev == b'l' => Ending::Rieul,
        b'e' if matches!(prev, b'm' | b'n') => Ending::Consonant,
        b'k' if prev == b'c' => Ending::Consonant,
        b'b' if lower.ends_with("hub") => Ending::Vowel,
        b'b' | b'c' | b'k' | b'p' | b't' if closed => Ending::Consonant,
        _ => Ending::Vowel,
    }
}

#[cfg(test)]
mod tests {
    use super::Particle::*;
    use super::*;

    #[test]
    fn hangul_follows_the_final_consonant() {
        assert_eq!(i_ga("기본 공간"), "기본 공간이");
        assert_eq!(i_ga("세션"), "세션이");
        assert_eq!(i_ga("인계 문서"), "인계 문서가");
        assert_eq!(eul_reul("작업실"), "작업실을");
        assert_eq!(eul_reul("서버"), "서버를");
        assert_eq!(eun_neun("공간"), "공간은");
        assert_eq!(eun_neun("기기"), "기기는");
        assert_eq!(gwa_wa("작업실"), "작업실과");
        assert_eq!(gwa_wa("개발 서버"), "개발 서버와");
        assert_eq!(particle("서버 기준", GwaWa), "과");
        assert_eq!(particle("세 번까지", GwaWa), "와");
    }

    #[test]
    fn euro_ro_treats_rieul_like_a_vowel() {
        assert_eq!(euro_ro("기본 공간"), "기본 공간으로");
        assert_eq!(euro_ro("서버"), "서버로");
        assert_eq!(euro_ro("작업실"), "작업실로");
        assert_eq!(euro_ro("셸"), "셸로");
        assert_eq!(euro_ro("ㄹ"), "ㄹ로");
        assert_eq!(euro_ro("ㅋ"), "ㅋ으로");
    }

    #[test]
    fn digits_read_in_sino_korean() {
        assert_eq!(particle("v2", EulReul), "를");
        assert_eq!(particle("v1", EuroRo), "로");
        assert_eq!(particle("1", IGa), "이");
        assert_eq!(particle("3", IGa), "이");
        assert_eq!(particle("4", IGa), "가");
        assert_eq!(particle("10", EuroRo), "으로");
        assert_eq!(particle("2.0", EuroRo), "으로");
        assert_eq!(particle("v0.4.1", EulReul), "을");
        assert_eq!(particle("7", EuroRo), "로");
    }

    #[test]
    fn file_names_and_acronyms_are_spelled_out() {
        assert_eq!(i_ga("README.md"), "README.md가");
        assert_eq!(i_ga("auth-notes.md"), "auth-notes.md가");
        assert_eq!(particle("index.html", EulReul), "을");
        assert_eq!(particle("index.html", EuroRo), "로");
        assert_eq!(particle("main.rs", IGa), "가");
        assert_eq!(particle("config.json", IGa), "이");
        assert_eq!(particle("CLI", EulReul), "를");
        assert_eq!(particle("MCP", EunNeun), "는");
        assert_eq!(particle("URL", EuroRo), "로");
        assert_eq!(particle("URL", IGa), "이");
        assert_eq!(particle("IAM", IGa), "이");
        assert_eq!(particle("zsh", IGa), "가");
        assert_eq!(particle("SSH", GwaWa), "와");
    }

    #[test]
    fn english_words_follow_common_transcriptions() {
        assert_eq!(i_ga("Claude Code"), "Claude Code가");
        assert_eq!(i_ga("Codex"), "Codex가");
        assert_eq!(particle("GitHub", EulReul), "를");
        assert_eq!(particle("git", EulReul), "을");
        assert_eq!(particle("Runwick", IGa), "이");
        assert_eq!(particle("Mac", EuroRo), "으로");
        assert_eq!(particle("MacBook", IGa), "이");
        assert_eq!(particle("집 Mac mini", GwaWa), "와");
        assert_eq!(particle("shell", EuroRo), "로");
        assert_eq!(particle("shell", IGa), "이");
        assert_eq!(particle("file", EulReul), "을");
        assert_eq!(particle("main", EuroRo), "으로");
        assert_eq!(particle("auth-tests", EuroRo), "로");
        assert_eq!(particle("worktree", IGa), "가");
        assert_eq!(particle("app", IGa), "이");
        assert_eq!(particle("test", IGa), "가");
        assert_eq!(particle("server", EuroRo), "로");
        assert_eq!(particle("string", EuroRo), "으로");
        assert_eq!(particle("time", IGa), "이");
        assert_eq!(particle("Gemini", IGa), "가");
        assert_eq!(particle("bash", EulReul), "를");
    }

    #[test]
    fn quotes_and_brackets_are_skipped() {
        assert_eq!(euro_ro("‘집 Mac mini’"), "‘집 Mac mini’로");
        assert_eq!(euro_ro("‘작업실’"), "‘작업실’로");
        assert_eq!(i_ga("“세션 만료 원인”"), "“세션 만료 원인”이");
        assert_eq!(i_ga("Claude Code (beta)"), "Claude Code (beta)가");
    }

    #[test]
    fn unreadable_endings_use_the_paired_form() {
        assert_eq!(i_ga(""), "이(가)");
        assert_eq!(eul_reul("🙂"), "🙂을(를)");
        assert_eq!(euro_ro("東京"), "東京(으)로");
        assert_eq!(gwa_wa("…"), "…와(과)");
        assert_eq!(eun_neun("中"), "中은(는)");
    }
}
