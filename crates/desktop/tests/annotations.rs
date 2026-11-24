//! Selection → annotation helpers (#19); persistence is tested in crates/daemon/tests.
use cm_desktop::annotations::*;
use cm_protocol::{
    annotations::{Anchor, MAX_CONTEXT_CHARS},
    attachments::{PageChar, Rect},
    domain::AttachmentId,
};

fn chars(lines: &[(&str, f32)]) -> Vec<PageChar> {
    let mut out = vec![];
    for (text, bottom) in lines {
        for (i, c) in text.chars().enumerate() {
            out.push(PageChar {
                unicode: Some(c),
                quad: Rect {
                    left: 10.0 + i as f32 * 6.0,
                    bottom: *bottom,
                    right: 16.0 + i as f32 * 6.0,
                    top: bottom + 10.0,
                }
                .quad(),
            });
        }
    }
    out
}

#[test]
fn multi_line_selection_becomes_one_quad_per_line_with_quote_and_context() {
    let page = chars(&[("前置き Hello", 100.0), ("world 後書き", 80.0)]);
    let att = AttachmentId("00000000-0000-0000-0000-000000000001".parse().unwrap());
    // "Hello" + "world" (indices 4..=13 span the line break).
    let a = highlight(att, 3, Some("iv".into()), &page, 4, 13, "green").unwrap();
    assert_eq!(a.quote, "Helloworld");
    let Anchor::Resolved {
        page_index,
        page_label,
        quads,
    } = &a.anchor
    else {
        panic!()
    };
    assert_eq!((*page_index, page_label.as_deref()), (3, Some("iv")));
    assert_eq!(quads.len(), 2, "one rectangle per line");
    let first = quads[0].bounds();
    assert_eq!((first.left, first.right), (34.0, 64.0));
    assert_eq!(a.context_before.as_deref(), Some("前置き "));
    assert_eq!(a.context_after.as_deref(), Some(" 後書き"));
    assert_eq!(a.color.as_deref(), Some("green"));
    // Out-of-range or whitespace-only selections create nothing.
    assert!(highlight(att, 0, None, &page, 5, 99, "yellow").is_none());
    assert!(highlight(att, 0, None, &page, 3, 3, "yellow").is_none());
    assert!(highlight(att, 0, None, &[], 0, 0, "yellow").is_none());
}

#[test]
fn context_is_bounded_and_colors_map_safely() {
    let long = "x".repeat(200);
    let page = chars(&[(&long, 0.0)]);
    let (before, after) = context(&page, 100, 101);
    assert_eq!(before.unwrap().chars().count(), MAX_CONTEXT_CHARS);
    assert_eq!(after.unwrap().chars().count(), MAX_CONTEXT_CHARS);
    assert_eq!(context(&page, 0, 199), (None, None));
    assert_eq!(rgb(Some("#102030")), 0x102030);
    assert_eq!(rgb(Some("bogus")), rgb(None));
}
