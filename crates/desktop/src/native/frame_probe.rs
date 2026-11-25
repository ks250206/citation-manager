//! #86 GPUI glue for the opt-in frame-time recorder ([`crate::frame_timing`]). Nothing here
//! runs unless `CM_FRAME_TIMING` was set at start-up.
use gpui::{prelude::*, *};

/// Appended after the whole window tree only while recording: its paint closes the frame the
/// root view's render opened.
pub(super) fn end_marker() -> impl IntoElement {
    canvas(|_, _, _| (), |_, _, _, _| crate::frame_timing::frame_end())
        .absolute()
        .size_0()
}

/// Writes the last partial second on a normal quit (the writer thread covers signals).
pub(super) fn flush_on_quit(cx: &mut App) {
    if crate::frame_timing::enabled() {
        cx.on_app_quit(|_| {
            crate::frame_timing::flush();
            async {}
        })
        .detach();
    }
}
