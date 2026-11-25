//! #30 representative library fixture: 10,000 synthetic papers (direct fixture SQL, as in
//! #47) plus N synthetic PDFs attached through the real managed attachment store, then
//! read-only post-indexing statistics. Driven by `scripts/benchmarks/representative.py`.
//!
//! `seed <new-library-dir> <pdf-dir> <papers> <pdfs>`: the library directory must not
//! exist (never an existing user library). Prints JSON with timings, counts and the
//! attachment id of every PDF in corpus order (the oracle mapping).
//! `stats <library-dir>`: integrity, extraction states, page counts, index sizes, file sizes
//! and the extraction jobs' recorded timestamps. Opens a read-only connection.
use cm_core::attachments::{AttachmentLibrary, ImportAttachment};
use cm_domain::PaperId;
use cm_storage::{LibraryDb, LibraryDirectory};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const SEED: usize = 30;
pub const COLLECTIONS: usize = 10;
pub const TAGS: usize = 8;

fn id(kind: u128, index: usize) -> Uuid {
    Uuid::from_u128((kind << 96) | (index as u128 + 1))
}
pub fn paper_id(index: usize) -> PaperId {
    PaperId(id(1, index))
}
pub fn collection_id(index: usize) -> Uuid {
    id(2, index)
}
pub fn tag_id(index: usize) -> Uuid {
    id(3, index)
}
/// Papers whose title contains the metadata keyword `needle`.
pub fn is_needle(index: usize) -> bool {
    (index + SEED).is_multiple_of(7)
}
pub fn collections_of(index: usize) -> [usize; 2] {
    let n = index + SEED;
    [n % COLLECTIONS, (n % COLLECTIONS + 1) % COLLECTIONS]
}
pub fn tags_of(index: usize) -> [usize; 2] {
    let n = index + SEED;
    [n % TAGS, (n % TAGS + 1) % TAGS]
}
/// The paper that owns corpus PDF `pdf`: one per `papers/pdfs` block, at a rotating offset
/// so the owners spread evenly over all collections and tags.
pub fn pdf_owner(pdf: usize, papers: usize, pdfs: usize) -> usize {
    let step = (papers / pdfs.max(1)).max(1);
    pdf * step + pdf % step
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Metadata text never contains the planted page-text query terms (see pdfgen.py), so page
/// hits can be compared with the page oracle exactly.
fn seed_papers(directory: &LibraryDirectory, count: usize) -> Result<Value> {
    drop(LibraryDb::open(directory)?);
    let mut db = Connection::open_with_flags(
        directory.database_path(),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    let start = Instant::now();
    let tx = db.transaction()?;
    for j in 0..COLLECTIONS {
        tx.execute(
            "INSERT INTO collections(id,name,sort_order) VALUES(?1,?2,?3)",
            params![
                collection_id(j).to_string(),
                format!("Bench collection {j}"),
                j as i64
            ],
        )?;
    }
    for j in 0..TAGS {
        tx.execute(
            "INSERT INTO tags(id,name,normalized_name) VALUES(?1,?2,?3)",
            params![
                tag_id(j).to_string(),
                format!("Bench tag {j}"),
                format!("bench tag {j}")
            ],
        )?;
    }
    for i in 0..count {
        let n = i + SEED;
        let paper = paper_id(i).0.to_string();
        let title = format!(
            "Synthetic {} transport {i:05} 界面研究 Ω",
            if is_needle(i) { "needle" } else { "matrix" }
        );
        let abstract_text = format!(
            "Synthetic abstract {}",
            "ionic transport and reproducible materials data. ".repeat(12)
        );
        tx.execute(
            "INSERT INTO papers(id,type,title,abstract,year,publication_date,journal,volume,issue,pages,doi_normalized,url,language,citation_key,extra_json,created_at,updated_at)
             VALUES(?1,'journal-article',?2,?3,?4,?5,?6,'12','3','1–12',?7,?8,'ja',?9,?10,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
            params![
                paper,
                title,
                abstract_text,
                2000 + (n % 25) as i64,
                format!("{}-01-01", 2000 + n % 25),
                format!("Synthetic Journal {}", n % 5),
                format!("10.99999/bench-{i}"),
                format!("https://example.invalid/bench/{i}"),
                format!("Bench{i:05}"),
                json!({"keywords": ["synthetic"], "seed": SEED}).to_string(),
            ],
        )?;
        for (j, (given, family, literal, role)) in [
            (
                Some("Synthetic"),
                Some(format!("Family{}", n % 13)),
                None,
                "author",
            ),
            (None, None, Some("合成編集組織"), "editor"),
        ]
        .into_iter()
        .enumerate()
        {
            let person = id(4, i * 2 + j).to_string();
            tx.execute(
                "INSERT INTO people(id,given_name,family_name,literal_name,orcid) VALUES(?1,?2,?3,?4,NULL)",
                params![person, given, family, literal],
            )?;
            tx.execute(
                "INSERT INTO paper_people VALUES(?1,?2,?3,?4)",
                params![paper, person, role, j as i64],
            )?;
        }
        tx.execute(
            "INSERT INTO paper_external_ids VALUES(?1,'doi',?2,0)",
            params![paper, format!("10.99999/bench-{i}")],
        )?;
        for c in collections_of(i) {
            tx.execute(
                "INSERT INTO paper_collections VALUES(?1,?2)",
                params![paper, collection_id(c).to_string()],
            )?;
        }
        for t in tags_of(i) {
            tx.execute(
                "INSERT INTO paper_tags VALUES(?1,?2)",
                params![paper, tag_id(t).to_string()],
            )?;
        }
    }
    tx.commit()?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    Ok(json!({"papers": count, "seed_ms": milliseconds(start),
        "method": "dedicated fixture SQL connection, one transaction, FULL/WAL (search triggers fire); not CRUD throughput"}))
}

fn seed(library: &Path, pdf_dir: &Path, papers: usize, pdfs: usize) -> Result<Value> {
    if library.exists() || !library.is_absolute() {
        return Err("library must be a new absolute path".into());
    }
    if pdfs > papers {
        return Err("more PDFs than papers".into());
    }
    let directory = LibraryDirectory::prepare(library)?;
    let papers_report = seed_papers(&directory, papers)?;
    let mut db = LibraryDb::open(&directory)?;
    let start = Instant::now();
    let mut attachments = Vec::with_capacity(pdfs);
    let mut pdf_bytes = 0u64;
    for pdf in 0..pdfs {
        let source = pdf_dir.join(format!("pdf-{pdf:04}.pdf"));
        let attachment = db
            .managed_attachments(&directory)?
            .import(ImportAttachment {
                paper_id: paper_id(pdf_owner(pdf, papers, pdfs)),
                source,
                original_name: format!("pdf-{pdf:04}.pdf"),
                media_type: "application/pdf".into(),
                expected_sha256: None,
            })
            .map_err(|e| format!("attach {pdf}: {e:?}"))?;
        pdf_bytes += attachment.byte_size;
        attachments.push(attachment.id.0.to_string());
    }
    let attach_ms = milliseconds(start);
    drop(db);
    Ok(json!({
        "seed": SEED, "papers": papers_report, "pdfs": pdfs, "pdf_bytes": pdf_bytes,
        "attach_ms": attach_ms,
        "attach_method": "ManagedAttachments::import per PDF (copy, re-hash, fsync, atomic publish, DB reference)",
        "attachments": attachments,
    }))
}

fn file_size(path: PathBuf) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
fn tree_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => tree_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn stats(library: &Path) -> Result<Value> {
    let directory = LibraryDirectory::prepare(library)?;
    let db_path = directory.database_path();
    let sizes = json!({
        "library_sqlite": file_size(db_path.clone()),
        "library_sqlite_wal": file_size(db_path.with_extension("sqlite-wal")),
        "attachments_dir": tree_size(&library.join("attachments")),
        "backups_dir": tree_size(&library.join("backups")),
    });
    let index_bytes: Vec<(String, u64)> = {
        let db = LibraryDb::open(&directory)?;
        db.search_index().index_bytes()?
    };
    let c = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let one = |sql: &str| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
    let mut states = serde_json::Map::new();
    for row in c
        .prepare("SELECT status, count(*), sum(page_count), sum(pages_with_text) FROM fulltext_extractions GROUP BY status ORDER BY status")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
    {
        let (status, n, pages, with_text) = row?;
        states.insert(
            status,
            json!({"attachments": n, "pages": pages, "pages_with_text": with_text}),
        );
    }
    let jobs = c
        .prepare("SELECT kind, state, attempts, created_at_ms, updated_at_ms, coalesce(error,'') FROM daemon_jobs ORDER BY created_at_ms")?
        .query_map([], |r| {
            Ok(json!({"kind": r.get::<_, String>(0)?, "state": r.get::<_, String>(1)?,
                "attempts": r.get::<_, i64>(2)?, "created_at_ms": r.get::<_, i64>(3)?,
                "updated_at_ms": r.get::<_, i64>(4)?, "error": r.get::<_, String>(5)?}))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total_index: u64 = index_bytes.iter().map(|(_, b)| b).sum();
    Ok(json!({
        "integrity_check": c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "papers": one("SELECT count(*) FROM papers")?,
        "attachments": one("SELECT count(*) FROM attachments")?,
        "fulltext_pages": one("SELECT count(*) FROM fulltext_pages")?,
        "fulltext_text_bytes": one("SELECT coalesce(sum(length(CAST(text AS BLOB))),0) FROM fulltext_pages")?,
        "search_documents": one("SELECT count(*) FROM search_documents")?,
        "extraction_states": states,
        "index_bytes": index_bytes.into_iter().map(|(k, v)| (k, json!(v)))
            .collect::<serde_json::Map<String, Value>>(),
        "index_bytes_total": total_index,
        "file_bytes": sizes,
        "jobs": jobs,
    }))
}

fn main() -> Result<()> {
    if cfg!(debug_assertions) {
        return Err("benchmark executable requires --release".into());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let report = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["seed", library, pdfs_dir, papers, pdfs] => seed(
            Path::new(library),
            Path::new(pdfs_dir),
            papers.parse()?,
            pdfs.parse()?,
        )?,
        ["stats", library] => stats(Path::new(library))?,
        _ => return Err("usage: seed LIB PDF_DIR PAPERS PDFS | stats LIB".into()),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_seed_attaches_and_reports() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let pdfs = root.join("pdfs");
        std::fs::create_dir(&pdfs)?;
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/library/layouts.pdf");
        for i in 0..2 {
            // Distinct bytes per file so each import publishes its own blob.
            let mut bytes = std::fs::read(&fixture)?;
            bytes.extend_from_slice(format!("\n%bench {i}\n").as_bytes());
            std::fs::write(pdfs.join(format!("pdf-{i:04}.pdf")), bytes)?;
        }
        let report = seed(&root.join("library"), &pdfs, 23, 2)?;
        assert_eq!(report["attachments"].as_array().unwrap().len(), 2);
        assert!(
            seed(&root.join("library"), &pdfs, 23, 2).is_err(),
            "never reuses a library"
        );
        let stats = stats(&root.join("library"))?;
        assert_eq!(stats["integrity_check"], "ok");
        assert_eq!(stats["papers"], 23);
        assert_eq!(stats["attachments"], 2);
        assert_eq!(pdf_owner(1, 23, 2), 12);
        assert_eq!(pdf_owner(999, 10_000, 1_000), 9_999);
        assert_eq!(collections_of(0), [0, 1]);
        Ok(())
    }
}
