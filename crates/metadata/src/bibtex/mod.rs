//! Bounded BibTeX data reader/writer. Never runs TeX or expands macros/crossrefs.
use crate::exchange::{self, *};
use cm_core::{
    ServiceError,
    export::{BibliographySerializer, EncodedRecord},
    import::{PaperDraft, normalize::invalid},
};
use cm_domain::Paper;
use serde_json::json;

struct FieldValue {
    text: String,
    // Exact source expression for identifiers/concatenations; never evaluated.
    expression: Option<String>,
}
type Entry = (String, String, Vec<(String, FieldValue)>);

struct Reader<'a> {
    text: &'a str,
    pos: usize,
}
impl<'a> Reader<'a> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }
    fn skip(&mut self) {
        loop {
            while self.byte().is_some_and(|v| v.is_ascii_whitespace()) {
                self.pos += 1;
            }
            if self.byte() == Some(b'%') {
                while self.byte().is_some_and(|v| v != b'\n') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }
    fn ident(&mut self) -> Result<String, &'static str> {
        self.skip();
        let start = self.pos;
        while self
            .byte()
            .is_some_and(|v| v.is_ascii_alphanumeric() || b"_-:".contains(&v))
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("Expected a field/type identifier");
        }
        if self.pos - start > 128 {
            return Err("Identifier byte budget exceeded");
        }
        Ok(self.text[start..self.pos].to_ascii_lowercase())
    }
    fn take(&mut self, b: u8) -> Result<(), &'static str> {
        self.skip();
        if self.byte() != Some(b) {
            return Err("Unexpected delimiter or missing closing delimiter");
        }
        self.pos += 1;
        Ok(())
    }
    fn value(&mut self) -> Result<FieldValue, &'static str> {
        self.skip();
        let start = self.pos;
        let (text, mut unresolved) = self.atom()?;
        let mut end = self.pos;
        self.skip();
        while self.byte() == Some(b'#') {
            unresolved = true;
            self.pos += 1;
            self.atom()?;
            end = self.pos;
            self.skip();
            if self.pos - start > MAX_RECORD_BYTES {
                return Err("Record expression byte budget exceeded");
            }
        }
        Ok(FieldValue {
            text,
            expression: unresolved.then(|| self.text[start..end].to_owned()),
        })
    }
    fn atom(&mut self) -> Result<(String, bool), &'static str> {
        self.skip();
        let opener = self.byte().ok_or("Missing field value")?;
        if !b"{\"".contains(&opener) {
            let start = self.pos;
            while self
                .byte()
                .is_some_and(|v| v.is_ascii_alphanumeric() || b"_-:.".contains(&v))
            {
                self.pos += 1;
                if self.pos - start > 128 {
                    return Err("Value identifier byte budget exceeded");
                }
            }
            if self.pos == start {
                return Err("Expected braced/quoted text, number or raw identifier");
            }
            let raw = &self.text[start..self.pos];
            return Ok((raw.into(), !raw.bytes().all(|v| v.is_ascii_digit())));
        }
        self.pos += 1;
        let start = self.pos;
        let mut level = 0usize;
        while let Some(b) = self.byte() {
            if self.pos - start > MAX_RECORD_BYTES {
                return Err("Record value byte budget exceeded");
            }
            if b == b'\\' {
                self.pos += 1;
                if self.byte().is_some() {
                    self.pos += self.text[self.pos..].chars().next().unwrap().len_utf8();
                }
                continue;
            }
            if b == b'\n'
                && self.text[self.pos + 1..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with('@')
            {
                return Err(
                    "Ambiguous entry boundary in unclosed/multiline value; record requires review",
                );
            }
            if b == b'}' && level == 0 && opener == b'{'
                || b == b'"' && level == 0 && opener == b'"'
            {
                let value = self.text[start..self.pos].to_owned();
                self.pos += 1;
                return Ok((value, false));
            }
            if b == b'{' {
                level += 1;
                if level > MAX_DEPTH {
                    return Err("Brace nesting budget exceeded");
                }
            }
            if b == b'}' {
                level = level.checked_sub(1).ok_or("Unmatched value brace")?;
            }
            self.pos += 1;
        }
        Err("Unclosed field value")
    }
    fn entry(&mut self) -> Result<Entry, &'static str> {
        let record_start = self.pos;
        self.take(b'@')?;
        let kind = self.ident()?;
        if ["string", "preamble", "comment"].contains(&kind.as_str()) {
            return Err("BibTeX directive ignored; no definitions executed or expanded");
        }
        self.skip();
        let close = match self.byte() {
            Some(b'{') => b'}',
            Some(b'(') => b')',
            _ => return Err("Missing entry delimiter"),
        };
        self.pos += 1;
        let start = self.pos;
        while self
            .byte()
            .is_some_and(|v| v != b',' && v != close && !v.is_ascii_whitespace())
        {
            self.pos += 1;
            if self.pos - start > 256 {
                return Err("Citation key byte budget exceeded");
            }
        }
        let key = self.text[start..self.pos].to_owned();
        self.take(b',')?;
        let mut fields = vec![];
        loop {
            self.skip();
            if self.byte() == Some(close) {
                self.pos += 1;
                break;
            }
            if self.pos - record_start > MAX_RECORD_BYTES || fields.len() >= 256 {
                return Err("Record byte/field budget exceeded");
            }
            let field = self.ident()?;
            self.take(b'=')?;
            let value = self.value()?;
            fields.push((field, value));
            self.skip();
            if self.byte() == Some(b',') {
                self.pos += 1;
            } else if self.byte() != Some(close) {
                return Err("Expected field comma or closing delimiter");
            }
        }
        if self.pos - record_start > MAX_RECORD_BYTES {
            return Err("Record byte budget exceeded");
        }
        Ok((kind, key, fields))
    }
}
fn has_group(s: &str) -> bool {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == '{' && !s[i..].starts_with("{}") && !s[i..].starts_with("{@}") {
            return true;
        }
    }
    false
}
fn validate_verbatim(s: &str) -> Result<(), ServiceError> {
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '\\' | '\n' | '\r' => {
                return Err(invalid(
                    "BibTeX verbatim DOI/URL cannot represent backslashes or newlines safely",
                ));
            }
            '{' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(invalid("BibTeX verbatim brace depth exceeded"));
                }
            }
            '}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("Unmatched BibTeX verbatim brace"))?;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(invalid("Unmatched BibTeX verbatim brace"));
    }
    Ok(())
}
// Finite character conversion only: no command definitions, expansion or TeX engine.
// Precomposed common Latin pairs plus a fixed Unicode combining mark for other ASCII letters.
fn accent(command: &str, base: char) -> Option<String> {
    let (bases, composed, mark) = match command {
        "\"" => ("aAeEiIoOuUyY", "äÄëËïÏöÖüÜÿŸ", '\u{308}'),
        "'" => (
            "aAcCeEiIlLnNoOrRsSuUyYzZ",
            "áÁćĆéÉíÍĺĹńŃóÓŕŔśŚúÚýÝźŹ",
            '\u{301}',
        ),
        "`" => ("aAeEiIoOuU", "àÀèÈìÌòÒùÙ", '\u{300}'),
        "^" => (
            "aAcCeEgGhHiIjJoOsSuUwWyY",
            "âÂĉĈêÊĝĜĥĤîÎĵĴôÔŝŜûÛŵŴŷŶ",
            '\u{302}',
        ),
        "~" => ("aAiInNoOuU", "ãÃĩĨñÑõÕũŨ", '\u{303}'),
        "c" => ("cCgGkKlLnNrRsStT", "çÇģĢķĶļĻņŅŗŖşŞţŢ", '\u{327}'),
        "v" => ("cCdDeElLnNrRsStTzZ", "čČďĎěĚľĽňŇřŘšŠťŤžŽ", '\u{30c}'),
        "u" => ("aAeEgGiIoOuU", "ăĂĕĔğĞĭĬŏŎŭŬ", '\u{306}'),
        "H" => ("oOuU", "őŐűŰ", '\u{30b}'),
        "k" => ("aAeEiIoOuU", "ąĄęĘįĮǫǪųŲ", '\u{328}'),
        "=" => ("aAeEiIoOuU", "āĀēĒīĪōŌūŪ", '\u{304}'),
        "." => ("cCeEgGIiZz", "ċĊėĖġĠİiŻż", '\u{307}'),
        _ => return None,
    };
    if let Some(i) = bases.chars().position(|c| c == base) {
        return Some(composed.chars().nth(i)?.to_string());
    }
    base.is_ascii_alphabetic().then(|| format!("{base}{mark}"))
}
fn tex_character(rest: &str) -> Option<(String, usize)> {
    let tail = rest.strip_prefix('\\')?;
    let n = tail.bytes().take_while(u8::is_ascii_alphabetic).count();
    let n = if n == 0 {
        tail.chars().next()?.len_utf8()
    } else {
        n
    };
    let command = &tail[..n];
    let remaining = &tail[n..];
    let character = match command {
        "ss" => Some('ß'),
        "o" => Some('ø'),
        "O" => Some('Ø'),
        "aa" => Some('å'),
        "AA" => Some('Å'),
        "l" => Some('ł'),
        "L" => Some('Ł'),
        "ae" => Some('æ'),
        "AE" => Some('Æ'),
        "oe" => Some('œ'),
        "OE" => Some('Œ'),
        "i" => Some('ı'),
        "j" => Some('ȷ'),
        _ => None,
    };
    if let Some(c) = character {
        let consumed = if remaining.starts_with("{}") {
            2
        } else {
            remaining.len() - remaining.trim_start_matches(char::is_whitespace).len()
        };
        return Some((c.to_string(), 1 + n + consumed));
    }
    let argument = remaining.trim_start_matches(char::is_whitespace);
    let whitespace = remaining.len() - argument.len();
    let (base, consumed) = if let Some(group) = argument.strip_prefix('{') {
        let end = group.find('}')?;
        let raw = &group[..end];
        let base = if raw == "\\i" {
            'i'
        } else if raw == "\\j" {
            'j'
        } else {
            let mut chars = raw.chars();
            let base = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            base
        };
        (base, end + 2)
    } else if argument.starts_with("\\i") || argument.starts_with("\\j") {
        // A following letter would belong to the command name, not its argument.
        if argument
            .as_bytes()
            .get(2)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            return None;
        }
        (
            if argument.starts_with("\\i") {
                'i'
            } else {
                'j'
            },
            2,
        )
    } else {
        let base = argument.chars().next()?;
        (base, base.len_utf8())
    };
    Some((accent(command, base)?, 1 + n + whitespace + consumed))
}
/// Remove protection groups and decode escaped punctuation. Unknown TeX commands
/// stay verbatim (with a warning); only the finite character table is converted.
fn decode(text: &str) -> (String, bool) {
    let mut out = String::new();
    let mut rest = text;
    let mut unsupported = false;
    while !rest.is_empty() {
        if let Some((character, consumed)) = tex_character(rest) {
            out.push_str(&character);
            rest = &rest[consumed..];
            continue;
        }
        if let Some(tail) = rest.strip_prefix("\\textbackslash{}") {
            out.push('\\');
            rest = tail;
            continue;
        }
        if let Some(tail) = rest.strip_prefix("\\textasciicircum{}") {
            out.push('^');
            rest = tail;
            continue;
        }
        if let Some(tail) = rest.strip_prefix("\\textasciitilde{}") {
            out.push('~');
            rest = tail;
            continue;
        }
        let c = rest.chars().next().unwrap();
        rest = &rest[c.len_utf8()..];
        if c == '\\' {
            if let Some(next) = rest.chars().next()
                && "{}%&#_$\\".contains(next)
            {
                out.push(next);
                rest = &rest[next.len_utf8()..];
                continue;
            }
            // Retain unknown command and its immediate argument literally.
            unsupported = true;
            out.push(c);
            let n = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            if n > 0 {
                out.push_str(&rest[..n]);
                rest = &rest[n..];
            } else if let Some(symbol) = rest.chars().next() {
                out.push(symbol);
                rest = &rest[symbol.len_utf8()..];
            }
            if rest.starts_with('{') {
                let mut depth = 0;
                let mut end = 0;
                let mut escaped = false;
                for (i, ch) in rest.char_indices() {
                    end = i + ch.len_utf8();
                    if escaped {
                        escaped = false;
                        continue;
                    }
                    if ch == '\\' {
                        escaped = true;
                        continue;
                    }
                    if ch == '{' {
                        depth += 1;
                    }
                    if ch == '}' {
                        depth -= 1;
                    }
                    if depth == 0 {
                        break;
                    }
                }
                out.push_str(&rest[..end]);
                rest = &rest[end..];
            }
        } else if !['{', '}'].contains(&c) {
            out.push(c);
        }
    }
    (out, unsupported)
}
fn authors(raw: &str, role: &str, d: &mut PaperDraft) {
    let mut depth = 0usize;
    let mut escaped = false;
    let mut start = 0;
    let mut parts = vec![];
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if b == b'\\' {
            escaped = true;
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth = depth.saturating_sub(1);
        }
        if depth == 0
            && i > 0
            && bytes[i - 1].is_ascii_whitespace()
            && bytes
                .get(i..i + 3)
                .is_some_and(|v| v.eq_ignore_ascii_case(b"and"))
            && bytes.get(i + 3).is_some_and(u8::is_ascii_whitespace)
        {
            parts.push(&raw[start..i]);
            i += 3;
            start = i;
            continue;
        }
        i += 1;
    }
    parts.push(&raw[start..]);
    for p in parts {
        let p = p.trim();
        let literal = outer_group(p);
        let raw = if literal { &p[1..p.len() - 1] } else { p };
        let (text, unknown) = decode(raw);
        if unknown {
            warn(
                d,
                "author",
                "Unsupported TeX name command retained literally; no expansion",
                false,
            );
        }
        if !literal && text.matches(',').count() > 1 {
            warn(
                d,
                "author",
                "Suffix name form retained as a literal name; review required",
                false,
            );
        }
        d.bibliography.authors.push(person(
            &text,
            role,
            literal || text.matches(',').count() > 1,
        ));
    }
}
fn draft(
    kind: String,
    key: String,
    fields: Vec<(String, FieldValue)>,
) -> Result<PaperDraft, ServiceError> {
    let mut d = PaperDraft::default();
    d.bibliography.paper_type = Some(kind.clone());
    if !key.is_empty() {
        d.bibliography.citation_key = Some(key);
    }
    if ![
        "article",
        "book",
        "booklet",
        "inbook",
        "incollection",
        "inproceedings",
        "conference",
        "manual",
        "mastersthesis",
        "phdthesis",
        "proceedings",
        "techreport",
        "unpublished",
        "misc",
    ]
    .contains(&kind.as_str())
    {
        warn(&mut d, "type", "Unknown BibTeX type retained", false);
    }
    let mut unknown = vec![];
    let mut extension = None;
    let mut used = std::collections::HashSet::new();
    for (field, parsed) in fields {
        let raw = parsed.text;
        if !used.insert(
            match field.as_str() {
                "journal" | "journaltitle" => "journal",
                "number" | "issue" => "number",
                _ => &field,
            }
            .to_owned(),
        ) {
            warn(
                &mut d,
                "fields",
                "Repeated BibTeX field: record requires review (no silent overwrite)",
                true,
            );
        }
        if let Some(expression) = parsed.expression {
            let standard_month =
                field == "month" && standard_month_identifier(&expression).is_some();
            warn(
                &mut d,
                &field,
                if standard_month {
                    "Standard month identifier retained as raw syntax; no expansion"
                } else {
                    "Unresolved identifier/concatenation retained as raw syntax; review required, no evaluation"
                },
                !standard_month,
            );
            unknown.push((format!("expression:{field}"), expression));
            continue;
        }
        if field == "author" || field == "editor" {
            authors(&raw, &field, &mut d);
            if raw.contains('\\') {
                unknown.push((format!("raw:{field}"), raw));
            }
            continue;
        }
        let verbatim = field == "doi" || field == "url";
        let (value, tex) = if verbatim {
            (raw.clone(), false)
        } else {
            decode(&raw)
        };
        if !verbatim && has_group(&raw) {
            warn(
                &mut d,
                &field,
                "BibTeX protection groups decoded: public re-export loses case protection; raw field retained in extra_json",
                false,
            );
            unknown.push((format!("protection:{field}"), raw.clone()));
        }
        if tex {
            warn(
                &mut d,
                "tex",
                "Unsupported TeX commands retained literally; raw field preserved, no execution/expansion",
                false,
            );
            unknown.push((format!("raw:{field}"), raw.clone()));
        }
        let b = &mut d.bibliography;
        match field.as_str() {
            "title" => d.title = value,
            "journal" | "journaltitle" => b.journal = Some(value),
            "volume" => b.volume = Some(value),
            "number" | "issue" => b.issue = Some(value),
            "pages" => b.pages = Some(value),
            "doi" => b.doi = Some(value),
            "url" => b.url = Some(value),
            "abstract" => b.abstract_text = Some(value),
            "language" => b.language = Some(value),
            "date" => b.publication_date = Some(value),
            "year" => match value.parse() {
                Ok(v) => b.year = Some(v),
                Err(_) => {
                    unknown.push((field, raw));
                    warn(
                        &mut d,
                        "year",
                        "Invalid year retained in extra fields; row requires review",
                        true,
                    );
                }
            },
            "keywords" => extra(&mut d, "keywords", json!(value)),
            "urldate" => extra(&mut d, "accessed", json!(value)),
            "cm_data" => extension = Some(value),
            _ => {
                if field == "crossref" || field == "xdata" {
                    warn(
                        &mut d,
                        "crossref",
                        "Cross-reference inheritance is not expanded; field retained",
                        false,
                    );
                }
                unknown.push((field, raw));
            }
        }
    }
    if let (Some(year), Some(date)) = (d.bibliography.year, &d.bibliography.publication_date)
        && date
            .split(['-', '/'])
            .next()
            .and_then(|s| s.parse::<i32>().ok())
            .is_some_and(|y| y != year)
    {
        warn(
            &mut d,
            "year",
            "Conflicting BibTeX year/date; review required",
            true,
        );
    }
    if !unknown.is_empty() {
        extra(&mut d, "bibtex", json!(unknown));
        warn(
            &mut d,
            "extra_json",
            "Unsupported BibTeX fields retained in extra_json; other tools may ignore them",
            false,
        );
    }
    if let Some(e) = extension
        && let Err(error) = apply_extension(&mut d, &e)
    {
        if error.to_string().contains("budget") {
            return Err(error);
        }
        warn(
            &mut d,
            "cm-v1",
            "Invalid/unsupported cm-v1 extension; row requires review",
            true,
        );
    }
    Ok(d)
}
pub fn parse(input: &[u8]) -> Result<ParseReport, ServiceError> {
    let text = utf8(input)?;
    let mut report = ParseReport::new(input);
    let mut r = Reader { text, pos: 0 };
    let mut blocks = 0;
    let mut line = 1u64;
    let mut counted = 0;
    let mut ambiguous_line = None;
    while r.pos < text.len() {
        r.skip();
        if r.pos == text.len() {
            break;
        }
        let start = r.pos;
        line += text[counted..start].bytes().filter(|v| *v == b'\n').count() as u64;
        counted = start;
        blocks += 1;
        if blocks > exchange::MAX_RECORDS {
            return Err(invalid("BibTeX block/diagnostic budget exceeded"));
        }
        match r.entry() {
            Ok((kind, key, fields)) => match draft(kind, key, fields) {
                Ok(mut d) => {
                    if let Some(origin) = ambiguous_line {
                        warn(
                            &mut d,
                            "syntax",
                            &format!(
                                "Recovered after ambiguous boundary at line {origin}; edit source and reparse before saving"
                            ),
                            true,
                        );
                    }
                    report.push(d, "bibtex", line)?;
                }
                Err(e) => report.diagnostic(line, format!("Record rejected: {e}"), true),
            },
            Err(e) => {
                if e.starts_with("Ambiguous entry boundary") && ambiguous_line.is_none() {
                    ambiguous_line = Some(line);
                }
                report.diagnostic(line, e, true);
                // Recover only at an explicit physical-line entry start, never by
                // guessing an arbitrary @ inside an email/URL or broken value.
                let next = text[start..].find('\n').map(|n| start + n + 1);
                r.pos = next
                    .and_then(|mut p| {
                        while p < text.len() {
                            let s = &text[p..];
                            let trim = s.trim_start_matches([' ', '\t']);
                            if trim.starts_with('@') {
                                return Some(p + s.len() - trim.len());
                            }
                            p += s.find('\n').map_or(s.len(), |n| n + 1);
                        }
                        None
                    })
                    .unwrap_or(text.len());
            }
        }
    }
    Ok(report)
}
fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            '~' => out.push_str("\\textasciitilde{}"),
            '@' => out.push_str("{@}"),
            '{' | '}' | '%' | '&' | '#' | '_' | '$' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-:".contains(&b))
}
pub struct BibtexSerializer;
impl BibliographySerializer for BibtexSerializer {
    fn encode(&self, p: &Paper) -> Result<EncodedRecord, ServiceError> {
        check_paper(p)?;
        let b = &p.bibliography;
        let kind = b.paper_type.as_deref().unwrap_or("misc");
        let kind = if identifier(kind) { kind } else { "misc" };
        let key = b.citation_key.as_deref().unwrap_or("");
        if key
            .chars()
            .any(|c| c.is_whitespace() || ",{}()=\"%\\".contains(c))
        {
            return Err(invalid(
                "citation key is not representable safely in BibTeX",
            ));
        }
        for v in [&b.doi, &b.url].into_iter().flatten() {
            validate_verbatim(v)?;
        }
        let mut out = format!("@{kind}{{{key},\n");
        let mut field = |k: &str, v: &str| {
            let value = if k == "doi" || k == "url" {
                v.to_owned()
            } else {
                escape(v)
            };
            out.push_str(&format!("  {k} = {{{value}}},\n"));
        };
        field("title", &p.title);
        for (k, v) in [
            ("journal", &b.journal),
            ("volume", &b.volume),
            ("number", &b.issue),
            ("pages", &b.pages),
            ("doi", &b.doi),
            ("url", &b.url),
            ("abstract", &b.abstract_text),
            ("language", &b.language),
            ("date", &b.publication_date),
        ] {
            if let Some(v) = v {
                field(k, v);
            }
        }
        if let Some(y) = b.year {
            field("year", &y.to_string());
        }
        for (k, target) in [("keywords", "keywords"), ("accessed", "urldate")] {
            if let Some(v) = b
                .extra_json
                .as_ref()
                .and_then(|e| e.get(k))
                .and_then(|v| v.as_str())
            {
                field(target, v);
            }
        }
        if let Some(fields) = b
            .extra_json
            .as_ref()
            .and_then(|e| e.get("bibtex"))
            .and_then(|v| v.as_array())
        {
            for item in fields {
                if let (Some(k), Some(v)) = (
                    item.get(0).and_then(|v| v.as_str()),
                    item.get(1).and_then(|v| v.as_str()),
                ) && identifier(k)
                    && !k.starts_with("raw:")
                    && !k.starts_with("protection:")
                    && !k.starts_with("expression:")
                    && ![
                        "title",
                        "author",
                        "editor",
                        "journal",
                        "journaltitle",
                        "volume",
                        "number",
                        "issue",
                        "pages",
                        "doi",
                        "url",
                        "abstract",
                        "language",
                        "date",
                        "year",
                        "keywords",
                        "urldate",
                        "cm_data",
                    ]
                    .contains(&k)
                {
                    field(k, &decode(v).0);
                }
            }
        }
        // Only the closed set of standard month tokens is replayed without expansion.
        if let Some(month) = standard_month(p) {
            out.push_str(&format!("  month = {month},\n"));
        }
        // Names need grouping that protects organizations and literal "and".
        for role in ["author", "editor"] {
            let names = b
                .authors
                .iter()
                .filter(|a| a.role == role)
                .map(|a| {
                    let n = escape(&name(a));
                    if a.literal_name.is_some() {
                        format!("{{{n}}}")
                    } else {
                        n
                    }
                })
                .collect::<Vec<_>>()
                .join(" and ");
            if !names.is_empty() {
                out.push_str(&format!("  {role} = {{{names}}},\n"));
            }
        }
        // Hash the actual public representation, before adding the extension.
        let native = parse(format!("{out}}}\n").as_bytes())?;
        let native = native
            .candidates
            .first()
            .ok_or_else(|| invalid("unrepresentable BibTeX record"))?;
        if native.diagnostics.iter().any(|d| d.error) {
            return Err(invalid("unrepresentable BibTeX fields"));
        }
        let ext = extension(p, native)?;
        out.push_str(&format!("  cm_data = {{{ext}}},\n}}\n\n"));
        let mut encoded = finish_record(p, out, "BibTeX")?;
        if let Some(fields) = b
            .extra_json
            .as_ref()
            .and_then(|e| e.get("bibtex"))
            .and_then(|v| v.as_array())
        {
            for field in fields
                .iter()
                .filter_map(|v| v.get(0)?.as_str()?.strip_prefix("protection:"))
            {
                if !encoded.diagnostics.iter().any(|d| d.field == field) {
                    encoded.diagnostics.push(cm_core::export::ExportDiagnostic {
                        paper: Some(p.id), field: field.into(),
                        message: "Public BibTeX field loses original case protection; raw syntax is retained only in cm-v1/extra_json".into(),
                    });
                }
            }
        }
        Ok(encoded)
    }
}
