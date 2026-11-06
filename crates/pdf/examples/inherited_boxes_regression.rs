//! Actual worker regression probe. Run before adapter correction with `before`, then `after`.
use cm_domain::PageIndex;
use cm_pdf::{geometry::Rect, worker::*};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(args.len() == 3 && matches!(args[1].as_str(), "before" | "after"));
    let source = Source {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pdf/inherited.pdf"),
        password: None,
    };
    let original = std::fs::read(&source.path).unwrap();
    let worker = PdfWorker::start(std::env::var_os("CM_PDFIUM_LIBRARY").unwrap().into()).unwrap();
    let call = |op| {
        worker
            .submit(op, Cancellation::default(), Duration::from_secs(30))
            .unwrap()
            .wait(Duration::from_secs(30))
    };
    let mut pages = Vec::new();
    for i in 0..2 {
        let page_index = PageIndex::try_from(i).unwrap();
        let page = match call(Operation::Page {
            source: source.clone(),
            page_index,
            zoom: Some(1.),
        }) {
            Ok(Output::Page(p)) => json!({"crop":p.crop,"rotation":p.rotation,
                "bitmap":[p.bitmap.as_ref().unwrap().width,p.bitmap.as_ref().unwrap().height]}),
            Err(e) => json!({"error":format!("{e:?}")}),
            _ => panic!(),
        };
        let export = |bounds: Rect| match call(Operation::Export {
            source: source.clone(),
            highlights: vec![Highlight {
                page_index,
                quads: vec![bounds.quad()],
                comment: "regression".into(),
            }],
            notes: vec![],
        }) {
            Ok(Output::Export(e)) => json!({"bytes":e.bytes.len()}),
            Err(e) => json!({"error":format!("{e:?}")}),
            _ => panic!(),
        };
        let inside = export(Rect {
            left: 45.,
            bottom: 400.,
            right: 200.,
            top: 425.,
        });
        // Inside MediaBox, outside the effective inherited CropBox.
        let outside = export(Rect {
            left: 5.,
            bottom: 40.,
            right: 15.,
            top: 50.,
        });
        if args[1] == "before" {
            if i == 0 {
                assert_eq!(page["error"], "NativeFailure");
                assert_eq!(inside["error"], "NativeFailure");
            } else {
                assert_eq!(page["bitmap"], json!([500, 400]));
                assert_eq!(
                    page["crop"],
                    json!({"left":0.,"bottom":0.,"right":400.,"top":500.})
                );
                assert!(inside["bytes"].as_u64().unwrap() > 0);
                assert!(outside["bytes"].as_u64().unwrap() > 0);
            }
        } else {
            assert_eq!(page["bitmap"], json!([440, 360]));
            assert_eq!(page["rotation"], "R90");
            assert_eq!(
                page["crop"],
                json!({"left":20.,"bottom":30.,"right":380.,"top":470.})
            );
            assert!(inside["bytes"].as_u64().unwrap() > 0);
            assert_eq!(outside["error"], "InvalidGeometry");
        }
        pages.push(json!({"page_index":i,"page":page,"inside_export":inside,"outside_crop_export":outside}));
    }
    assert_eq!(std::fs::read(&source.path).unwrap(), original);
    worker.shutdown().unwrap();
    std::fs::write(
        &args[2],
        serde_json::to_vec_pretty(&json!({"phase":args[1],
        "library_sha256":PDFIUM_SHA256,"original_sha256":format!("{:x}",Sha256::digest(original)),
        "original_unchanged":true,"pages":pages}))
        .unwrap(),
    )
    .unwrap();
}
