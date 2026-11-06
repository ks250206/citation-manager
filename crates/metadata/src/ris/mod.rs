//! Bounded UTF-8 RIS records, including folded values and safe TY/ER recovery.
use crate::exchange::{self, *};
use cm_core::{
    ServiceError,
    export::{BibliographySerializer, EncodedRecord},
    import::{PaperDraft, normalize::invalid},
};
use cm_domain::Paper;
use serde_json::json;

fn kind(ty: &str) -> &str {
    match ty {
        "JOUR" => "article",
        "BOOK" => "book",
        "CHAP" => "incollection",
        "CONF" | "CPAPER" => "inproceedings",
        "THES" => "phdthesis",
        "RPRT" => "techreport",
        "UNPB" => "unpublished",
        "ELEC" | "GEN" => "misc",
        _ => ty,
    }
}
fn ty(kind: &str) -> &str {
    match kind {
        "article" => "JOUR",
        "book" => "BOOK",
        "inbook" | "incollection" => "CHAP",
        "inproceedings" | "conference" | "proceedings" => "CONF",
        "mastersthesis" | "phdthesis" => "THES",
        "techreport" => "RPRT",
        "unpublished" => "UNPB",
        _ => "GEN",
    }
}
fn draft(fields: Vec<(String, String)>) -> Result<PaperDraft, ServiceError> {
    let mut d = PaperDraft::default();
    let mut unknown = vec![];
    let mut keywords = vec![];
    let mut ext = None;
    let mut endpage = None;
    let mut singles = std::collections::HashSet::new();
    for (tag, value) in fields {
        if [
            "TY", "TI", "T1", "JO", "JF", "JA", "T2", "VL", "IS", "SP", "EP", "DO", "UR", "AB",
            "N2", "LA", "ID", "PY", "Y1", "DA", "Y2", "C9",
        ]
        .contains(&tag.as_str())
            && !singles.insert(
                match tag.as_str() {
                    "TI" | "T1" => "TI",
                    "JO" | "JF" | "JA" | "T2" => "JO",
                    "AB" | "N2" => "AB",
                    _ => &tag,
                }
                .to_owned(),
            )
        {
            warn(
                &mut d,
                "fields",
                "Repeated singleton RIS tag; row requires review",
                true,
            );
            unknown.push((tag.clone(), value.clone()));
        }
        let b = &mut d.bibliography;
        match tag.as_str() {
            "TY" => {
                b.paper_type = Some(kind(&value).into());
                if kind(&value) == value
                    && ![
                        "JOUR", "BOOK", "CHAP", "CONF", "CPAPER", "THES", "RPRT", "UNPB", "ELEC",
                        "GEN",
                    ]
                    .contains(&value.as_str())
                {
                    warn(&mut d, "type", "Unknown RIS type retained", false);
                }
            }
            "TI" | "T1" => {
                if !d.title.is_empty() {
                    warn(
                        &mut d,
                        "title",
                        "Conflicting RIS title aliases; row requires review",
                        true,
                    );
                }
                d.title = value
            }
            "AU" | "A1" | "A2" | "ED" => {
                let role = if tag == "AU" || tag == "A1" {
                    "author"
                } else {
                    "editor"
                };
                let literal = outer_group(&value);
                let name = if literal {
                    value[1..value.len() - 1].to_owned()
                } else {
                    value
                        .split(',')
                        .map(|part| {
                            let part = part.trim();
                            if outer_group(part) {
                                &part[1..part.len() - 1]
                            } else {
                                part
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                b.authors.push(person(&name, role, literal));
            }
            "JO" | "JF" | "JA" | "T2" => {
                if b.journal.as_ref().is_some_and(|v| v != &value) {
                    unknown.push((tag, value));
                    warn(
                        &mut d,
                        "journal",
                        "Conflicting container aliases retained; review required",
                        true,
                    );
                } else {
                    b.journal = Some(value);
                }
            }
            "VL" => b.volume = Some(value),
            "IS" => b.issue = Some(value),
            "SP" => b.pages = Some(value),
            "EP" => endpage = Some(value),
            "DO" => b.doi = Some(value),
            "UR" => b.url = Some(value),
            "AB" | "N2" => b.abstract_text = Some(value),
            "LA" => b.language = Some(value),
            "ID" => b.citation_key = Some(value),
            "PY" | "Y1" | "DA" => {
                let date = value.trim_end_matches('/').to_owned();
                let year = date
                    .split(['/', '-'])
                    .next()
                    .and_then(|v| v.parse::<i32>().ok());
                if year.is_none() || b.year.is_some_and(|y| Some(y) != year) {
                    unknown.push((tag, value));
                    warn(
                        &mut d,
                        "year",
                        "Invalid/conflicting RIS date retained; review required",
                        true,
                    );
                } else {
                    b.year = year;
                    if date.contains(['/', '-']) {
                        b.publication_date = Some(date);
                    }
                }
            }
            "KW" => keywords.push(value),
            "Y2" => extra(&mut d, "accessed", json!(value)),
            "C9" if value.starts_with("cm-v1:") => ext = Some(value[6..].to_owned()),
            "ER" => {}
            _ => unknown.push((tag, value)),
        }
    }
    if let Some(ep) = endpage {
        if let Some(start) = &mut d.bibliography.pages {
            start.push('-');
            start.push_str(&ep);
        } else {
            unknown.push(("EP".into(), ep));
            warn(
                &mut d,
                "pages",
                "End page without start page retained",
                false,
            );
        }
    }
    if !keywords.is_empty() {
        extra(&mut d, "keywords", json!(keywords));
    }
    if !unknown.is_empty() {
        extra(&mut d, "ris", json!(unknown));
        warn(
            &mut d,
            "extra_json",
            "Unsupported RIS tags retained in extra_json; other tools may ignore them",
            false,
        );
    }
    if let Some(e) = ext
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
fn tag(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    if bytes.len() >= 6
        && bytes[0..2].iter().all(u8::is_ascii_alphanumeric)
        && &bytes[2..6] == b"  - "
    {
        Some((&line[..2], &line[6..]))
    } else if bytes.len() == 5
        && &bytes[2..] == b"  -"
        && bytes[0..2].iter().all(u8::is_ascii_alphanumeric)
    {
        Some((&line[..2], ""))
    } else {
        None
    }
}
pub fn parse(input: &[u8]) -> Result<ParseReport, ServiceError> {
    let text = utf8(input)?;
    let mut report = ParseReport::new(input);
    let mut fields: Vec<(String, String)> = vec![];
    let mut start = 0;
    let mut bytes = 0;
    let mut damaged = false;
    let mut discarding = false;
    let mut blocks = 0;
    for (index, physical) in text.split_inclusive('\n').enumerate() {
        if report.diagnostics.len() > exchange::MAX_RECORDS {
            return Err(invalid("RIS diagnostic budget exceeded"));
        }
        let line = physical.strip_suffix('\n').unwrap_or(physical);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let row = index as u64 + 1;
        let tagged = tag(line);
        if tagged.is_some_and(|(key, _)| key == "TY") {
            blocks += 1;
            if blocks > exchange::MAX_RECORDS {
                return Err(invalid("RIS record budget exceeded"));
            }
            if !fields.is_empty() {
                report.diagnostic(
                    start,
                    "Missing ER: incomplete record rejected before next TY",
                    true,
                );
            }
            fields.clear();
            start = row;
            bytes = 0;
            damaged = false;
            discarding = false;
        } else if discarding {
            // No retained fragments or per-line allocation until the next physical TY.
            continue;
        } else if fields.is_empty() {
            if !line.trim().is_empty() {
                report.diagnostic(row, "RIS content outside TY/ER record ignored", true);
            }
            continue;
        }
        bytes += physical.len();
        if bytes > MAX_RECORD_BYTES || (tagged.is_some() && fields.len() >= 256) {
            report.diagnostic(
                start,
                format!("RIS record byte/field budget exceeded at line {row}; record rejected"),
                true,
            );
            fields.clear();
            discarding = true;
            continue;
        }
        if let Some((key, value)) = tagged {
            fields.push((key.into(), value.into()));
            if key == "ER" {
                let mut d = match draft(std::mem::take(&mut fields)) {
                    Ok(d) => d,
                    Err(e) => {
                        report.diagnostic(start, format!("Record rejected: {e}"), true);
                        continue;
                    }
                };
                if damaged {
                    warn(
                        &mut d,
                        "syntax",
                        "Malformed RIS line: row requires review; valid neighboring records recovered",
                        true,
                    );
                }
                report.push(d, "ris", start)?;
            }
        } else if line.starts_with([' ', '\t']) {
            let last = fields.last_mut().unwrap();
            last.1.push('\n');
            last.1.push_str(
                line.strip_prefix("  ")
                    .unwrap_or_else(|| line.trim_start_matches([' ', '\t'])),
            );
        } else if !line.trim().is_empty() {
            report.diagnostic(
                row,
                "Malformed RIS line; only indented continuations accepted",
                true,
            );
            damaged = true;
        }
    }
    if !fields.is_empty() {
        report.diagnostic(start, "Missing ER at EOF: incomplete record rejected", true);
    }
    if report.diagnostics.len() > exchange::MAX_RECORDS {
        return Err(invalid("RIS diagnostic budget exceeded"));
    }
    Ok(report)
}

fn has_cr(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains('\r'),
        serde_json::Value::Array(a) => a.iter().any(has_cr),
        serde_json::Value::Object(o) => o.values().any(has_cr),
        _ => false,
    }
}
pub struct RisSerializer;
impl BibliographySerializer for RisSerializer {
    fn encode(&self, p: &Paper) -> Result<EncodedRecord, ServiceError> {
        check_paper(p)?;
        // RIS uses physical CRLF/LF as framing. Standalone CR cannot be retained
        // exactly in public fields, so fail explicitly instead of replacing it.
        if has_cr(&serde_json::to_value(p).map_err(|_| invalid("invalid paper"))?) {
            return Err(invalid(
                "RIS cannot export carriage returns losslessly; edit metadata explicitly",
            ));
        }
        let b = &p.bibliography;
        let mut out = String::new();
        let mut field = |key: &str, value: &str| {
            out.push_str(key);
            out.push_str("  - ");
            out.push_str(&value.replace('\r', " ").replace('\n', "\n  "));
            out.push('\n');
        };
        field("TY", ty(b.paper_type.as_deref().unwrap_or("misc")));
        field("TI", &p.title);
        for a in &b.authors {
            if a.role == "author" || a.role == "editor" {
                let n = name(a);
                field(
                    if a.role == "author" { "AU" } else { "ED" },
                    &if a.literal_name.is_some() {
                        format!("{{{n}}}")
                    } else {
                        n
                    },
                );
            }
        }
        for (key, value) in [
            ("JO", &b.journal),
            ("VL", &b.volume),
            ("IS", &b.issue),
            ("SP", &b.pages),
            ("DO", &b.doi),
            ("UR", &b.url),
            ("AB", &b.abstract_text),
            ("LA", &b.language),
            ("ID", &b.citation_key),
        ] {
            if let Some(v) = value {
                field(key, v);
            }
        }
        if let Some(y) = b.year {
            field("PY", &y.to_string());
        }
        if let Some(v) = &b.publication_date {
            field("DA", v);
        }
        if let Some(extra) = &b.extra_json {
            if let Some(v) = extra.get("keywords") {
                if let Some(s) = v.as_str() {
                    field("KW", s);
                } else if let Some(a) = v.as_array() {
                    for s in a.iter().filter_map(|v| v.as_str()) {
                        field("KW", s);
                    }
                }
            }
            if let Some(s) = extra.get("accessed").and_then(|v| v.as_str()) {
                field("Y2", s);
            }
        }
        if let Some(fields) = b
            .extra_json
            .as_ref()
            .and_then(|e| e.get("ris"))
            .and_then(|v| v.as_array())
        {
            for item in fields {
                if let (Some(k), Some(v)) = (
                    item.get(0).and_then(|v| v.as_str()),
                    item.get(1).and_then(|v| v.as_str()),
                ) && k.len() == 2
                    && k.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                    && ![
                        "TY", "TI", "T1", "AU", "A1", "A2", "ED", "JO", "JF", "JA", "T2", "VL",
                        "IS", "SP", "EP", "DO", "UR", "AB", "N2", "LA", "ID", "PY", "Y1", "DA",
                        "KW", "Y2", "C9", "ER",
                    ]
                    .contains(&k)
                {
                    field(k, v);
                }
            }
        }
        field("ER", "");
        let native = parse(out.as_bytes())?;
        let native = native
            .candidates
            .first()
            .ok_or_else(|| invalid("unrepresentable RIS record"))?;
        if native.diagnostics.iter().any(|d| d.error) {
            return Err(invalid("unrepresentable RIS fields"));
        }
        let ext = extension(p, native)?;
        out.truncate(out.len() - "ER  - \n".len());
        out.push_str(&format!("C9  - cm-v1:{ext}\nER  - \n\n"));
        let mut encoded = finish_record(p, out, "RIS")?;
        if let Some(month) = standard_month(p) {
            encoded.diagnostics.push(cm_core::export::ExportDiagnostic {
                paper: Some(p.id), field: "month".into(),
                message: format!("Public RIS cannot retain BibTeX month identifier {month}; exact syntax requires cm-v1"),
            });
        }
        let original = b.paper_type.as_deref().unwrap_or("misc");
        let public = kind(ty(original));
        if original != public {
            encoded.diagnostics.push(cm_core::export::ExportDiagnostic {
                paper: Some(p.id),
                field: "paper_type".into(),
                message: format!(
                    "Public RIS type {} maps {original} to {public}; exact type requires cm-v1",
                    ty(original)
                ),
            });
        }
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
                        message: "Public RIS field cannot carry BibTeX case protection; raw syntax is retained only in cm-v1/extra_json".into(),
                    });
                }
            }
        }
        Ok(encoded)
    }
}
