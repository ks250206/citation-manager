#[path = "../tests/support/mod.rs"]
mod support;
fn main() {
    let library = std::env::var_os("CM_PDFIUM_LIBRARY")
        .expect("CM_PDFIUM_LIBRARY must be the absolute pinned dylib");
    let out = std::env::args_os()
        .nth(1)
        .expect("output directory argument required");
    if let Some(input) = std::env::args_os().nth(2) {
        let expected: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::env::args_os().nth(3).expect("expected report")).unwrap(),
        )
        .unwrap();
        inspect_final(
            library.into(),
            &std::path::PathBuf::from(input).canonicalize().unwrap(),
            std::path::Path::new(&out),
            &expected,
        );
        println!("PDFium independently re-opened portable export");
        return;
    }
    let report = support::run(library.into(), std::path::Path::new(&out));
    std::fs::write(
        std::path::Path::new(&out).join("pdfium-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!(
        "PDFium runtime corpus/export/status assertions passed; report in {}",
        std::path::Path::new(&out).display()
    );
}

use cm_pdf::worker::*;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn inspect_final(library: PathBuf, input: &Path, out: &Path, expected: &Value) {
    fs::create_dir_all(out).unwrap();
    let w = PdfWorker::start(library).unwrap();
    let mut results = Vec::new();
    for i in 0..expected["pages"].as_array().unwrap().len() as u32 {
        let p = support::page(&w, input, i, Some(1.));
        assert_eq!(p.annotations.len(), 2);
        assert!(p.text.contains("日本語"));
        let expected = &expected["exported_pages"][i as usize];
        let actual = serde_json::to_value(&p).unwrap();
        compare_json(&actual["annotations"], &expected["annotations"]);
        support::ppm(
            &out.join(format!("page-{i}.ppm")),
            p.bitmap.as_ref().unwrap(),
        );
        results.push(actual);
    }
    fs::write(
        out.join("report.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
    w.shutdown().unwrap();
}

fn compare_json(a: &Value, b: &Value) {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            assert!((a.as_f64().unwrap() - b.as_f64().unwrap()).abs() < 0.01)
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                compare_json(a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.len(), b.len());
            for (k, a) in a {
                compare_json(a, &b[k]);
            }
        }
        _ => assert_eq!(a, b),
    }
}
