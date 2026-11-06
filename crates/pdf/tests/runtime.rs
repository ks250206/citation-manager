mod support;
#[test]
fn pinned_pdfium_runtime_corpus() {
    let library = std::env::var_os("CM_PDFIUM_LIBRARY")
        .expect("runtime test requires pinned dylib; run python3 scripts/pdfium/fetch.py first");
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/pdfium-test-{}", std::process::id()));
    support::run(library.into(), &out);
}
