//! Composing Korean (Hangul) text to Unicode NFC.
//!
//! macOS file systems and Finder may hand back file names with Hangul decomposed into jamo
//! (NFD). Names that identify records (wiki slugs) are compared composed, so the same page never
//! appears twice. Only Hangul is composed; other text is returned unchanged.

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const S_COUNT: u32 = L_COUNT * V_COUNT * T_COUNT;

/// `s` with decomposed Hangul syllables composed (the NFC form for Korean).
pub fn hangul(s: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let c = ch as u32;
        if let Some(&last) = out.last() {
            let p = last as u32;
            // leading consonant + vowel → LV syllable
            if (L_BASE..L_BASE + L_COUNT).contains(&p) && (V_BASE..V_BASE + V_COUNT).contains(&c) {
                let lv = S_BASE + ((p - L_BASE) * V_COUNT + (c - V_BASE)) * T_COUNT;
                if let (Some(sy), Some(slot)) = (char::from_u32(lv), out.last_mut()) {
                    *slot = sy;
                    continue;
                }
            }
            // LV syllable + trailing consonant → LVT syllable
            if (S_BASE..S_BASE + S_COUNT).contains(&p)
                && (p - S_BASE).is_multiple_of(T_COUNT)
                && (T_BASE + 1..T_BASE + T_COUNT).contains(&c)
                && let (Some(sy), Some(slot)) = (char::from_u32(p + (c - T_BASE)), out.last_mut())
            {
                *slot = sy;
                continue;
            }
        }
        out.push(ch);
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decomposed_korean_becomes_composed() {
        let nfd = "\u{1107}\u{1162}\u{1111}\u{1169}-\u{110C}\u{1165}\u{11AF}\u{110E}\u{1161}"; // 배포-절차
        assert_eq!(hangul(nfd), "배포-절차");
        assert_eq!(hangul("배포-절차"), "배포-절차");
        assert_eq!(hangul("deploy-v2"), "deploy-v2");
        assert_eq!(hangul("\u{1112}\u{116C}\u{110B}\u{1174} \u{1106}\u{1166}\u{1106}\u{1169}"), "회의 메모");
    }
}
