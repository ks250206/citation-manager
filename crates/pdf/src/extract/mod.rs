//! #18 bounded page-text extraction: one PDF load per call, text only (no glyph boxes,
//! bitmaps or annotations), a page range per call and a character cap per page.
//! Runs only on the cm-pdf-serial thread like every other PDFium entry point.
use crate::worker::{Context, PdfError, Source};
use pdfium_render::prelude::*;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Pages per `Operation::Text` call.
pub const MAX_TEXT_PAGES: u32 = 64;
/// Upper bound for `max_chars` per page.
pub const MAX_TEXT_CHARS: u32 = 1_000_000;
/// Pages whose PDFium character count exceeds this are not materialized at all.
const MAX_NATIVE_CHARS: i32 = 4_000_000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PageText {
    pub page_index: u32,
    pub page_label: Option<String>,
    /// Text in PDFium's character order, cut to `max_chars` Unicode scalars.
    pub text: String,
    /// The page had more than `max_chars` scalars (or an over-limit native text layer).
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TextBatch {
    pub page_count: u32,
    /// SHA-256 of the bytes that were parsed.
    pub sha256: [u8; 32],
    pub pages: Vec<PageText>,
}

pub(crate) fn validate(first_page: u32, max_pages: u32, max_chars: u32) -> Result<(), PdfError> {
    if !(1..=MAX_TEXT_PAGES).contains(&max_pages)
        || !(1..=MAX_TEXT_CHARS).contains(&max_chars)
        || first_page == u32::MAX
    {
        return Err(PdfError::LimitExceeded);
    }
    Ok(())
}

pub(crate) fn text(
    pdfium: &Pdfium,
    source: &Source,
    first_page: u32,
    max_pages: u32,
    max_chars: u32,
    ctx: &Context,
) -> Result<TextBatch, PdfError> {
    use crate::adapter::{native, read};
    let data = read(source, ctx)?;
    let sha256 = Sha256::digest(&data).into();
    let doc = pdfium
        .load_pdf_from_byte_vec(data, source.password.as_deref())
        .map_err(native)?;
    ctx.check()?;
    let count = doc.pages().len();
    let page_count = u32::try_from(count).map_err(|_| PdfError::Malformed)?;
    let end = first_page.saturating_add(max_pages).min(page_count);
    let mut pages = Vec::new();
    for index in first_page..end {
        ctx.check()?;
        let page = doc
            .pages()
            .get(i32::try_from(index).map_err(|_| PdfError::InvalidPage)?)
            .map_err(native)?;
        let layer = page.text().map_err(native)?;
        let (text, truncated) = if layer.len() > MAX_NATIVE_CHARS {
            (String::new(), true)
        } else {
            // Character order as defined in the document (including PDFium's generated
            // spaces/line breaks). `PdfPageText::all()` (bounded text over the page-size
            // rectangle) returned empty text for the corpus fixture on the pinned runtime,
            // so it is not used here.
            let mut text = String::new();
            let mut kept = 0u32;
            let mut truncated = false;
            for c in layer.chars().iter() {
                if kept.is_multiple_of(4096) {
                    ctx.check()?;
                }
                if let Some(c) = c.unicode_char() {
                    if kept == max_chars {
                        truncated = true;
                        break;
                    }
                    text.push(c);
                    kept += 1;
                }
            }
            (text, truncated)
        };
        pages.push(PageText {
            page_index: index,
            page_label: page.label().map(str::to_owned),
            text,
            truncated,
        });
    }
    ctx.check()?;
    Ok(TextBatch {
        page_count,
        sha256,
        pages,
    })
}
