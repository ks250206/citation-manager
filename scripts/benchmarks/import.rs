//! #47 actual #13 parser -> default #33 retained preview -> explicit Create.
mod common;
use cm_core::{
    collections::{CollectionInput, CollectionService},
    import::*,
    library_query::{LibraryFilter, LibraryQuery, LibraryQueryService},
    ports::ImportService,
    tags::{TagInput, TagService},
};
use cm_metadata::bibtex;
use cm_storage::{LibraryDb, LibraryDirectory};
use common::{Result, milliseconds};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, time::Instant};

const SEED: usize = 47;
fn source(count: usize) -> Vec<u8> {
    (0..count).map(|i| {
        // Deliberately diverse deterministic titles avoid an artificial library
        // of near-identical titles overflowing the real 20-match preview budget.
        let title = format!("{:x}",Sha256::digest(format!("backend-bench-{SEED}-{i}")));
        let year = 2000+i%25;
        format!("@article{{Bench{i},\n title = {{Synthetic Ω {title}}},\n author = {{Family{i}, Synthetic and {{合成研究組織}}}},\n editor = {{Editor, Synthetic}},\n year = {{{year}}},\n date = {{{year}-01-02}},\n journal = {{Synthetic Journal {}}},\n doi = {{https://doi.org/10.99999/import-{i}}},\n url = {{https://example.invalid/import/{i}}},\n volume = {{12}},\n number = {{3}},\n pages = {{1--12}},\n language = {{ja}},\n abstract = {{Synthetic only. {}}},\n keywords = {{synthetic; 固体電解質}}\n}}\n",i%5,"Ionic transport and reproducible materials data. ".repeat(12))
    }).collect::<String>().into_bytes()
}
fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(std::fs::read(path)?)))
}
fn fingerprint(directory: &LibraryDirectory) -> Result<(String, String)> {
    Ok((
        digest(&directory.database_path())?,
        digest(&directory.database_path().with_extension("sqlite-wal"))?,
    ))
}
fn inventory(db: &LibraryDb) -> Result<Vec<cm_protocol::domain::Paper>> {
    let mut query = LibraryQuery {
        limit: 50,
        ..Default::default()
    };
    let mut papers = vec![];
    loop {
        let page = LibraryQueryService::new(db.library_query()).query(&query)?;
        papers.extend(page.papers);
        match page.next {
            Some(cursor) => query.after = Some(cursor),
            None => break,
        }
    }
    Ok(papers)
}
fn run(count: usize, samples: usize) -> Result<Value> {
    let generation = Instant::now();
    let bytes = source(count);
    let source_generation_ms = milliseconds(generation);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut runs = vec![];
    for sample in 0..samples {
        let setup = Instant::now();
        let (_temp, directory, mut db) = common::private_library()?;
        assert!(inventory(&db)?.is_empty());
        let collection = CollectionService::new(db.collections())
            .create(CollectionInput::named("Synthetic reviewed import"))?;
        let tag = TagService::new(db.tags()).create(TagInput::named("Synthetic imported"))?;
        let destination = ImportDestination {
            collection: Some(collection.id),
            tags: vec![tag.id],
        };
        // An independent production read connection observes real committed counts.
        let observer = LibraryDb::open(&directory)?;
        let setup_ms = milliseconds(setup);
        let total = Instant::now();
        let parse_start = Instant::now();
        let parsed = bibtex::parse(&bytes)?;
        let parse_ms = milliseconds(parse_start);
        assert_eq!(parsed.candidates.len(), count);
        assert!(parsed.diagnostics.is_empty());
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), sha256);
        let mut expected = BTreeMap::new();
        let mut batches = vec![];
        let mut coordinator = ImportCoordinator::new(db.imports());
        for start in (0..count).step_by(100) {
            let batch_count = (count - start).min(100);
            let request = parsed.batch(start, batch_count, destination.clone())?;
            let before_count = inventory(&observer)?.len();
            assert_eq!(before_count, start);
            let before = fingerprint(&directory)?;
            let preview_start = Instant::now();
            let preview = coordinator.preview(request)?;
            let preview_ms = milliseconds(preview_start);
            // Byte-for-byte DB+WAL comparison: no hidden preview writes in any table.
            assert_eq!(fingerprint(&directory)?, before);
            assert_eq!(inventory(&observer)?.len(), before_count);
            assert_eq!(preview.candidates.len(), batch_count);
            let decision_start = Instant::now();
            let mut decisions = vec![];
            let mut suggested_matches = 0;
            for (candidate_index, c) in preview.candidates.iter().enumerate() {
                assert!(!c.diagnostics.iter().any(|d| d.error));
                assert!(c.batch_matches.is_empty());
                assert!(c.matches.iter().all(|m| !m.kind.exact()));
                suggested_matches += c.matches.len();
                assert_eq!(c.draft.source.sha256, Some(parsed.sha256));
                expected.insert(c.draft.bibliography.doi.clone().unwrap(), c.draft.clone());
                // Explicit synthetic fixture policy: reviewed valid unique DOI/key;
                // do not merge resemblance suggestions. No user approval is modeled.
                decisions.push(ImportDecision {
                    candidate_index,
                    action: ImportAction::Create,
                });
            }
            let decision_ms = milliseconds(decision_start);
            let commit_start = Instant::now();
            let outcome = coordinator.commit(ImportCommit {
                preview_id: preview.id,
                decisions,
            })?;
            let commit_ms = milliseconds(commit_start);
            assert_eq!(outcome.created.len(), batch_count);
            assert_eq!(outcome.rows.len(), batch_count);
            assert!(outcome.merged.is_empty());
            assert_eq!(outcome.skipped, 0);
            assert_eq!(inventory(&observer)?.len(), start + batch_count);
            batches.push(json!({"batch":batches.len(),"start":start,"rows":batch_count,"before_count":before_count,"after_count":start+batch_count,"preview_ms":preview_ms,"decision_ms":decision_ms,"commit_ms":commit_ms,"created":outcome.created.len(),"merged":0,"skipped":0,"suggested_matches":suggested_matches,"preview_db_wal_unchanged":true}));
            eprintln!(
                "synthetic import sample {sample} batch {}: preview {preview_ms:.3} ms, commit {commit_ms:.3} ms, stored {}",
                batches.len(),
                start + batch_count
            );
        }
        let total_ms = milliseconds(total);
        drop(coordinator);
        drop(observer);
        drop(db);
        let validation = Instant::now();
        let mut reopened = LibraryDb::open(&directory)?;
        let papers = inventory(&reopened)?;
        assert_eq!(papers.len(), count);
        for paper in &papers {
            let draft = &expected[paper.bibliography.doi.as_ref().unwrap()];
            assert_eq!(paper.title, draft.title);
            assert_eq!(paper.bibliography, draft.bibliography);
            assert_eq!(paper.external_ids, draft.external_ids);
            assert!(
                paper.created_at.is_some()
                    && paper.updated_at.is_some()
                    && paper.deleted_at.is_none()
            );
            assert_eq!(
                CollectionService::new(reopened.collections())
                    .for_paper(paper.id)?
                    .iter()
                    .map(|c| c.id)
                    .collect::<Vec<_>>(),
                vec![collection.id]
            );
            assert_eq!(
                TagService::new(reopened.tags())
                    .for_paper(paper.id)?
                    .iter()
                    .map(|t| t.id)
                    .collect::<Vec<_>>(),
                vec![tag.id]
            );
        }
        let query = LibraryQuery {
            filter: LibraryFilter {
                collection: Some(collection.id),
                tags: vec![tag.id],
                ..Default::default()
            },
            limit: 50,
            ..Default::default()
        };
        assert_eq!(
            LibraryQueryService::new(reopened.library_query())
                .query(&query)?
                .papers
                .len(),
            50.min(count)
        );
        let validation_ms = milliseconds(validation);
        runs.push(json!({"sample":sample,"setup_ms":setup_ms,"parse_ms":parse_ms,"total_ms":total_ms,"post_reopen_validation_ms":validation_ms,"stored_count":papers.len(),"full_metadata_equal":true,"memberships_validated":papers.len(),"batches":batches}));
    }
    Ok(
        json!({"kind":"import","records":count,"seed":SEED,"format":"BibTeX","source_bytes":bytes.len(),"source_sha256":sha256,"source_generation_ms":source_generation_ms,"batch_limit":100,"policy":"default ImportCoordinator; explicit fixture Create decisions; no human/IPC approval claim","runs":runs}),
    )
}
fn main() -> Result<()> {
    common::entry(run, 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_parser_preview_commit_no_write_and_reopen() -> Result<()> {
        assert_eq!(source(3), source(3));
        let report = run(101, 1)?;
        assert_eq!(report["runs"][0]["stored_count"], 101);
        assert_eq!(report["runs"][0]["batches"][0]["created"], 100);
        assert_eq!(report["runs"][0]["batches"][1]["created"], 1);
        assert_eq!(
            report["runs"][0]["batches"][0]["preview_db_wal_unchanged"],
            true
        );
        Ok(())
    }
}
