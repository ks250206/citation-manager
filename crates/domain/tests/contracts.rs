use cm_domain::*;
use serde_json::{from_value, json, to_value};

#[test]
fn page_boundaries_and_wire_values_cannot_bypass_validation() {
    assert!(PhysicalPageNumber::try_from(0).is_err());
    assert!(PageIndex::try_from(u32::MAX).is_err());
    assert!(from_value::<PhysicalPageNumber>(json!(0)).is_err());
    assert!(from_value::<PageIndex>(json!(u32::MAX)).is_err());
    assert!(from_value::<PageIndex>(json!(-1)).is_err());
    for raw in [0, 1, 998, u32::MAX - 1] {
        let index = PageIndex::try_from(raw).unwrap();
        assert_eq!(u32::from(index.physical_display()), raw + 1);
        assert_eq!(index.physical_display().index(), index);
        assert_eq!(to_value(index).unwrap(), json!(raw));
    }
}

#[test]
fn paper_and_attachment_roundtrip_preserves_internal_and_external_identity() {
    let paper = json!({"id":"019a5665-18c0-7000-8000-000000000001", "title":"論文",
        "external_ids":[{"namespace":"doi","value":"10.1234/example"},
                        {"namespace":"arxiv","value":"2601.12345v2"}]});
    let p: Paper = from_value(paper.clone()).unwrap();
    assert_eq!(to_value(p).unwrap(), paper);
    let attachment = json!({"id":"019a5665-18c0-7000-8000-000000000002",
        "paper_id": paper["id"], "storage_key":"ab/cd/hash.pdf", "sha256":vec![7;32],
        "original_name":"資料.pdf", "media_type":"application/pdf", "byte_size":123});
    assert_eq!(
        to_value(from_value::<Attachment>(attachment.clone()).unwrap()).unwrap(),
        attachment
    );
}

#[test]
fn page_label_is_not_a_physical_display_number_and_unresolved_ai_is_retained() {
    let resolved = json!({"state":"resolved", "quote":"text", "page":{
        "attachment_id":"019a5665-18c0-7000-8000-000000000002", "page_index":4,"page_label":"iv"}});
    let anchor: Anchor = from_value(resolved.clone()).unwrap();
    assert_eq!(to_value(anchor).unwrap(), resolved);
    let unresolved = json!({"state":"unresolved", "attachment_id":resolved["page"]["attachment_id"],
        "quote":"unverified quote", "reason":"not found in replacement PDF"});
    let saved = to_value(from_value::<Anchor>(unresolved.clone()).unwrap()).unwrap();
    assert_eq!(saved, unresolved);
    assert_eq!(to_value(AnnotationSource::Ai).unwrap(), json!("ai"));
    assert!(!saved.as_object().unwrap().contains_key("page"));
}

#[test]
fn an_unknown_page_field_does_not_promote_an_unresolved_anchor() {
    let canonical = json!({"state":"unresolved",
        "attachment_id":"019a5665-18c0-7000-8000-000000000002",
        "quote":"unverified quote", "reason":"not found"});
    let mut input = canonical.clone();
    input["page"] = json!({"attachment_id":canonical["attachment_id"],
        "page_index":4, "page_label":"iv"});

    let anchor: Anchor = from_value(input).unwrap();
    assert!(matches!(&anchor, Anchor::Unresolved { .. }));
    assert_eq!(to_value(anchor).unwrap(), canonical);
}

#[test]
fn storage_keys_reject_path_escape_on_construction_and_deserialization() {
    for bad in [
        "", "/tmp/a", "../a", "a/../b", "a//b", "a/./b", "C:\\a", "a\0b",
    ] {
        assert!(StorageKey::try_from(bad.to_owned()).is_err(), "{bad:?}");
        assert!(from_value::<StorageKey>(json!(bad)).is_err());
    }
    assert_eq!(
        StorageKey::try_from("ab/cd/file.pdf".to_owned())
            .unwrap()
            .as_str(),
        "ab/cd/file.pdf"
    );
}
