//! Bounded, read-only bibliography export on the real library query port.
//! Pages observe latest committed state, not a cross-page snapshot (#12).
use crate::{ServiceError, import::normalize::invalid, library_query::*};
use cm_domain::{CollectionId, Paper, PaperId};
use std::collections::HashSet;

pub const MAX_RECORDS: usize = 10_000;
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 256 * 1024;
pub const MIGRATION_NOTICE: &str = "BibTeX/RIS transfer bibliography only. PDFs, attachments, annotations, collection/tag relationships and internal IDs are not transferred.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportSelection {
    /// Active records only, as in #12's default query.
    All,
    /// Unique, non-nil active IDs; missing/trashed IDs fail, never silently disappear.
    Ids(Vec<PaperId>),
    /// Active direct members only; no descendants (#12).
    Collection(CollectionId),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDiagnostic {
    pub paper: Option<PaperId>,
    pub field: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedRecord {
    pub text: String,
    pub diagnostics: Vec<ExportDiagnostic>,
}
/// Implementations must bound input/work before allocating; each returned record
/// must fit MAX_RECORD_BYTES. Concrete formats may impose smaller round-trip limits.
/// No filesystem, network, attachment reading or database writes are required.
pub trait BibliographySerializer {
    fn encode(&self, paper: &Paper) -> Result<EncodedRecord, ServiceError>;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    pub text: String,
    pub records: usize,
    pub diagnostics: Vec<ExportDiagnostic>,
    pub migration_notice: String,
}
pub struct ExportService<R> {
    repository: R,
}
impl<R: LibraryQueryRepository> ExportService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
    pub fn export(
        &self,
        selection: ExportSelection,
        serializer: &impl BibliographySerializer,
    ) -> Result<ExportReport, ServiceError> {
        let mut query = LibraryQuery {
            limit: 500,
            ..Default::default()
        };
        let selected = match selection {
            ExportSelection::All => None,
            ExportSelection::Collection(id) => {
                query.filter.collection = Some(id);
                None
            }
            ExportSelection::Ids(ids) => {
                let unique: HashSet<_> = ids.iter().copied().collect();
                if ids.len() > MAX_RECORDS
                    || unique.len() != ids.len()
                    || ids.iter().any(|id| id.0.is_nil())
                {
                    return Err(invalid(
                        "export requires unique non-nil IDs within record budget",
                    ));
                }
                Some(unique)
            }
        };
        query.validate()?;
        let mut report = ExportReport {
            text: String::new(),
            records: 0,
            diagnostics: vec![],
            migration_notice: MIGRATION_NOTICE.into(),
        };
        let mut seen = HashSet::new();
        let mut scanned = 0;
        if selected.as_ref().is_some_and(HashSet::is_empty) {
            return Ok(report);
        }
        loop {
            let page = self.repository.query(&query)?;
            for paper in page.papers {
                scanned += 1;
                if scanned > 20_000 || !seen.insert(paper.id) {
                    return Err(invalid(
                        "export scan budget or repeated ID; refresh library",
                    ));
                }
                if selected
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&paper.id))
                {
                    continue;
                }
                if report.records >= MAX_RECORDS {
                    return Err(invalid("export record budget exceeded"));
                }
                if paper
                    .bibliography
                    .extra_json
                    .as_ref()
                    .is_some_and(|v| !json_depth(v, 0))
                {
                    return Err(invalid("export JSON nesting budget exceeded"));
                }
                // A bounded writer avoids allocating a serialized copy of huge metadata.
                let mut budget = ByteBudget(MAX_RECORD_BYTES);
                serde_json::to_writer(&mut budget, &paper)
                    .map_err(|_| invalid("export paper exceeds record budget"))?;
                let encoded = serializer.encode(&paper)?;
                if encoded.text.len() > MAX_RECORD_BYTES
                    || report.text.len() + encoded.text.len() > MAX_OUTPUT_BYTES
                    || encoded.diagnostics.len() > 32
                    || encoded
                        .diagnostics
                        .iter()
                        .any(|d| d.field.len() > 128 || d.message.len() > 1024)
                {
                    return Err(invalid("export output/diagnostic budget exceeded"));
                }
                report.text.push_str(&encoded.text);
                report.diagnostics.extend(encoded.diagnostics);
                report.records += 1;
            }
            match page.next {
                Some(next) => {
                    if query.after.as_ref() == Some(&next) {
                        return Err(invalid("non-progressing export cursor"));
                    }
                    query.after = Some(next);
                }
                None => break,
            }
        }
        if selected
            .as_ref()
            .is_some_and(|ids| ids.len() != report.records)
        {
            return Err(ServiceError::NotFound);
        }
        Ok(report)
    }
}
/// Used by adapters too: fail before extending a serialization buffer past a budget.
pub struct ByteBudget(pub usize);
impl std::io::Write for ByteBudget {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() > self.0 {
            return Err(std::io::Error::other("byte budget exceeded"));
        }
        self.0 -= buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn json_depth(value: &serde_json::Value, level: usize) -> bool {
    if level > 32 {
        return false;
    }
    match value {
        serde_json::Value::Array(a) => a.iter().all(|v| json_depth(v, level + 1)),
        serde_json::Value::Object(o) => o.values().all(|v| json_depth(v, level + 1)),
        _ => true,
    }
}
