//! Minimal host boundaries. #15 replaces navigation/layout; #17 supplies the PDF viewer.
use gpui::{prelude::*, *};

pub(super) fn navigation_host() -> Div {
    div()
        .w(px(112.))
        .flex_shrink_0()
        .p_3()
        .bg(rgb(0xeceff4))
        .child("LOCAL")
        .child(div().mt_3().child("All papers"))
        .child(
            div()
                .mt_4()
                .text_color(rgb(0x667084))
                .child("Manual library"),
        )
}

pub(super) fn pdf_host() -> Div {
    div()
        .mt_4()
        .p_4()
        .border_1()
        .border_color(rgb(0xd5dce5))
        .rounded_md()
        .child("PDF panel · viewer is not available yet")
}
