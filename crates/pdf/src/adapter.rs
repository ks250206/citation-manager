//! All entry points run only on cm-pdf-serial; no native handles escape.
use crate::{
    geometry::{Origin, Quad, Rect, Rotation, Viewport},
    worker::*,
};
use pdfium_render::prelude::*;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub(crate) fn load(path: &Path) -> Result<Pdfium, PdfError> {
    if process_started() {
        return Err(PdfError::AlreadyStarted);
    }
    if !path.is_absolute() {
        return Err(PdfError::LibraryPath);
    }
    let bytes = std::fs::read(path).map_err(|_| PdfError::LibraryLoad)?;
    if format!("{:x}", Sha256::digest(&bytes)) != PDFIUM_SHA256 {
        return Err(PdfError::LibraryChecksum);
    }
    let bindings = Pdfium::bind_to_library(path).map_err(|_| PdfError::LibraryLoad)?;
    claim_process()?;
    Ok(Pdfium::new(bindings))
}
fn native(e: PdfiumError) -> PdfError {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            PdfError::PasswordRequiredOrIncorrect
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError) => {
            PdfError::Malformed
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => {
            PdfError::UnsupportedSecurity
        }
        PdfiumError::PageIndexOutOfBounds => PdfError::InvalidPage,
        _ => PdfError::NativeFailure,
    }
}
fn read(source: &Source, ctx: &Context) -> Result<Vec<u8>, PdfError> {
    ctx.check()?;
    let f = File::open(&source.path).map_err(|_| PdfError::Io)?;
    if !f.metadata().map_err(|_| PdfError::Io)?.is_file() {
        return Err(PdfError::Io);
    }
    let mut data = Vec::new();
    f.take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| PdfError::Io)?;
    if data.len() > MAX_INPUT_BYTES {
        return Err(PdfError::LimitExceeded);
    }
    ctx.check()?;
    Ok(data)
}
fn rect(r: PdfRect) -> Rect {
    Rect {
        left: r.left().value,
        bottom: r.bottom().value,
        right: r.right().value,
        top: r.top().value,
    }
}
fn pdf_rect(r: Rect) -> PdfRect {
    PdfRect::new_from_values(r.bottom, r.left, r.top, r.right)
}
fn pdf_quad(q: Quad) -> PdfQuadPoints {
    let [a, b, c, d] = q.0;
    PdfQuadPoints::new_from_values(a.x, a.y, b.x, b.y, c.x, c.y, d.x, d.y)
}
fn quad(q: PdfQuadPoints) -> Quad {
    use crate::geometry::Point;
    Quad([
        Point {
            x: q.x1().value,
            y: q.y1().value,
        },
        Point {
            x: q.x2().value,
            y: q.y2().value,
        },
        Point {
            x: q.x3().value,
            y: q.y3().value,
        },
        Point {
            x: q.x4().value,
            y: q.y4().value,
        },
    ])
}
fn page_index(index: cm_domain::PageIndex, count: i32) -> Result<i32, PdfError> {
    let i = u32::from(index);
    if i >= count.max(0) as u32 {
        Err(PdfError::InvalidPage)
    } else {
        Ok(i as i32)
    }
}
fn crop(page: &PdfPage<'_>) -> Result<Rect, PdfError> {
    // In 0.9.5 media()/crop() call the non-inheriting FPDFPage_Get*Box getters.
    // FPDF_GetPageBoundingBox resolves page-tree inheritance and the effective
    // MediaBox/CropBox intersection used by PDFium rendering (unrotated points).
    let crop = rect(page.boundaries().bounding().map_err(native)?.bounds);
    crop.valid()
        .then_some(crop)
        .ok_or(PdfError::InvalidGeometry)
}
fn check_points(q: Quad, crop: Rect) -> Result<(), PdfError> {
    if q.valid() && q == q.bounds().quad() && q.0.iter().all(|p| crop.contains(*p)) {
        Ok(())
    } else {
        Err(PdfError::InvalidGeometry)
    }
}
pub(crate) fn execute(pdfium: &Pdfium, op: Operation, ctx: &Context) -> Result<Output, PdfError> {
    match op {
        Operation::Metadata { source } => {
            let data = read(&source, ctx)?;
            let digest = Sha256::digest(&data).into();
            let (xmp, mut warnings) = crate::metadata::xmp(&data);
            let doc = pdfium
                .load_pdf_from_byte_vec(data, source.password.as_deref())
                .map_err(native)?;
            ctx.check()?;
            let mut draft = xmp.unwrap_or_default();
            use pdfium_render::prelude::PdfDocumentMetadataTagType as Tag;
            if draft.title.trim().is_empty() {
                draft.title = doc
                    .metadata()
                    .get(Tag::Title)
                    .map(|t| t.value().to_owned())
                    .unwrap_or_default();
            }
            if draft.bibliography.authors.is_empty()
                && let Some(author) = doc.metadata().get(Tag::Author)
                && !author.value().trim().is_empty()
            {
                draft.bibliography.authors.push(cm_domain::Contributor {
                    given_name: None,
                    family_name: None,
                    literal_name: Some(author.value().to_owned()),
                    orcid: None,
                    role: "author".into(),
                });
            }
            if draft.bibliography.doi.is_none() {
                for tag in [Tag::Subject, Tag::Keywords] {
                    if let Some(value) = doc.metadata().get(tag)
                        && let Ok(doi) = cm_core::papers::normalize_doi(value.value())
                    {
                        draft.bibliography.doi = Some(doi);
                        break;
                    }
                }
            }
            // Expose competing Info values instead of silently treating a packet hint as authority.
            if let Some(info_title) = doc.metadata().get(Tag::Title)
                && !info_title.value().trim().is_empty()
                && info_title.value() != draft.title
            {
                warnings.push(if info_title.value().len() <= 4096 {
                    format!(
                        "Alternative Info title (not selected): {}",
                        info_title.value()
                    )
                } else {
                    "Alternative Info title exceeds display limits".into()
                });
            }
            if let Some(selected_doi) = &draft.bibliography.doi {
                for tag in [Tag::Subject, Tag::Keywords] {
                    if let Some(value) = doc.metadata().get(tag)
                        && let Ok(alternative) = cm_core::papers::normalize_doi(value.value())
                        && &alternative != selected_doi
                    {
                        warnings.push(format!(
                            "Conflicting embedded Info DOI (not selected): {alternative}"
                        ));
                    }
                }
            }
            if cm_core::import::normalize::validate_limits(&draft).is_err() {
                draft = Default::default();
                warnings.push(
                    "Embedded metadata exceeds review limits; use filename/manual metadata".into(),
                );
            }
            warnings.push(
                "Embedded PDF metadata is an untrusted suggestion; review before saving".into(),
            );
            ctx.check()?;
            Ok(Output::Metadata(Box::new(MetadataSnapshot {
                sha256: digest,
                draft,
                page_count: doc.pages().len(),
                warnings,
            })))
        }
        Operation::Page {
            source,
            page_index: index,
            zoom,
        } => {
            let data = read(&source, ctx)?;
            let doc = pdfium
                .load_pdf_from_byte_vec(data, source.password.as_deref())
                .map_err(native)?;
            ctx.check()?;
            let page = doc
                .pages()
                .get(page_index(index, doc.pages().len())?)
                .map_err(native)?;
            let crop = crop(&page)?;
            let rotation = match page.rotation().map_err(native)? {
                PdfPageRenderRotation::None => Rotation::R0,
                PdfPageRenderRotation::Degrees90 => Rotation::R90,
                PdfPageRenderRotation::Degrees180 => Rotation::R180,
                PdfPageRenderRotation::Degrees270 => Rotation::R270,
            };
            let text = page.text().map_err(native)?;
            if text.chars().len() > MAX_CHARS {
                return Err(PdfError::LimitExceeded);
            }
            let mut characters = Vec::new();
            let mut string = String::new();
            for (index, c) in text.chars().iter().enumerate() {
                ctx.check()?;
                let unicode = c.unicode_char();
                if let Some(c) = unicode {
                    string.push(c);
                }
                characters.push(Character {
                    index,
                    unicode,
                    bounds: rect(c.tight_bounds().map_err(native)?),
                    quad: rect(c.loose_bounds().map_err(native)?).quad(),
                });
            }
            let mut annotations = Vec::new();
            if page.annotations().len() > 1024 {
                return Err(PdfError::LimitExceeded);
            }
            for a in page.annotations().iter() {
                ctx.check()?;
                if a.attachment_points().len() > 1024 {
                    return Err(PdfError::LimitExceeded);
                }
                let quads = a.attachment_points().iter().map(quad).collect();
                annotations.push(AnnotationSnapshot {
                    kind: format!("{:?}", a.annotation_type()),
                    contents: a.contents(),
                    bounds: rect(a.bounds().map_err(native)?),
                    quads,
                });
            }
            let bitmap = if let Some(zoom) = zoom {
                let v = Viewport::new(crop, rotation, zoom, Origin::TopLeft)
                    .ok_or(PdfError::InvalidGeometry)?;
                let (w, h) = v.size();
                if w < 1.
                    || h < 1.
                    || w.ceil() > 8192.
                    || h.ceil() > 8192.
                    || w.ceil() * h.ceil() > MAX_PIXELS as f32
                {
                    return Err(PdfError::LimitExceeded);
                }
                ctx.check()?;
                let bitmap = page
                    .render_with_config(
                        &PdfRenderConfig::new()
                            .set_fixed_size(w.ceil() as i32, h.ceil() as i32)
                            .set_format(PdfBitmapFormat::BGRA)
                            .render_annotations(true),
                    )
                    .map_err(native)?;
                ctx.check()?;
                Some(Bitmap {
                    width: bitmap.width() as u32,
                    height: bitmap.height() as u32,
                    rgba: bitmap.as_rgba_bytes(),
                })
            } else {
                None
            };
            ctx.check()?;
            Ok(Output::Page(Box::new(PageSnapshot {
                page_index: index,
                physical_page: u32::from(index.physical_display()),
                page_label: page.label().map(str::to_owned),
                page_count: doc.pages().len(),
                crop,
                rotation,
                text_status: if characters.is_empty() {
                    TextStatus::NoExtractableText
                } else {
                    TextStatus::Extracted
                },
                text: string,
                characters,
                annotations,
                bitmap,
            })))
        }
        Operation::Export {
            source,
            highlights,
            notes,
        } => {
            let data = read(&source, ctx)?;
            let doc = pdfium
                .load_pdf_from_byte_vec(data, source.password.as_deref())
                .map_err(native)?;
            ctx.check()?;
            for h in highlights {
                ctx.check()?;
                let mut page = doc
                    .pages()
                    .get(page_index(h.page_index, doc.pages().len())?)
                    .map_err(native)?;
                let crop = crop(&page)?;
                for q in &h.quads {
                    check_points(*q, crop)?;
                }
                let bounds = h
                    .quads
                    .iter()
                    .map(|q| q.bounds())
                    .reduce(|a, b| Rect {
                        left: a.left.min(b.left),
                        bottom: a.bottom.min(b.bottom),
                        right: a.right.max(b.right),
                        top: a.top.max(b.top),
                    })
                    .ok_or(PdfError::InvalidGeometry)?;
                let mut a = page
                    .annotations_mut()
                    .create_highlight_annotation()
                    .map_err(native)?;
                a.set_bounds(pdf_rect(bounds)).map_err(native)?;
                a.set_stroke_color(PdfColor::new(255, 255, 0, 128))
                    .map_err(native)?;
                a.set_contents(&h.comment).map_err(native)?;
                a.set_is_printed(true).map_err(native)?;
                for q in h.quads {
                    a.attachment_points_mut()
                        .create_attachment_point_at_end(pdf_quad(q))
                        .map_err(native)?;
                }
            }
            for n in notes {
                ctx.check()?;
                let mut page = doc
                    .pages()
                    .get(page_index(n.page_index, doc.pages().len())?)
                    .map_err(native)?;
                check_points(n.bounds.quad(), crop(&page)?)?;
                let mut a = page
                    .annotations_mut()
                    .create_text_annotation(&n.contents)
                    .map_err(native)?;
                a.set_bounds(pdf_rect(n.bounds)).map_err(native)?;
                a.set_is_printed(true).map_err(native)?;
            }
            ctx.check()?;
            let bytes = doc.save_to_bytes().map_err(native)?;
            ctx.check()?;
            if bytes.len() > MAX_INPUT_BYTES {
                return Err(PdfError::LimitExceeded);
            }
            Ok(Output::Export(NativeExport {
                bytes,
                appearance: AppearanceStatus::RequiresExportCompletion,
            }))
        }
    }
}
