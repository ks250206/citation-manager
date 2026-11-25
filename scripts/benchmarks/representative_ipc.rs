//! #30 representative workload through the real authenticated IPC of a running `papersd`
//! (the same transport and `Session` the GUI/CLI use). Driven by `representative.py`.
//!
//! `warm-start PAPERSD LIBRARY RUNTIME REPEATS IDLE_MS`: each repeat lazily starts the
//! daemon exactly like the GUI/CLI connector (`Launch`), opens the authenticated session,
//! reads the first 100-row library page and records the daemon's RSS; then disconnects and
//! waits for the idle exit before the next repeat.
//! `queries LIBRARY RUNTIME ORACLE SEED SAMPLES`: collection/tag switches, metadata keyword,
//! full-text queries (FTS and Japanese short terms) and an exact recall check of page hits
//! against the generator's literal oracle. Raw samples only; quantiles are computed by
//! the Python runner (nearest rank).
use cm_daemon::lifecycle::{DaemonPaths, euid, local_connector};
use cm_protocol::{
    client::{Session, local::Launch},
    domain::{AttachmentId, CollectionId, PaperId, TagId},
    ipc::{Command, Reply},
    library_query::{LibraryFilter, LibraryQuery, TagMatch},
    search::{DocumentSource, SearchCoverage, SearchReply, SearchRequest, ShortTermScope},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Must match representative_seed.rs.
const SEED: usize = 30;
const COLLECTIONS: usize = 10;
const TAGS: usize = 8;
/// The GUI library list requests 100 rows; the GUI search panel 50 hits.
const LIST_LIMIT: u32 = 100;
const SEARCH_LIMIT: u32 = 50;

fn id(kind: u128, index: usize) -> Uuid {
    Uuid::from_u128((kind << 96) | (index as u128 + 1))
}
fn paper_index(paper: PaperId) -> usize {
    (paper.0.as_u128() & 0xffff_ffff) as usize - 1
}
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Resident set size in KiB via `ps` (same tool on Linux and macOS).
pub fn rss_kib(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}
fn alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| {
            let stat = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            !stat.is_empty() && !stat.starts_with('Z')
        })
        .unwrap_or(false)
}

fn paths(library: &Path, runtime: &Path) -> Result<DaemonPaths> {
    Ok(DaemonPaths::new(library, Some(runtime), euid())?)
}

fn call(session: &mut Session, command: Command) -> Result<Reply> {
    match session.call(command)? {
        Ok(reply) => Ok(reply),
        Err(e) => Err(format!("daemon error: {e:?}").into()),
    }
}
fn page(session: &mut Session, filter: LibraryFilter) -> Result<Vec<PaperId>> {
    match call(
        session,
        Command::Query(LibraryQuery {
            filter,
            limit: LIST_LIMIT,
            ..Default::default()
        }),
    )? {
        Reply::Page(page) => Ok(page.papers.iter().map(|p| p.id).collect()),
        other => Err(format!("unexpected {other:?}").into()),
    }
}
fn search(
    session: &mut Session,
    request: &SearchRequest,
) -> Result<Box<cm_protocol::search::SearchPage>> {
    match call(session, Command::Search(Box::new(request.clone())))? {
        Reply::Search(SearchReply::Results(page)) => Ok(page),
        other => Err(format!("unexpected {other:?}").into()),
    }
}

fn warm_start(
    papersd: &Path,
    library: &Path,
    runtime: &Path,
    repeats: usize,
    idle_ms: u64,
) -> Result<Value> {
    let mut runs = Vec::new();
    for repeat in 0..repeats {
        let mut connector = local_connector(paths(library, runtime)?, "desktop");
        connector.launch = Some(Launch {
            papersd: papersd.to_path_buf(),
            extra_args: vec!["--idle-timeout-ms".into(), idle_ms.to_string().into()],
            timeout: Duration::from_secs(60),
        });
        let start = Instant::now();
        let mut session = Session::open(&connector)?;
        let connected_ms = ms(start);
        let pid = match call(&mut session, Command::Status)? {
            Reply::Status(status) => status.pid,
            other => return Err(format!("unexpected {other:?}").into()),
        };
        let rows = page(&mut session, LibraryFilter::default())?.len();
        let first_page_ms = ms(start);
        if rows != LIST_LIMIT as usize {
            return Err(format!("first page returned {rows}").into());
        }
        let rss = rss_kib(pid);
        drop(session);
        let exit_start = Instant::now();
        while alive(pid) {
            if exit_start.elapsed() > Duration::from_secs(120) {
                return Err("daemon did not exit after idle timeout".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        runs.push(
            json!({"repeat": repeat, "pid": pid, "connected_ms": connected_ms,
            "first_page_ms": first_page_ms, "rows": rows, "rss_kib_after_first_page": rss,
            "idle_exit_wait_ms": ms(exit_start)}),
        );
    }
    Ok(
        json!({"kind": "warm_start", "idle_ms": idle_ms, "runs": runs,
        "method": "lazy start through the GUI/CLI LocalConnector Launch path: spawn papersd, wait for its socket, authenticated handshake, Status, first 100-row library page; repeat 0 follows seeding/indexing, later repeats have a warm OS cache"}),
    )
}

struct Oracle {
    /// term -> expected (attachment, page) pairs.
    terms: BTreeMap<String, BTreeSet<(Uuid, u32)>>,
}
fn load_oracle(oracle: &Path, seed: &Path) -> Result<Oracle> {
    let oracle: Value = serde_json::from_str(&std::fs::read_to_string(oracle)?)?;
    let seed: Value = serde_json::from_str(&std::fs::read_to_string(seed)?)?;
    let attachments: Vec<Uuid> = seed["attachments"]
        .as_array()
        .ok_or("seed attachments")?
        .iter()
        .map(|v| Uuid::parse_str(v.as_str().unwrap_or_default()))
        .collect::<std::result::Result<_, _>>()?;
    let mut terms = BTreeMap::new();
    for (term, entry) in oracle["terms"].as_object().ok_or("oracle terms")? {
        let mut set = BTreeSet::new();
        for pair in entry["hits"].as_array().ok_or("oracle hits")? {
            let pdf = pair[0].as_u64().ok_or("pdf")? as usize;
            let page = pair[1].as_u64().ok_or("page")? as u32;
            set.insert((*attachments.get(pdf).ok_or("pdf index")?, page));
        }
        terms.insert(term.clone(), set);
    }
    Ok(Oracle { terms })
}

/// Every page hit of a request, following result pages and (for short terms with
/// `AllPages`) every continuation window until the scan is complete.
fn all_page_hits(
    session: &mut Session,
    base: &SearchRequest,
) -> Result<(BTreeSet<(Uuid, u32)>, usize, usize, Value)> {
    let mut request = base.clone();
    request.limit = 100;
    let mut found = BTreeSet::new();
    let mut other = 0;
    let mut windows = 0;
    let mut coverage: Option<SearchCoverage>;
    loop {
        windows += 1;
        loop {
            let page = search(session, &request)?;
            coverage = Some(page.coverage.clone());
            for hit in &page.hits {
                match (hit.source, hit.attachment_id, hit.page_index) {
                    (DocumentSource::Page, Some(AttachmentId(a)), Some(p)) => {
                        found.insert((a, p));
                    }
                    _ => other += 1,
                }
            }
            match page.next_page {
                Some(next) => request.cursor = Some(next),
                None => break,
            }
        }
        match coverage.as_ref().and_then(|c| c.continuation.clone()) {
            Some(next) => request.cursor = Some(next),
            None => break,
        }
        if windows > 1000 {
            return Err("unbounded continuation".into());
        }
    }
    let coverage = coverage.ok_or("no page")?;
    Ok((
        found,
        other,
        windows,
        json!({
            "complete": coverage.complete, "fts_truncated": coverage.fts_truncated,
            "timed_out": coverage.timed_out, "short_terms": coverage.short_terms,
            "unsearched": format!("{:?}", coverage.unsearched),
        }),
    ))
}

fn queries(
    library: &Path,
    runtime: &Path,
    oracle: &Path,
    seed: &Path,
    samples: usize,
) -> Result<Value> {
    let oracle = load_oracle(oracle, seed)?;
    // The runner has just spawned papersd: retry until it listens (bounded).
    let connector = local_connector(paths(library, runtime)?, "desktop");
    let opened = Instant::now();
    let mut session = loop {
        match Session::open(&connector) {
            Ok(session) => break session,
            Err(e) if opened.elapsed() > Duration::from_secs(60) => return Err(e.into()),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let pid = match call(&mut session, Command::Status)? {
        Reply::Status(status) => status.pid,
        other => return Err(format!("unexpected {other:?}").into()),
    };
    let rss_before = rss_kib(pid);
    let mut rows = Vec::new();
    let mut record = |case: &str, sample: usize, elapsed: f64, returned: usize, extra: Value| {
        rows.push(json!({"case": case, "sample": sample, "elapsed_ms": elapsed, "returned": returned, "detail": extra}));
    };
    // Warm-up (untimed): one call per case so the first timed sample is not a cold path.
    page(&mut session, LibraryFilter::default())?;
    // 1. Collection switch: cycle through all collections, first 100 rows each.
    for sample in 0..samples {
        for c in 0..COLLECTIONS {
            let filter = LibraryFilter {
                collection: Some(CollectionId(id(2, c))),
                ..Default::default()
            };
            let start = Instant::now();
            let ids = page(&mut session, filter)?;
            let elapsed = ms(start);
            let n = SEED;
            if ids.is_empty()
                || ids.iter().any(|p| {
                    let i = paper_index(*p) + n;
                    i % COLLECTIONS != c && (i % COLLECTIONS + 1) % COLLECTIONS != c
                })
            {
                return Err(format!("collection {c} page mismatch").into());
            }
            record(
                "collection_switch",
                sample,
                elapsed,
                ids.len(),
                json!({"collection": c}),
            );
        }
        // Tag switch (single tag, then two tags AND).
        for t in 0..TAGS {
            let filter = LibraryFilter {
                tags: vec![TagId(id(3, t)), TagId(id(3, (t + 1) % TAGS))],
                tag_match: TagMatch::All,
                ..Default::default()
            };
            let start = Instant::now();
            let ids = page(&mut session, filter)?;
            record(
                "tag_and_switch",
                sample,
                ms(start),
                ids.len(),
                json!({"tag": t}),
            );
        }
        // 2. Metadata keyword (the library list's literal keyword filter).
        let start = Instant::now();
        let ids = page(
            &mut session,
            LibraryFilter {
                keyword: Some("needle".into()),
                ..Default::default()
            },
        )?;
        let elapsed = ms(start);
        if ids.is_empty()
            || ids
                .iter()
                .any(|p| !(paper_index(*p) + SEED).is_multiple_of(7))
        {
            return Err("metadata keyword mismatch".into());
        }
        record("metadata_keyword", sample, elapsed, ids.len(), json!({}));
        let start = Instant::now();
        let ids = page(
            &mut session,
            LibraryFilter {
                keyword: Some("absent-sentinel-30".into()),
                ..Default::default()
            },
        )?;
        record("metadata_no_hit", sample, ms(start), ids.len(), json!({}));
        let start = Instant::now();
        let ids = page(
            &mut session,
            LibraryFilter {
                collection: Some(CollectionId(id(2, 0))),
                tags: vec![TagId(id(3, 0))],
                keyword: Some("needle".into()),
                ..Default::default()
            },
        )?;
        record("metadata_combined", sample, ms(start), ids.len(), json!({}));
        // 3. Search box (FTS DSL) for a bibliographic word, then page-text terms.
        let mut cases: Vec<(String, SearchRequest)> = vec![(
            "fts_bibliographic:needle".into(),
            SearchRequest::new("needle"),
        )];
        for term in oracle.terms.keys() {
            cases.push((
                format!("fulltext_default:{term}"),
                SearchRequest::new(term.as_str()),
            ));
            let mut scoped = SearchRequest::new(term.as_str());
            scoped.collection = Some(CollectionId(id(2, 0)));
            cases.push((format!("fulltext_in_collection:{term}"), scoped));
        }
        for (case, mut request) in cases {
            request.limit = SEARCH_LIMIT;
            if sample == 0 {
                search(&mut session, &request)?; // untimed warm-up
            }
            let start = Instant::now();
            let page = search(&mut session, &request)?;
            let elapsed = ms(start);
            record(
                &case,
                sample,
                elapsed,
                page.hits.len(),
                json!({
                    "total_hits": page.total_hits, "complete": page.coverage.complete,
                    "timed_out": page.coverage.timed_out, "fts_truncated": page.coverage.fts_truncated,
                    "short_terms": page.coverage.short_terms, "scanned_documents": page.coverage.scanned_documents,
                    "unsearched": format!("{:?}", page.coverage.unsearched),
                }),
            );
        }
    }
    // 4. Exact recall of page hits vs the literal oracle (default and complete scans).
    let mut recall = Vec::new();
    for (term, expected) in &oracle.terms {
        for scope in [ShortTermScope::Bounded, ShortTermScope::AllPages] {
            let mut request = SearchRequest::new(term.as_str());
            request.short_term_scope = scope;
            request.timeout_ms = 10_000;
            let start = Instant::now();
            let (found, other, windows, coverage) = all_page_hits(&mut session, &request)?;
            let elapsed = ms(start);
            let hit = expected.intersection(&found).count();
            recall.push(json!({
                "term": term, "scalars": term.chars().count(),
                "scope": format!("{scope:?}"), "expected_pages": expected.len(),
                "found_pages": found.len(), "true_positive_pages": hit,
                "false_positive_pages": found.difference(expected).count(),
                "recall": if expected.is_empty() { 1.0 } else { hit as f64 / expected.len() as f64 },
                "non_page_hits": other, "windows": windows, "total_ms": elapsed,
                "final_coverage": coverage,
            }));
        }
    }
    let rss_after = rss_kib(pid);
    Ok(json!({"kind": "queries", "pid": pid, "samples": samples,
        "list_limit": LIST_LIMIT, "search_limit": SEARCH_LIMIT,
        "rss_kib_before": rss_before, "rss_kib_after": rss_after,
        "rows": rows, "recall": recall,
        "method": "one authenticated desktop IPC session; each timed call is a full request/response round trip (frame encode, auth check, service, decode)"}))
}

fn main() -> Result<()> {
    if cfg!(debug_assertions) {
        return Err("benchmark executable requires --release".into());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let p = |s: &String| PathBuf::from(s);
    let report = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["warm-start", _, _, _, repeats, idle] => warm_start(
            &p(&args[1]),
            &p(&args[2]),
            &p(&args[3]),
            repeats.parse()?,
            idle.parse()?,
        )?,
        ["queries", _, _, _, _, samples] => queries(
            &p(&args[1]),
            &p(&args[2]),
            &p(&args[3]),
            &p(&args[4]),
            samples.parse()?,
        )?,
        _ => return Err("usage: warm-start PAPERSD LIB RUNTIME REPEATS IDLE_MS | queries LIB RUNTIME ORACLE SEED SAMPLES".into()),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_and_oracle_mapping_match_the_seed_layout() -> Result<()> {
        assert_eq!(paper_index(PaperId(id(1, 41))), 41);
        let temp = tempfile::tempdir()?;
        let oracle = temp.path().join("oracle.json");
        let seed = temp.path().join("seed.json");
        let a = Uuid::now_v7();
        std::fs::write(&oracle, r#"{"terms":{"電池":{"hits":[[0,3],[0,5]]}}}"#)?;
        std::fs::write(&seed, format!(r#"{{"attachments":["{a}"]}}"#))?;
        let loaded = load_oracle(&oracle, &seed)?;
        assert_eq!(loaded.terms["電池"], BTreeSet::from([(a, 3), (a, 5)]));
        assert!(rss_kib(std::process::id()).is_some_and(|k| k > 0));
        assert!(alive(std::process::id()));
        Ok(())
    }
}
