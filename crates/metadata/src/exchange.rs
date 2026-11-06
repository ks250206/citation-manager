//! Format-neutral parse report and explicit batching. No commits or DB access.
use cm_core::{
    ServiceError,
    export::*,
    import::{
        normalize::{MAX_BATCH, invalid, validate_limits},
        *,
    },
};
use cm_domain::{Contributor, ExternalId, Paper};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RECORDS: usize = 10_000;
pub const MAX_RECORD_BYTES: usize = 64 * 1024;
pub const MAX_DEPTH: usize = 32;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub line: u64,
    pub message: String,
    pub error: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseReport {
    pub candidates: Vec<PaperDraft>,
    /// File/directive/malformed-block diagnostics, including records not recoverable as drafts.
    pub diagnostics: Vec<ParseDiagnostic>,
    pub sha256: [u8; 32],
    pub migration_notice: String,
    /// Accounted serialized drafts, bounded to 32 MiB across the entire file.
    pub draft_bytes: usize,
}
impl ParseReport {
    /// Caller explicitly selects the next bounded range, reviews a fresh retained
    /// preview, and commits/cancels it. There is no file-wide transaction or auto approval.
    pub fn batch(
        &self,
        start: usize,
        count: usize,
        destination: ImportDestination,
    ) -> Result<ImportRequest, ServiceError> {
        let end = start
            .checked_add(count)
            .ok_or_else(|| invalid("batch range overflow"))?;
        if count == 0 || count > MAX_BATCH || end > self.candidates.len() {
            return Err(invalid(
                "explicit batch must contain 1..=100 available candidates",
            ));
        }
        Ok(ImportRequest {
            candidates: self.candidates[start..end].to_vec(),
            destination,
        })
    }
    pub(crate) fn new(input: &[u8]) -> Self {
        Self {
            candidates: vec![],
            diagnostics: vec![],
            sha256: Sha256::digest(input).into(),
            draft_bytes: 0,
            migration_notice: MIGRATION_NOTICE.into(),
        }
    }
    pub(crate) fn diagnostic(&mut self, line: u64, message: impl Into<String>, error: bool) {
        self.diagnostics.push(ParseDiagnostic {
            line,
            message: message.into(),
            error,
        });
    }
    pub(crate) fn push(
        &mut self,
        mut draft: PaperDraft,
        adapter: &str,
        line: u64,
    ) -> Result<(), ServiceError> {
        if self.candidates.len() >= MAX_RECORDS {
            return Err(invalid("parser record budget exceeded"));
        }
        draft.source = ImportSource {
            adapter: adapter.into(),
            locator: None,
            row: Some(line),
            sha256: Some(self.sha256),
        };
        if let Err(e) = validate_limits(&draft) {
            self.diagnostic(line, format!("Record rejected: {e}"), true);
            return Ok(());
        }
        self.draft_bytes += serde_json::to_vec(&draft)
            .map_err(|_| invalid("draft serialization failed"))?
            .len();
        if self.draft_bytes > 32 * 1024 * 1024 {
            return Err(invalid("file draft budget exceeded"));
        }
        self.candidates.push(draft);
        Ok(())
    }
}
pub(crate) fn utf8(input: &[u8]) -> Result<&str, ServiceError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(invalid("parser input exceeds 16 MiB"));
    }
    let text = std::str::from_utf8(input).map_err(|_| {
        invalid("unsupported encoding: UTF-8 required; convert explicitly (no guessed decoding)")
    })?;
    if text.contains('\0') {
        return Err(invalid(
            "unsupported encoding/NUL: UTF-16/32 is not UTF-8 text",
        ));
    }
    Ok(text.strip_prefix('\u{feff}').unwrap_or(text))
}
pub(crate) fn warn(d: &mut PaperDraft, field: &str, message: &str, error: bool) {
    // One summary per field/category; keep bounded even with hostile repeated input.
    if !d
        .diagnostics
        .iter()
        .any(|v| v.field == field && v.message == message)
    {
        d.diagnostics.push(RowDiagnostic {
            field: field.into(),
            message: message.into(),
            error,
        });
    }
}
pub(crate) fn person(name: &str, role: &str, literal: bool) -> Contributor {
    let (given, family, organization) = if literal {
        (None, None, Some(name.trim().to_owned()))
    } else if let Some((family, given)) = name.split_once(',') {
        (Some(given.trim().into()), Some(family.trim().into()), None)
    } else if let Some((given, family)) = name.trim().rsplit_once(char::is_whitespace) {
        (Some(given.trim().into()), Some(family.trim().into()), None)
    } else {
        (None, Some(name.trim().into()), None)
    };
    Contributor {
        given_name: given,
        family_name: family,
        literal_name: organization,
        orcid: None,
        role: role.into(),
    }
}
pub(crate) fn name(p: &Contributor) -> String {
    p.literal_name
        .clone()
        .unwrap_or_else(|| match (&p.family_name, &p.given_name) {
            (Some(f), Some(g)) => format!("{f}, {g}"),
            (Some(f), None) => f.clone(),
            (None, Some(g)) => g.clone(),
            _ => String::new(),
        })
}
pub(crate) fn extra(d: &mut PaperDraft, key: &str, value: Value) {
    d.bibliography.extra_json.get_or_insert_with(|| json!({}))[key] = value;
}
/// Private, versioned interoperability extension. Only otherwise nonrepresentable
/// members; ordinary title/DOI/year/etc always come from public format fields.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Extension {
    version: u8,
    public_sha256: [u8; 32],
    authors: Vec<Contributor>,
    external_ids: Vec<ExternalId>,
    extra_json: Option<Value>,
    publication_date: Option<String>,
    paper_type: Option<String>,
}
fn depth(value: &Value, level: usize) -> bool {
    if level > MAX_DEPTH {
        return false;
    }
    match value {
        Value::Array(a) => a.iter().all(|v| depth(v, level + 1)),
        Value::Object(o) => o.values().all(|v| depth(v, level + 1)),
        _ => true,
    }
}
pub(crate) fn check_paper(p: &Paper) -> Result<(), ServiceError> {
    if p.bibliography
        .extra_json
        .as_ref()
        .is_some_and(|v| !depth(v, 0))
    {
        return Err(invalid("serializer JSON nesting budget exceeded"));
    }
    serde_json::to_writer(&mut ByteBudget(MAX_RECORD_BYTES), p)
        .map_err(|_| invalid("serializer metadata exceeds byte budget"))?;
    Ok(())
}
fn public_hash(d: &PaperDraft) -> Result<[u8; 32], ServiceError> {
    let bytes = serde_json::to_vec(&(&d.title, &d.bibliography, &d.external_ids))
        .map_err(|_| invalid("public fields serialization failed"))?;
    Ok(Sha256::digest(bytes).into())
}
pub(crate) fn extension(p: &Paper, native: &PaperDraft) -> Result<String, ServiceError> {
    let b = &p.bibliography;
    let data = serde_json::to_vec(&Extension {
        version: 1,
        public_sha256: public_hash(native)?,
        authors: b.authors.clone(),
        external_ids: p.external_ids.clone(),
        extra_json: b.extra_json.clone(),
        publication_date: b.publication_date.clone(),
        paper_type: b.paper_type.clone(),
    })
    .map_err(|_| invalid("extension serialization failed"))?;
    Ok(data.iter().map(|v| format!("{v:02x}")).collect())
}
pub(crate) fn apply_extension(d: &mut PaperDraft, text: &str) -> Result<(), ServiceError> {
    if text.len() > MAX_RECORD_BYTES || !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err(invalid("invalid cm-v1 extension budget/hex"));
    }
    let bytes: Result<Vec<u8>, _> = text
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| u8::from_str_radix(std::str::from_utf8(s).unwrap(), 16))
        .collect();
    let bytes = bytes.map_err(|_| invalid("invalid cm-v1 extension hex"))?;
    // Iterative preflight rejects hostile deep JSON before serde's recursive reader.
    let mut nesting = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for &byte in &bytes {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == b'"' {
            quoted = !quoted;
            continue;
        }
        if !quoted {
            match byte {
                b'{' | b'[' => {
                    nesting += 1;
                    if nesting > MAX_DEPTH {
                        return Err(invalid("extension JSON nesting budget exceeded"));
                    }
                }
                b'}' | b']' => nesting = nesting.saturating_sub(1),
                _ => {}
            }
        }
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid cm-v1 extension JSON"))?;
    if !depth(&value, 0) {
        return Err(invalid("extension JSON nesting budget exceeded"));
    }
    let e: Extension =
        serde_json::from_value(value).map_err(|_| invalid("invalid cm-v1 extension structure"))?;
    if e.version != 1 {
        return Err(invalid("unsupported cm extension version"));
    }
    if e.public_sha256 != public_hash(d)? {
        return Err(invalid("stale cm-v1 extension: public fields were edited"));
    }
    d.bibliography.authors = e.authors;
    d.external_ids = e.external_ids;
    d.bibliography.extra_json = e.extra_json;
    d.bibliography.publication_date = e.publication_date;
    d.bibliography.paper_type = e.paper_type;
    warn(
        d,
        "cm-v1",
        "Private cm-v1 extension restored exact contributors, external IDs, extra fields, type and publication date; other applications may ignore it",
        false,
    );
    Ok(())
}
pub(crate) fn finish_record(
    p: &Paper,
    text: String,
    format: &str,
) -> Result<EncodedRecord, ServiceError> {
    if text.len() > MAX_RECORD_BYTES {
        return Err(invalid("serialized record exceeds output budget"));
    }
    let mut diagnostics = vec![ExportDiagnostic {
        paper: Some(p.id),
        field: "cm-v1".into(),
        message: format!(
            "{format} includes a private cm-v1 extension for exact contributors/external IDs/extra fields/type/date; other tools may ignore it"
        ),
    }];
    if p.bibliography.citation_key.is_none() {
        diagnostics.push(ExportDiagnostic {
            paper: Some(p.id),
            field: "citation_key".into(),
            message: "Missing key emitted empty; shared import allocates a key after preview"
                .into(),
        });
    }
    Ok(EncodedRecord { text, diagnostics })
}
