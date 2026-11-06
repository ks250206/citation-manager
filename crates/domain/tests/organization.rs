use cm_domain::{TagKey, organization_name};

#[test]
fn tag_keys_share_unicode_whitespace_and_case_policy_and_serde_validation() {
    for (input, expected) in [
        ("  Solid\tSTATE\n Battery\u{3000}", "solid state battery"),
        ("\u{a0}ÉNERGIE\u{2003}日本語 研究 ", "énergie 日本語 研究"),
        ("İ", "i\u{307}"),
        ("Σ", "σ"),
        ("ＡＢＣ", "ａｂｃ"),
        ("100%_\\'", "100%_\\'"),
    ] {
        let key = TagKey::new(input).unwrap();
        assert_eq!(key.as_str(), expected);
        assert_eq!(TagKey::new(key.as_str()).unwrap(), key);
        assert_eq!(
            serde_json::from_str::<TagKey>(&serde_json::to_string(&key).unwrap()).unwrap(),
            key
        );
    }
    for (a, b) in [("é", "e\u{301}"), ("ß", "ss"), ("Ａ", "A"), ("ς", "σ")] {
        assert_ne!(TagKey::new(a).unwrap(), TagKey::new(b).unwrap());
    }
    assert_eq!(
        organization_name(" Japanese\u{3000}TAG ").unwrap(),
        "Japanese TAG"
    );
    for input in [
        "".to_string(),
        " \t\n\u{3000}".to_string(),
        "a\0b".into(),
        "a\u{7f}b".into(),
        "x".repeat(257),
        "İ".repeat(129),
        " ".repeat(4097),
    ] {
        assert!(TagKey::new(&input).is_err(), "{input:?}");
        assert!(serde_json::from_str::<TagKey>(&serde_json::to_string(&input).unwrap()).is_err());
    }
    assert!(TagKey::new(&"界".repeat(256)).is_ok());
}
