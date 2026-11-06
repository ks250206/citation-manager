//! #47 synthetic-only SQL fixture, measured through the unchanged #12 service.
mod common;
use cm_core::library_query::{LibraryFilter, LibraryQuery, LibraryQueryService, TagMatch};
use cm_domain::{Bibliography, CollectionId, Contributor, ExternalId, Paper, PaperId, TagId};
use cm_storage::{LibraryDb, LibraryDirectory};
use common::{Result, milliseconds};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Instant;
use uuid::Uuid;

const SEED: usize = 47;
const PAGE: usize = 50;
fn id(kind: u128, index: usize) -> Uuid {
    Uuid::from_u128((kind << 96) | (index as u128 + 1))
}
fn fixture(i: usize) -> Paper {
    let n = i + SEED;
    Paper {
        id: PaperId(id(1, i)),
        title: format!(
            "Synthetic {} transport {i:05} 固体電解質 Ω",
            if n.is_multiple_of(7) {
                "needle"
            } else {
                "matrix"
            }
        ),
        external_ids: vec![
            ExternalId {
                namespace: "doi".into(),
                value: format!("10.99999/synthetic-{i}"),
            },
            ExternalId {
                namespace: "synthetic".into(),
                value: format!("accession-{i:05}"),
            },
        ],
        bibliography: Bibliography {
            paper_type: Some("journal-article".into()),
            abstract_text: Some(format!(
                "Synthetic abstract {}",
                "ionic transport and reproducible materials data. ".repeat(12)
            )),
            year: Some(2000 + (n % 25) as i32),
            publication_date: Some(format!("{}-01-01", 2000 + n % 25)),
            journal: Some(format!("Synthetic Journal {}", n % 5)),
            volume: Some("12".into()),
            issue: Some("3".into()),
            pages: Some("1–12".into()),
            doi: Some(format!("10.99999/synthetic-{i}")),
            url: Some(format!("https://example.invalid/synthetic/{i}")),
            language: Some("ja".into()),
            citation_key: Some(format!("Synthetic{i:05}")),
            authors: vec![
                Contributor {
                    given_name: Some("Synthetic".into()),
                    family_name: Some(format!("Family{}", n % 13)),
                    literal_name: None,
                    orcid: None,
                    role: "author".into(),
                },
                Contributor {
                    given_name: None,
                    family_name: None,
                    literal_name: Some("合成編集組織".into()),
                    orcid: None,
                    role: "editor".into(),
                },
            ],
            extra_json: Some(json!({"keywords":["synthetic", "固体電解質"], "seed":SEED})),
        },
        created_at: Some("2026-01-01T00:00:00.000Z".into()),
        updated_at: Some("2026-01-01T00:00:00.000Z".into()),
        deleted_at: None,
    }
}
fn fixture_connection(directory: &LibraryDirectory) -> Result<Connection> {
    // Public factory revalidates private paths/sidecars; do not expose its connection.
    drop(LibraryDb::open(directory)?);
    let db = Connection::open_with_flags(
        directory.database_path(),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    assert!(db.pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0))?);
    assert_eq!(
        db.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))?,
        "wal"
    );
    Ok(db)
}
fn seed(directory: &LibraryDirectory, count: usize) -> Result<Value> {
    let mut db = fixture_connection(directory)?;
    let start = Instant::now();
    let tx = db.transaction()?;
    for j in 0..10 {
        tx.execute(
            "INSERT INTO collections(id,name,sort_order) VALUES(?1,?2,?3)",
            params![
                id(2, j).to_string(),
                format!("Synthetic collection {j}"),
                j as i64
            ],
        )?;
    }
    for j in 0..8 {
        tx.execute(
            "INSERT INTO tags(id,name,normalized_name) VALUES(?1,?2,?3)",
            params![
                id(3, j).to_string(),
                format!("Synthetic tag {j}"),
                format!("synthetic tag {j}")
            ],
        )?;
    }
    let mut digest = Sha256::new();
    for i in 0..count {
        let p = fixture(i);
        digest.update(serde_json::to_vec(&p)?);
        let b = &p.bibliography;
        tx.execute("INSERT INTO papers(id,type,title,abstract,year,publication_date,journal,volume,issue,pages,doi_normalized,url,language,citation_key,extra_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)", params![p.id.0.to_string(),b.paper_type,p.title,b.abstract_text,b.year,b.publication_date,b.journal,b.volume,b.issue,b.pages,b.doi,b.url,b.language,b.citation_key,b.extra_json.as_ref().map(Value::to_string),p.created_at,p.updated_at])?;
        for (j, a) in b.authors.iter().enumerate() {
            let person = id(4, i * 2 + j).to_string();
            tx.execute("INSERT INTO people(id,given_name,family_name,literal_name,orcid) VALUES(?1,?2,?3,?4,?5)", params![person,a.given_name,a.family_name,a.literal_name,a.orcid])?;
            tx.execute(
                "INSERT INTO paper_people VALUES(?1,?2,?3,?4)",
                params![p.id.0.to_string(), person, a.role, j as i64],
            )?;
        }
        for (j, e) in p.external_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO paper_external_ids VALUES(?1,?2,?3,?4)",
                params![p.id.0.to_string(), e.namespace, e.value, j as i64],
            )?;
        }
        for c in [(i + SEED) % 10, ((i + SEED) % 10 + 1) % 10] {
            tx.execute(
                "INSERT INTO paper_collections VALUES(?1,?2)",
                params![p.id.0.to_string(), id(2, c).to_string()],
            )?;
        }
        for t in [(i + SEED) % 8, ((i + SEED) % 8 + 1) % 8] {
            tx.execute(
                "INSERT INTO paper_tags VALUES(?1,?2)",
                params![p.id.0.to_string(), id(3, t).to_string()],
            )?;
        }
    }
    tx.commit()?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let seed_ms = milliseconds(start);
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    assert!(!db.prepare("PRAGMA foreign_key_check")?.exists([])?);
    let mut counts = serde_json::Map::new();
    for (table, expected) in [
        ("papers", count),
        ("people", 2 * count),
        ("paper_people", 2 * count),
        ("paper_external_ids", 2 * count),
        ("collections", 10),
        ("tags", 8),
        ("paper_collections", 2 * count),
        ("paper_tags", 2 * count),
        ("attachments", 0),
        ("fulltext_pages", 0),
        ("annotations", 0),
        ("jobs", 0),
        ("api_tokens", 0),
    ] {
        let actual = db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })?;
        assert_eq!(usize::try_from(actual)?, expected, "{table}");
        counts.insert(table.into(), json!(actual));
    }
    for (table, column, kind, modulus) in [
        ("paper_collections", "collection_id", 2, 10),
        ("paper_tags", "tag_id", 3, 8),
    ] {
        let actual = db
            .prepare(&format!(
                "SELECT paper_id,{column} FROM {table} ORDER BY paper_id,{column}"
            ))?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut expected = (0..count)
            .flat_map(|i| {
                [(i + SEED) % modulus, ((i + SEED) % modulus + 1) % modulus]
                    .map(|j| (id(1, i).to_string(), id(kind, j).to_string()))
            })
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(actual, expected, "{table} exact membership contents");
    }
    drop(LibraryDb::open(directory)?);
    Ok(
        json!({"seed_ms":seed_ms,"method":"dedicated fixture SQL connection, one transaction, FULL/WAL, checkpoint; not CRUD/import throughput", "sha256":format!("{:x}",digest.finalize()),"counts":counts,"integrity_check":"ok","foreign_key_check_rows":0}),
    )
}
fn cases() -> Vec<(&'static str, LibraryFilter)> {
    vec![
        ("list", LibraryFilter::default()),
        (
            "metadata_keyword",
            LibraryFilter {
                keyword: Some("needle".into()),
                ..Default::default()
            },
        ),
        (
            "metadata_no_hit",
            LibraryFilter {
                keyword: Some("absent-sentinel-47".into()),
                ..Default::default()
            },
        ),
        (
            "collection",
            LibraryFilter {
                collection: Some(CollectionId(id(2, 0))),
                ..Default::default()
            },
        ),
        (
            "tag_all",
            LibraryFilter {
                tags: vec![TagId(id(3, 0)), TagId(id(3, 1))],
                ..Default::default()
            },
        ),
        (
            "tag_any",
            LibraryFilter {
                tags: vec![TagId(id(3, 0)), TagId(id(3, 1))],
                tag_match: TagMatch::Any,
                ..Default::default()
            },
        ),
        (
            "combined",
            LibraryFilter {
                collection: Some(CollectionId(id(2, 0))),
                tags: vec![TagId(id(3, 0))],
                keyword: Some("needle".into()),
                ..Default::default()
            },
        ),
    ]
}
fn expected(name: &str, count: usize) -> Vec<PaperId> {
    (0..count)
        .filter(|i| {
            let n = i + SEED;
            let collection = n.is_multiple_of(10) || n % 10 == 9;
            let tag0 = n.is_multiple_of(8) || n % 8 == 7;
            match name {
                "list" => true,
                "metadata_keyword" => n.is_multiple_of(7),
                "metadata_no_hit" => false,
                "collection" => collection,
                "tag_all" => n.is_multiple_of(8),
                "tag_any" => tag0 || n % 8 == 1,
                "combined" => collection && tag0 && n.is_multiple_of(7),
                _ => unreachable!(),
            }
        })
        .map(|i| PaperId(id(1, i)))
        .collect()
}
fn verify_page(papers: &[Paper], ids: &[PaperId]) {
    assert_eq!(papers.iter().map(|p| p.id).collect::<Vec<_>>(), ids);
    for p in papers {
        let i = (p.id.0.as_u128() & 0xffff_ffff) as usize - 1;
        assert_eq!(*p, fixture(i));
    }
}
fn run(count: usize, samples: usize) -> Result<Value> {
    let (_temp, directory, db) = common::private_library()?;
    assert!(db.foreign_keys_enabled()?);
    assert_eq!(db.journal_mode()?, "wal");
    drop(db);
    let seeded = seed(&directory, count)?;
    let validation_start = Instant::now();
    let db = LibraryDb::open(&directory)?;
    let mut validations = vec![];
    let mut scenarios = vec![];
    for (name, filter) in cases() {
        let ids = expected(name, count);
        let mut query = LibraryQuery {
            filter,
            limit: PAGE as u32,
            ..Default::default()
        };
        let mut offset = 0;
        let mut first_cursor = None;
        let mut pages = 0;
        loop {
            let page = LibraryQueryService::new(db.library_query()).query(&query)?;
            let end = (offset + PAGE).min(ids.len());
            verify_page(&page.papers, &ids[offset..end]);
            assert_eq!(page.next.is_some(), end < ids.len());
            if pages == 0 {
                first_cursor = page.next.clone();
            }
            offset = end;
            pages += 1;
            match page.next {
                Some(next) => query.after = Some(next),
                None => break,
            }
            assert!(pages <= count.div_ceil(PAGE));
        }
        assert_eq!(offset, ids.len());
        validations.push(json!({"case":name,"expected_hits":ids.len(),"actual_hits":offset,"pages":pages,"full_metadata_equal":true}));
        query.after = None;
        scenarios.push((name, query.clone(), ids[..PAGE.min(ids.len())].to_vec(), 0));
        if let Some(cursor) = first_cursor {
            query.after = Some(cursor);
            scenarios.push((
                name,
                query,
                ids[PAGE..(2 * PAGE).min(ids.len())].to_vec(),
                1,
            ));
        }
    }
    drop(db);
    let validation_ms = milliseconds(validation_start);
    let mut measurements = vec![];
    // Cold means a fresh production connection for each page; OS caches are NOT evicted.
    for (name, query, ids, page_index) in scenarios {
        for condition in [
            "reopened_connection_os_cache_uncontrolled",
            "warm_connection",
        ] {
            let warm = if condition == "warm_connection" {
                Some(LibraryDb::open(&directory)?)
            } else {
                None
            };
            if let Some(db) = &warm {
                LibraryQueryService::new(db.library_query()).query(&query)?;
            }
            for sample in 0..samples {
                let open_start = Instant::now();
                let cold = if warm.is_none() {
                    Some(LibraryDb::open(&directory)?)
                } else {
                    None
                };
                let open_ms = if cold.is_some() {
                    milliseconds(open_start)
                } else {
                    0.0
                };
                let db = warm.as_ref().or(cold.as_ref()).unwrap();
                let start = Instant::now();
                let page = LibraryQueryService::new(db.library_query()).query(&query)?;
                let elapsed_ms = milliseconds(start);
                verify_page(&page.papers, &ids);
                measurements.push(json!({"case":name,"condition":condition,"page":page_index,"sample":sample,"elapsed_ms":elapsed_ms,"open_ms":open_ms,"returned":page.papers.len(),"expected_returned":ids.len()}));
            }
        }
    }
    Ok(
        json!({"kind":"query","records":count,"seed":SEED,"page_limit":PAGE,"fixture":seeded,"validation_ms":validation_ms,"validation":validations,"samples":measurements,"sqlite_version":rusqlite::version()}),
    )
}
fn main() -> Result<()> {
    common::entry(run, 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_real_disk_fixture_counts_metadata_and_cursor() -> Result<()> {
        let report = run(117, 2)?;
        assert_eq!(report["fixture"]["counts"]["papers"], 117);
        assert_eq!(report["validation"][0]["pages"], 3);
        assert!(
            report["samples"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["returned"] == v["expected_returned"])
        );
        Ok(())
    }
}
