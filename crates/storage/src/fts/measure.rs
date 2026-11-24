//! #18 small-scale regression measurement (runs in CI as an ordinary test). Synthetic
//! Japanese/English corpus with a literal oracle: expected hits/recall for 1, 2 and 3+
//! scalar terms, bounded-scope behaviour, candidate-window continuation, index size and
//! warm p50/p95 for the FTS and short-term fallback series separately. The 10k papers /
//! 20k pages load of DESIGN §16 is #30; nothing here extrapolates to it.
//! `cargo test -p cm-storage --lib fts::measure -- --nocapture` prints the JSON summary.
use crate::LibraryDb;
use cm_core::{
    papers::NewPaper,
    papers::PaperLibrary,
    search::{search, types::*},
};
use rusqlite::params;
use std::{collections::BTreeSet, time::Instant};
use uuid::Uuid;

const PAPERS: usize = 120;
const PAGES_PER_PAPER: usize = 3;
const RUNS: usize = 31;

struct Corpus {
    /// (document id, searchable text) for the literal oracle.
    docs: Vec<(i64, String)>,
}

fn build(db: &mut LibraryDb) -> (Corpus, f64) {
    let mut insert_ms = 0.0;
    for i in 0..PAPERS {
        let title = match i % 4 {
            0 => format!("固体電解質の界面 {i}"),
            1 => format!("Lithium metal anodes {i}"),
            2 => format!("全固体電池の設計 {i}"),
            _ => format!("Solid-state interfaces {i}"),
        };
        let mut input = NewPaper::titled(title);
        input.bibliography.abstract_text = Some(if i % 5 == 0 {
            "Li dendrite growth".into()
        } else {
            "界面抵抗の評価".into()
        });
        let paper = db.papers().create(input).unwrap().id;
        let attachment = Uuid::now_v7().to_string();
        let mut hash = [0u8; 32];
        hash[..8].copy_from_slice(&(i as u64).to_le_bytes());
        db.connection
            .execute(
                "INSERT INTO attachments(id,paper_id,kind,file_hash,relative_path,original_name,mime_type,byte_size,added_at)
                 VALUES (?1,?2,'pdf',?3,'attachments/00/00/x.pdf','x.pdf','application/pdf',1,'now')",
                params![attachment, paper.0.to_string(), hash.as_slice()],
            )
            .unwrap();
        for p in 0..PAGES_PER_PAPER {
            let body = match (i + p) % 6 {
                0 => "本文では電池の劣化と電解液を議論する。",
                1 => "The lithium plating rate depends on current density.",
                2 => "電気化学インピーダンスの測定結果。",
                3 => "Interphase chemistry of the anode.",
                4 => "固体電解質と電極の界面。",
                _ => "Figures only.",
            };
            let started = Instant::now();
            db.connection
                .execute(
                    "INSERT INTO fulltext_pages(attachment_id,page_index,page_label,text,extraction_status,updated_at)
                     VALUES (?1,?2,?3,?4,'complete','now')",
                    params![attachment, p as i64, format!("{}", p + 1), body],
                )
                .unwrap();
            insert_ms += started.elapsed().as_secs_f64() * 1000.0;
        }
        db.connection
            .execute(
                "INSERT INTO fulltext_extractions(attachment_id,file_hash,status,page_count,next_page,pages_with_text,updated_at)
                 VALUES (?1,?2,'complete',?3,?3,?3,'now')",
                params![attachment, hash.as_slice(), PAGES_PER_PAPER as i64],
            )
            .unwrap();
    }
    let docs = db
        .connection
        .prepare("SELECT id, coalesce(title,'')||char(10)||coalesce(authors,'')||char(10)||coalesce(abstract,'')||char(10)||coalesce(journal,'')||char(10)||coalesce(tags,'')||char(10)||coalesce(identifiers,'')||char(10)||coalesce(notes,'')||char(10)||coalesce(body,'') FROM search_documents")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    (Corpus { docs }, insert_ms)
}

impl Corpus {
    fn expected(&self, needle: &str, sources: Option<&BTreeSet<i64>>) -> BTreeSet<i64> {
        let needle = needle.to_ascii_lowercase();
        self.docs
            .iter()
            .filter(|(id, text)| {
                sources.is_none_or(|s| s.contains(id))
                    && text.to_ascii_lowercase().contains(&needle)
            })
            .map(|(id, _)| *id)
            .collect()
    }
}

fn ids(db: &LibraryDb, request: &SearchRequest) -> (BTreeSet<i64>, SearchCoverage) {
    // Collect every hit document of one window (all result pages).
    let mut request = request.clone();
    request.limit = 100;
    let mut found = BTreeSet::new();
    let mut coverage;
    loop {
        let SearchReply::Results(page) = search(&db.search_index(), &request).unwrap() else {
            panic!("invalid")
        };
        coverage = page.coverage.clone();
        for hit in &page.hits {
            found.insert(doc_of(db, hit));
        }
        match page.next_page {
            Some(next) => request.cursor = Some(next),
            None => break,
        }
    }
    (found, coverage)
}
fn doc_of(db: &LibraryDb, hit: &SearchHit) -> i64 {
    let (source, key) = match hit.source {
        DocumentSource::Paper => ("paper", hit.paper_id.0.to_string()),
        DocumentSource::Annotation => ("annotation", hit.annotation_id.clone().unwrap()),
        DocumentSource::Page => {
            let key: i64 = db
                .connection
                .query_row(
                    "SELECT id FROM fulltext_pages WHERE attachment_id=?1 AND page_index=?2",
                    params![
                        hit.attachment_id.unwrap().0.to_string(),
                        hit.page_index.unwrap()
                    ],
                    |r| r.get(0),
                )
                .unwrap();
            ("page", key.to_string())
        }
    };
    db.connection
        .query_row(
            "SELECT id FROM search_documents WHERE source=?1 AND source_key=?2",
            [source, &key],
            |r| r.get(0),
        )
        .unwrap()
}
fn timed(db: &LibraryDb, request: &SearchRequest) -> (f64, f64) {
    for _ in 0..5 {
        search(&db.search_index(), request).unwrap();
    }
    let mut samples: Vec<f64> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            search(&db.search_index(), request).unwrap();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    // Nearest rank (ranks 16 and 30 of 31), as in #53.
    (samples[15], samples[29])
}

#[test]
fn small_scale_recall_scope_window_size_and_latency() {
    let mut db = LibraryDb::in_memory().unwrap();
    let (corpus, insert_ms) = build(&mut db);
    let bibliographic: BTreeSet<i64> = db
        .connection
        .prepare("SELECT id FROM search_documents WHERE source<>'page'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut rows = Vec::new();
    for (term, series) in [
        ("電", "short"),
        ("電池", "short"),
        ("Li", "short"),
        ("固体電", "fts"),
        ("電解質", "fts"),
        ("lithium", "fts"),
        ("\"current density\"", "fts"),
    ] {
        let literal = term.trim_matches('"');
        // Complete scan (AllPages, one window covers the corpus): exact recall vs oracle.
        let mut all = SearchRequest::new(term);
        all.short_term_scope = ShortTermScope::AllPages;
        all.candidate_limit = MAX_CANDIDATE_LIMIT;
        let expected = corpus.expected(literal, None);
        let (found, coverage) = ids(&db, &all);
        let recall = expected.intersection(&found).count() as f64 / expected.len().max(1) as f64;
        let extra = found.difference(&expected).count();
        assert!(coverage.complete, "{term}: {coverage:?}");
        assert_eq!(recall, 1.0, "{term}");
        assert_eq!(extra, 0, "{term}: literal oracle precision");
        // Default bounded scope: short terms search bibliographic docs only and say so.
        let (bounded, bounded_coverage) = ids(&db, &SearchRequest::new(term));
        if series == "short" {
            let expected_bib = corpus.expected(literal, Some(&bibliographic));
            assert!(expected_bib.is_subset(&bounded), "{term}");
            assert!(
                bounded_coverage
                    .unsearched
                    .contains(&UnsearchedScope::PdfPagesForShortTerms)
            );
        } else {
            assert_eq!(bounded, found, "{term}: FTS needs no scan scope");
        }
        let (p50, p95) = timed(&db, &all);
        rows.push(format!(
            "{{\"term\":{term:?},\"scalars\":{},\"series\":\"{series}\",\"expected\":{},\"found\":{},\"recall\":{recall},\"bounded_found\":{},\"p50_ms\":{p50:.4},\"p95_ms\":{p95:.4}}}",
            literal.chars().count(),
            expected.len(),
            found.len(),
            bounded.len(),
        ));
    }
    // Candidate window: 100 documents per call, continuation until complete; union = oracle.
    let mut windowed = SearchRequest::new("電池");
    windowed.short_term_scope = ShortTermScope::AllPages;
    windowed.candidate_limit = 100;
    let mut union = BTreeSet::new();
    let mut windows = 0;
    loop {
        let (found, coverage) = ids(&db, &windowed);
        windows += 1;
        assert!(union.is_disjoint(&found), "windows never repeat a hit");
        union.extend(found);
        match coverage.continuation {
            Some(next) => windowed.cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(union, corpus.expected("電池", None));
    let documents = corpus.docs.len();
    assert!(windows >= documents / 100);
    let sizes = db.search_index().index_bytes().unwrap();
    let total: u64 = sizes.iter().map(|(_, b)| b).sum();
    assert!(total > 0);
    println!(
        "{{\"papers\":{PAPERS},\"documents\":{documents},\"page_insert_ms_total\":{insert_ms:.3},\"windows_at_100\":{windows},\"index_bytes_total\":{total},\"index_bytes\":{},\"queries\":[{}]}}",
        sizes
            .iter()
            .map(|(n, b)| format!("{{\"{n}\":{b}}}"))
            .collect::<Vec<_>>()
            .join(","),
        rows.join(",")
    );
}
