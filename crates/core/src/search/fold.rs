//! Match normalization for snippets, highlights and `matched_fields` (#18).
//!
//! The FTS indexes match through the unicode61 tokenizer (`remove_diacritics 1`), which
//! ignores case, Latin diacritics and how tokens are separated, so a phrase may span a PDF
//! line break ("solid\r\nelectrolyte") or match "Müller" for "muller". Locating the match for
//! the excerpt uses the same approximation: lowercase, Latin letters without their
//! diacritics (precomposed or combining marks) and every run of whitespace/control
//! characters as one space. Each folded character keeps the byte range of the original text
//! it came from, so a folded match maps back to exact original offsets.
use std::{collections::HashMap, sync::OnceLock};

/// Lowercase precomposed Latin letters (U+00C0..U+024F, U+1E00..U+1EFF) whose canonical
/// decomposition is an ASCII letter plus combining marks, as (letter, base) pairs.
const LATIN: &str = "\
    àaáaâaãaäaåaçcèeéeêeëeìiíiîiïiñnòoóoôoõo\
    öoùuúuûuüuýyÿyāaăaąaćcĉcċcčcďdēeĕeėeęeěe\
    ĝgğgġgģgĥhĩiīiĭiįiĵjķkĺlļlľlńnņnňnōoŏoőo\
    ŕrŗrřrśsŝsşsšsţtťtũuūuŭuůuűuųuŵwŷyźzżzžz\
    ơoưuǎaǐiǒoǔuǖuǘuǚuǜuǟaǡaǧgǩkǫoǭoǰjǵgǹnǻa\
    ȁaȃaȅeȇeȉiȋiȍoȏoȑrȓrȕuȗușsțtȟhȧaȩeȫoȭoȯo\
    ȱoȳyḁaḃbḅbḇbḉcḋdḍdḏdḑdḓdḕeḗeḙeḛeḝeḟfḡgḣh\
    ḥhḧhḩhḫhḭiḯiḱkḳkḵkḷlḹlḻlḽlḿmṁmṃmṅnṇnṉnṋn\
    ṍoṏoṑoṓoṕpṗpṙrṛrṝrṟrṡsṣsṥsṧsṩsṫtṭtṯtṱtṳu\
    ṵuṷuṹuṻuṽvṿvẁwẃwẅwẇwẉwẋxẍxẏyẑzẓzẕzẖhẗtẘw\
    ẙyạaảaấaầaẩaẫaậaắaằaẳaẵaặaẹeẻeẽeếeềeểeễe\
    ệeỉiịiọoỏoốoồoổoỗoộoớoờoởoỡoợoụuủuứuừuửu\
    ữuựuỳyỵyỷyỹy\
";

fn latin() -> &'static HashMap<char, char> {
    static TABLE: OnceLock<HashMap<char, char>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let chars: Vec<char> = LATIN.chars().collect();
        chars
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[c, b]| (*c, *b))
            .collect()
    })
}

/// Combining marks that unicode61 drops with `remove_diacritics`.
fn combining(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{FE20}'..='\u{FE2F}')
}
fn base(c: char) -> char {
    if c.is_ascii() {
        c
    } else {
        latin().get(&c).copied().unwrap_or(c)
    }
}

/// Folded text with, per folded character, `(folded byte start, original byte start,
/// original byte end)`.
pub(crate) struct Folded {
    pub text: String,
    spans: Vec<(usize, usize, usize)>,
}
impl Folded {
    /// Original byte range of the folded byte range `start..end` (character boundaries).
    fn original(&self, start: usize, end: usize) -> (usize, usize) {
        let first = self.spans.partition_point(|s| s.0 < start);
        let last = self.spans.partition_point(|s| s.0 < end) - 1;
        (self.spans[first].1, self.spans[last].2)
    }
    /// Original byte ranges of every (non-overlapping) occurrence of each folded needle,
    /// at most `max` in total.
    pub fn find_all(&self, needles: &[String], max: usize) -> Vec<(usize, usize)> {
        let mut found = Vec::new();
        for needle in needles.iter().filter(|n| !n.is_empty()) {
            let mut from = 0;
            while let Some(at) = self.text[from..].find(needle.as_str()) {
                let start = from + at;
                found.push(self.original(start, start + needle.len()));
                from = start + needle.len();
                if found.len() >= max {
                    return found;
                }
            }
        }
        found
    }
}

pub(crate) fn fold(text: &str) -> Folded {
    let mut out = Folded {
        text: String::with_capacity(text.len()),
        spans: Vec::with_capacity(text.len()),
    };
    let mut in_space = false;
    for (at, c) in text.char_indices() {
        let end = at + c.len_utf8();
        if c.is_whitespace() || c.is_control() {
            if in_space {
                if let Some(last) = out.spans.last_mut() {
                    last.2 = end;
                }
            } else {
                out.spans.push((out.text.len(), at, end));
                out.text.push(' ');
                in_space = true;
            }
            continue;
        }
        if combining(c) {
            // Dropped, but a highlight of the preceding letter covers it.
            if let Some(last) = out.spans.last_mut() {
                last.2 = end;
            }
            continue;
        }
        in_space = false;
        for lower in c.to_lowercase().filter(|l| !combining(*l)) {
            out.spans.push((out.text.len(), at, end));
            out.text.push(base(lower));
        }
    }
    out
}

/// A query term/phrase folded the same way; surrounding whitespace is not part of a match.
pub(crate) fn fold_needle(needle: &str) -> String {
    fold(needle.trim()).text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_diacritics_and_whitespace_runs_with_original_offsets() {
        let text = "Solid\r\n  ELECTROLYTE by Mu\u{0308}ller and Müller";
        let folded = fold(text);
        assert_eq!(folded.text, "solid electrolyte by muller and muller");
        let found = folded.find_all(&[fold_needle("solid electrolyte")], 10);
        assert_eq!(found, vec![(0, 20)]);
        assert_eq!(&text[0..20], "Solid\r\n  ELECTROLYTE");
        let names: Vec<&str> = folded
            .find_all(&[fold_needle("MULLER")], 10)
            .into_iter()
            .map(|(s, e)| &text[s..e])
            .collect();
        assert_eq!(names, vec!["Mu\u{0308}ller", "Müller"]);
        assert_eq!(fold_needle(" 電池 "), "電池");
    }
}
