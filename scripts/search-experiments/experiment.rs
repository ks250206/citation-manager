//! #53 isolated SQL experiment. No production schema, imports, search service or library paths.
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const FIXTURE: &str = include_str!("fixture.json");
const EXPECTED: &str = include_str!("expected.json");
const TOKENIZERS: [(&str, &str); 2] = [
    ("unicode61", "unicode61 remove_diacritics 1"),
    ("trigram", "trigram case_sensitive 0 remove_diacritics 0"),
];
#[derive(Deserialize)]
struct Fixture {
    documents: Vec<Document>,
    literal_rule: String,
}
#[derive(Deserialize)]
struct Document {
    id: i64,
    bucket: String,
    text: String,
}
#[derive(Deserialize)]
struct Oracle {
    queries: Vec<Query>,
    fallback: Vec<Fallback>,
}
#[derive(Deserialize)]
struct Query {
    name: String,
    text: String,
    literal: Vec<i64>,
    unicode61: Vec<i64>,
    trigram: Vec<i64>,
}
#[derive(Deserialize)]
struct Fallback {
    name: String,
    text: String,
    bucket: String,
    cap: usize,
    expected_filtered: Vec<i64>,
    expected_searched: Vec<i64>,
}
#[derive(Debug, Serialize, PartialEq)]
struct Bounded {
    eligible_count: usize,
    candidate_ids: Vec<i64>,
    hits: Vec<i64>,
    compared_bytes: usize,
}
fn literal(text: &str, query: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&query.to_ascii_lowercase())
}
fn ids(db: &Connection, sql: &str, arg: &str) -> Result<Vec<i64>> {
    Ok(db
        .prepare(sql)?
        .query_map([arg], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
fn setup(db: &mut Connection, fixture: &Fixture) -> Result<()> {
    db.execute_batch(
        "PRAGMA page_size=4096; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
        CREATE TABLE corpus(id INTEGER PRIMARY KEY,bucket TEXT NOT NULL,text TEXT NOT NULL);
        CREATE INDEX corpus_bucket ON corpus(bucket,id);",
    )?;
    for (table, tokenizer) in TOKENIZERS {
        // Identifiers/options are fixed constants; all text input is bound.
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE {table} USING fts5(text,tokenize='{tokenizer}',detail=full,columnsize=1);
             CREATE VIRTUAL TABLE {table}_v USING fts5vocab({table},instance);
             CREATE VIRTUAL TABLE {table}_probe USING fts5(text,tokenize='{tokenizer}',detail=full);
             CREATE VIRTUAL TABLE {table}_qv USING fts5vocab({table}_probe,instance);"
        ))?;
    }
    let tx = db.transaction()?;
    for d in &fixture.documents {
        tx.execute(
            "INSERT INTO corpus VALUES(?1,?2,?3)",
            params![d.id, d.bucket, d.text],
        )?;
        for (table, _) in TOKENIZERS {
            tx.execute(
                &format!("INSERT INTO {table}(rowid,text) VALUES(?1,?2)"),
                params![d.id, d.text],
            )?;
        }
    }
    tx.commit()?;
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    Ok(())
}
fn tokens(db: &Connection, table: &str) -> Result<Vec<Value>> {
    Ok(db
        .prepare(&format!(
            "SELECT term,doc,col,offset FROM {table} ORDER BY doc,offset,term"
        ))?
        .query_map([], |r| {
            Ok(
                json!({"term":r.get::<_,String>(0)?,"doc":r.get::<_,i64>(1)?,
                                   "column":r.get::<_,String>(2)?,"offset":r.get::<_,i64>(3)?}),
            )
        })?
        .collect::<rusqlite::Result<_>>()?)
}
fn phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}
fn matches(db: &Connection, table: &str, query: &str) -> Result<Vec<i64>> {
    ids(
        db,
        &format!("SELECT rowid FROM {table} WHERE {table} MATCH ?1 ORDER BY rowid"),
        &phrase(query),
    )
}
fn bounded(db: &Connection, case: &Fallback) -> Result<Bounded> {
    if !(1..=64).contains(&case.cap) || !(1..=2).contains(&case.text.chars().count()) {
        return Err("fallback requires 1/2 scalars and cap 1..64".into());
    }
    let count: i64 = db.query_row(
        "SELECT count(*) FROM corpus WHERE bucket=?1",
        [&case.bucket],
        |r| r.get(0),
    )?;
    let mut stmt = db.prepare("SELECT id,text FROM corpus WHERE bucket=?1 ORDER BY id LIMIT ?2")?;
    let mut rows = stmt.query(params![case.bucket, i64::try_from(case.cap)?])?;
    let mut result = Bounded {
        eligible_count: usize::try_from(count)?,
        candidate_ids: vec![],
        hits: vec![],
        compared_bytes: 0,
    };
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let text: String = row.get(1)?;
        result.candidate_ids.push(id);
        result.compared_bytes += text.len();
        if literal(&text, &case.text) {
            result.hits.push(id);
        }
    }
    Ok(result)
}
fn nearest_rank(values: &[f64], p: f64) -> f64 {
    assert!(!values.is_empty() && p > 0.0 && p <= 1.0);
    assert!(values.iter().all(|v| v.is_finite() && *v >= 0.0));
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[(p * values.len() as f64).ceil() as usize - 1]
}
fn timing<T: PartialEq>(
    warm: usize,
    repeats: usize,
    expected: &T,
    mut call: impl FnMut() -> Result<T>,
) -> Result<Value> {
    for _ in 0..warm {
        assert!(call()? == *expected);
    }
    let mut samples = Vec::new();
    for _ in 0..repeats {
        let start = Instant::now();
        let actual = call()?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        // Validation is outside the timer. Every measured return is still checked.
        assert!(actual == *expected);
        samples.push(elapsed);
    }
    Ok(
        json!({"samples_ms":samples,"p50_ms":nearest_rank(&samples,0.5),"p95_ms":nearest_rank(&samples,0.95)}),
    )
}
fn recall(hits: &[i64], expected: &[i64]) -> Option<f64> {
    (!expected.is_empty()).then(|| {
        hits.iter().filter(|id| expected.contains(id)).count() as f64 / expected.len() as f64
    })
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn experiment(repeats: usize, warm: usize) -> Result<Value> {
    if !(1..=100).contains(&repeats) || !(1..=20).contains(&warm) {
        return Err("repeats 1..100; warmups 1..20".into());
    }
    let fixture: Fixture = serde_json::from_str(FIXTURE)?;
    let oracle: Oracle = serde_json::from_str(EXPECTED)?;
    // Only a new private tempfile-owned directory. No path argument, no user library, no migrations.
    let temp = tempfile::Builder::new().prefix("cm-search-53-").tempdir()?;
    let mut db = Connection::open_with_flags(
        temp.path().canonicalize()?.join("experiment.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    setup(&mut db, &fixture)?;
    let compile_options: Vec<String> = db
        .prepare("PRAGMA compile_options")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    assert!(compile_options.iter().any(|o| o == "ENABLE_FTS5"));
    let mut observations = Vec::new();
    let mut sizes = Vec::new();
    for (table, tokenizer) in TOKENIZERS {
        let vocabulary = tokens(&db, &format!("{table}_v"))?;
        // Logical payload, not page-allocation/file delta: excludes duplicate stored content,
        // docsize/config, corpus/bucket index, probes, freelist and SQLite file overhead.
        let data_bytes: i64 = db.query_row(
            &format!("SELECT coalesce(sum(length(block)),0) FROM {table}_data"),
            [],
            |r| r.get(0),
        )?;
        let idx_bytes: i64 = db.query_row(
            &format!(
                "SELECT coalesce(sum(length(segid)+length(term)+length(pgno)),0) FROM {table}_idx"
            ),
            [],
            |r| r.get(0),
        )?;
        let allocated: Vec<Value> = db
            .prepare(
                "SELECT name,sum(pgsize),sum(payload),sum(unused) FROM dbstat
             WHERE name IN (?1,?2) GROUP BY name ORDER BY name",
            )?
            .query_map(
                params![format!("{table}_data"), format!("{table}_idx")],
                |r| {
                    Ok(
                        json!({"object":r.get::<_,String>(0)?,"allocated_bytes":r.get::<_,i64>(1)?,
                          "payload_bytes":r.get::<_,i64>(2)?,"unused_bytes":r.get::<_,i64>(3)?}),
                    )
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(allocated.len(), 2);
        let allocated_bytes: i64 = allocated
            .iter()
            .map(|v| v["allocated_bytes"].as_i64().unwrap())
            .sum();
        sizes.push(
            json!({"tokenizer":table,"data_blob_bytes":data_bytes,"idx_sql_value_bytes":idx_bytes,
                          "logical_index_payload_bytes":data_bytes+idx_bytes,
                          "allocated_index_bytes":allocated_bytes,"dbstat_index_objects":allocated}),
        );
        for q in &oracle.queries {
            let actual_literal: Vec<_> = fixture
                .documents
                .iter()
                .filter(|d| literal(&d.text, &q.text))
                .map(|d| d.id)
                .collect();
            assert_eq!(actual_literal, q.literal, "literal oracle {}", q.name);
            db.execute(&format!("DELETE FROM {table}_probe"), [])?;
            db.execute(
                &format!("INSERT INTO {table}_probe(rowid,text) VALUES(1,?1)"),
                [&q.text],
            )?;
            let query_tokens = tokens(&db, &format!("{table}_qv"))?;
            let candidates: Vec<i64> = db.prepare(&format!(
                "SELECT DISTINCT v.doc FROM {table}_v v JOIN {table}_qv q ON v.term=q.term ORDER BY v.doc"))?
                .query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            let hits = matches(&db, table, &q.text)?;
            let expected = if table == "unicode61" {
                &q.unicode61
            } else {
                &q.trigram
            };
            assert_eq!(&hits, expected, "{table} oracle {}", q.name);
            assert!(hits.iter().all(|id| candidates.contains(id)));
            let elapsed = timing(warm, repeats, &hits, || matches(&db, table, &q.text))?;
            observations.push(json!({"series":"fts","tokenizer":table,"case":q.name,"text":q.text,
                "scalar_count":q.text.chars().count(),"query_tokens":query_tokens,"candidate_ids":candidates,
                "hits":hits,"expected_semantic_hits":expected,"literal_expected":q.literal,
                "literal_recall":recall(&hits,&q.literal),"timing":elapsed}));
        }
        observations.push(json!({"series":"vocabulary","tokenizer":table,"options":tokenizer,"tokens":vocabulary}));
    }
    let all_ids: BTreeSet<_> = fixture.documents.iter().map(|d| d.id).collect();
    for case in &oracle.fallback {
        let eligible = ids(
            &db,
            "SELECT id FROM corpus WHERE bucket=?1 ORDER BY id",
            &case.bucket,
        )?;
        let eligible_set: BTreeSet<_> = eligible.iter().copied().collect();
        let actual_filtered: Vec<_> = fixture
            .documents
            .iter()
            .filter(|d| d.bucket == case.bucket && literal(&d.text, &case.text))
            .map(|d| d.id)
            .collect();
        assert_eq!(
            actual_filtered, case.expected_filtered,
            "filtered oracle {}",
            case.name
        );
        let result = bounded(&db, case)?;
        assert_eq!(
            result.hits, case.expected_searched,
            "bounded oracle {}",
            case.name
        );
        assert_eq!(
            result.candidate_ids,
            eligible.iter().take(case.cap).copied().collect::<Vec<_>>()
        );
        assert!(result.candidate_ids.len() <= case.cap);
        let searched: BTreeSet<_> = result.candidate_ids.iter().copied().collect();
        let unsearched: Vec<_> = eligible_set.difference(&searched).copied().collect();
        let excluded: Vec<_> = all_ids.difference(&eligible_set).copied().collect();
        let elapsed = timing(warm, repeats, &result, || bounded(&db, case))?;
        observations.push(json!({"series":"bounded_literal","case":case.name,"text":case.text,
            "filter":{"bucket_eq":case.bucket},"candidate_cap":case.cap,"result":result,
            "truncated":!unsearched.is_empty(),"status":if unsearched.is_empty(){"complete_within_filter"}else{"partial_unsearched"},
            "unsearched_eligible_ids":unsearched,"excluded_by_filter_ids":excluded,
            "fixture_oracle_filtered_hits":case.expected_filtered,"fixture_oracle_searched_hits":case.expected_searched,
            "fixture_oracle_recall_within_filter":recall(&result.hits,&case.expected_filtered),
            "timing":elapsed}));
    }
    Ok(
        json!({"kind":"search_tokenizer_experiment","fixture_sha256":digest(FIXTURE.as_bytes()),
        "expected_sha256":digest(EXPECTED.as_bytes()),"documents":fixture.documents.len(),
        "sqlite_version":db.query_row("SELECT sqlite_version()",[],|r|r.get::<_,String>(0))?,
        "sqlite_source_id":db.query_row("SELECT sqlite_source_id()",[],|r|r.get::<_,String>(0))?,
        "compile_options":compile_options,"fts5_enabled":true,"tokenizers":TOKENIZERS,
        "database":{"ownership":"new private TempDir; removed on exit","page_size":4096,"journal_mode":"delete","synchronous":"FULL"},
        "literal_rule":fixture.literal_rule,"warmups_per_case":warm,"repetitions_per_case":repeats,
        "cache":"same connection; seed, vocabulary/oracle queries and warmups before timings; OS cache uncontrolled, no cold-cache measurement",
        "fts_timer":"Instant before phrase quoting, prepare, bind, step and ordered rowid collection; excludes setup, token/candidate probes, oracle validation, serialization",
        "fallback_timer":"Instant before validation of cap/length, SQL eligible count, prepare/bind/step ordered capped id+text query, ASCII literal comparison and collected ids/byte count; excludes scope-ID diagnostics/oracle validation/serialization",
        "candidate_definition":"SQL union of documents sharing any query token via fts5vocab instance join; diagnostic superset, not internal FTS query planner candidates; exact MATCH phrase hits reported separately",
        "index_size_definition":"allocated_index_bytes = dbstat sum(pgsize) for exactly FTS _data and _idx B-trees, including their unused page space. Excludes content/docsize/config/corpus/bucket/probes/freelist/file headers. Also report logical_index_payload_bytes = sum length(block) in _data plus sum length(segid)+length(term)+length(pgno) in _idx (numeric SQL length is text representation length). Neither is production index size or total FTS/database storage.",
        "index_sizes":sizes,"quantile":"nearest rank sorted[ceil(p*N)-1], no interpolation or confidence interval",
        "observations":observations}),
    )
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() > 2 {
        return Err(
            "usage: search_experiment [repetitions 1..100] [warmups 1..20]; no DB path accepted"
                .into(),
        );
    }
    let repeats = args.first().map(|s| s.parse()).transpose()?.unwrap_or(31);
    let warm = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(5);
    println!(
        "{}",
        serde_json::to_string_pretty(&experiment(repeats, warm)?)?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_sqlite_hitsets_tokens_and_finite_scope() -> Result<()> {
        let report = experiment(2, 1)?;
        let rows = report["observations"].as_array().unwrap();
        assert_eq!(rows.len(), 69);
        let vocab = rows
            .iter()
            .find(|r| r["series"] == "vocabulary" && r["tokenizer"] == "unicode61")
            .unwrap();
        assert!(
            vocab["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["doc"] == 1 && t["term"] == "固体電解質リチウム")
        );
        let partial = rows.iter().find(|r| r["case"] == "two_cap2").unwrap();
        assert_eq!(partial["result"]["candidate_ids"], json!([1, 2]));
        assert_eq!(partial["status"], "partial_unsearched");
        assert_eq!(partial["fixture_oracle_recall_within_filter"], 0.0);
        let no_hit = rows.iter().find(|r| r["case"] == "absent_partial").unwrap();
        assert_eq!(no_hit["status"], "partial_unsearched");
        assert!(no_hit["fixture_oracle_recall_within_filter"].is_null());
        Ok(())
    }
    #[test]
    fn literal_comparison_and_nearest_rank_are_explicit() {
        assert!(literal("ABC", "abc"));
        for (text, q) in [
            ("ＡＢＣ", "abc"),
            ("é", "e\u{301}"),
            ("Ω", "ω"),
            ("CAFÉ", "café"),
            ("Straße", "strasse"),
        ] {
            assert!(!literal(text, q));
        }
        assert_eq!(nearest_rank(&[9., 1., 7., 3., 5.], 0.5), 5.);
        assert_eq!(
            nearest_rank(&(1..=21).map(f64::from).collect::<Vec<_>>(), 0.95),
            20.
        );
    }
    #[test]
    fn fallback_rejects_unbounded_and_long_queries() -> Result<()> {
        let db = Connection::open_in_memory()?;
        for (cap, text) in [(0, "電"), (65, "電"), (2, "電解質"), (2, "")] {
            let c = Fallback {
                name: "invalid".into(),
                text: text.into(),
                bucket: "materials".into(),
                cap,
                expected_filtered: vec![],
                expected_searched: vec![],
            };
            assert!(bounded(&db, &c).is_err());
        }
        Ok(())
    }
}
