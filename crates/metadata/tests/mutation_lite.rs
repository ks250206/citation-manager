//! #30 deterministic mutation regression ("fuzz-lite", no new dependency): every committed
//! BibTeX/RIS/HTML fixture is mutated with a seeded generator (byte flips, deletions,
//! insertions, duplications, truncations, hostile splices) and fed to the real parsers.
//! They must return (a report or a bounded error) without panicking, and HTML capture
//! must only offer PDF candidates that pass the fetch policy's URL check. Coverage-guided
//! fuzzing (cargo-fuzz) is not introduced here; see docs/validation/ISSUE_30.md.
use cm_core::capture::CaptureRequest;
use cm_metadata::{bibtex, fetch_policy::FetchPolicy, html, ris};
use std::path::Path;

/// xorshift64*: reproducible across platforms; the seed is printed on failure.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const SPLICES: &[&[u8]] = &[
    b"\0",
    b"\xff\xfe",
    b"{{{{{{{{{{{{{{{{{{{{",
    b"}}}}",
    b"@article{",
    b"\"",
    b"\\",
    b"%",
    b"TY  - ",
    b"ER  - ",
    b"<script>alert(1)</script>",
    b"<meta name=\"citation_pdf_url\" content=\"http://127.0.0.1/x.pdf\">",
    b"<meta name=\"citation_pdf_url\" content=\"https://user:pw@evil.example/x.pdf\">",
    b"<meta name=\"citation_pdf_url\" content=\"file:///etc/passwd\">",
    b"<script type=\"application/ld+json\">{\"@type\":\"ScholarlyArticle\",\"name\":[[[[",
    b"\xe6\x97\xa5\xe6\x9c\xac",
    b"\xed\xa0\x80",
];

fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut out = input.to_vec();
    for _ in 0..1 + rng.below(6) {
        let at = rng.below(out.len() + 1);
        match rng.below(6) {
            0 if !out.is_empty() => {
                let i = rng.below(out.len());
                out[i] = rng.next() as u8;
            }
            1 if !out.is_empty() => {
                let end = (at + 1 + rng.below(64)).min(out.len());
                out.drain(at.min(out.len())..end.max(at.min(out.len())));
            }
            2 => out.insert(at, rng.next() as u8),
            3 if !out.is_empty() => {
                let start = rng.below(out.len());
                let end = (start + 1 + rng.below(256)).min(out.len());
                let copy = out[start..end].to_vec();
                out.splice(at..at, copy);
            }
            4 => out.truncate(at),
            _ => {
                let splice = SPLICES[rng.below(SPLICES.len())];
                out.splice(at..at, splice.iter().copied());
            }
        }
    }
    out
}

fn fixtures(dir: &str) -> Vec<(String, Vec<u8>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(dir);
    let mut files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e != "md"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}

const ROUNDS: u64 = 300;
/// Large fixtures (the 1,000-record exchange files) get fewer rounds to bound CI time.
fn rounds(bytes: &[u8]) -> u64 {
    if bytes.len() > 32 * 1024 { 12 } else { ROUNDS }
}

#[test]
fn bibtex_and_ris_parsers_survive_seeded_mutations() {
    for (dir, parse) in [
        (
            "bibtex",
            bibtex::parse as fn(&[u8]) -> Result<_, cm_core::ServiceError>,
        ),
        ("ris", ris::parse),
    ] {
        for (name, bytes) in fixtures(dir) {
            for round in 0..rounds(&bytes) {
                let seed = 0x30_0000 + round;
                let input = mutate(&mut Rng(seed), &bytes);
                let result = std::panic::catch_unwind(|| parse(&input));
                assert!(result.is_ok(), "{dir}/{name} panicked with seed {seed}");
            }
        }
    }
}

#[test]
fn html_capture_survives_seeded_mutations_and_never_offers_unsafe_pdfs() {
    let policy = FetchPolicy::default();
    for (name, bytes) in fixtures("html") {
        for round in 0..rounds(&bytes) {
            let seed = 0x31_0000 + round;
            let input = mutate(&mut Rng(seed), &bytes);
            let request = CaptureRequest {
                page_url: "https://journal.example.org/article/1".into(),
                html: Some(String::from_utf8_lossy(&input).into_owned()),
                ..Default::default()
            };
            let page = std::panic::catch_unwind(|| html::extract(&request, &policy))
                .unwrap_or_else(|_| panic!("html/{name} panicked with seed {seed}"));
            // Every offered candidate passes the same syntactic policy the fetch job
            // enforces (HTTPS, public host, no credentials); nothing else slips through.
            for candidate in &page.pdf_candidates {
                let url = candidate.url.as_str();
                assert!(
                    url.starts_with("https://") && policy.check_url(url).is_ok(),
                    "html/{name} seed {seed}: unsafe candidate {url}"
                );
            }
        }
    }
}
