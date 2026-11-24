//! #19 annotation requests on the library worker (single writer).
use super::{Worker, files::invalid};
use cm_core::annotations::{AnnotationService, Edit};
use cm_protocol::{annotations::*, library::ServiceError};

/// Page index, label and characters of one page of the PDF being searched.
#[cfg(feature = "pdf-metadata")]
type ExtractedPage = (u32, Option<String>, Vec<(Option<char>, cm_core::geometry::Quad)>);
/// Re-anchoring reads at most this many pages of the replacement PDF.
#[cfg(feature = "pdf-metadata")]
const MAX_PROPOSAL_PAGES: u32 = 2000;

impl Worker<'_> {
    pub(super) fn annotations(
        &mut self,
        request: AnnotationRequest,
    ) -> Result<AnnotationReply, ServiceError> {
        use AnnotationRequest::*;
        if let Propose(id) = request {
            return self.propose(id).map(AnnotationReply::Proposal);
        }
        let mut service = AnnotationService::new(self.db.annotations());
        Ok(match request {
            List(id) => AnnotationReply::List(service.list(id)?),
            ForPaper(id) => AnnotationReply::List(service.for_paper(id)?),
            Create(input) => edit(service.create(*input)?),
            Update(id, patch) => edit(service.update(id, patch)?),
            Delete(id) => edit(service.delete(id)?),
            ConfirmAi(id) => edit(service.confirm_ai(id)?),
            Undo(e) => {
                service.undo(&e)?;
                AnnotationReply::Done
            }
            Redo(e) => {
                service.redo(&e)?;
                AnnotationReply::Done
            }
            CarryOver { from, to } => AnnotationReply::Edits(service.carry_over(from, to)?),
            Resolve {
                id,
                page_index,
                page_label,
                quads,
            } => edit(service.resolve(id, page_index, page_label, quads)?),
            Propose(_) => unreachable!("handled above"),
        })
    }
    #[cfg(feature = "pdf-metadata")]
    fn propose(&mut self, id: AnnotationId) -> Result<Option<Proposal>, ServiceError> {
        use cm_core::annotations::{PageText, propose_anchor};
        use cm_pdf::worker::*;
        let annotation = AnnotationService::new(self.db.annotations())
            .get(id)?
            .ok_or(ServiceError::NotFound)?;
        let attachment = cm_core::ports::AttachmentRepository::get(
            &self.db.attachments(),
            annotation.attachment_id,
        )?
        .ok_or(ServiceError::NotFound)?;
        let path = cm_storage::attachments::ManagedStore::new(&self.directory)
            .and_then(|s| s.verified_path(&attachment))
            .map_err(|_| invalid("The PDF is missing or does not match its checksum"))?;
        let mut pages: Vec<ExtractedPage> = Vec::new();
        let mut count = 1u32;
        let mut index = 0u32;
        while index < count.min(MAX_PROPOSAL_PAGES) {
            let worker = self.pdf_worker().map_err(|e| invalid(&e))?;
            let page_index = cm_protocol::domain::PageIndex::try_from(index)
                .map_err(|_| invalid("Page out of range"))?;
            let output = worker
                .submit(
                    Operation::Page {
                        source: Source {
                            path: path.clone(),
                            password: None,
                        },
                        page_index,
                        zoom: None,
                    },
                    Cancellation::default(),
                    std::time::Duration::from_secs(10),
                )
                .and_then(|t| t.wait(std::time::Duration::from_secs(11)))
                .map_err(|e| invalid(format!("Cannot read the PDF ({e})")))?;
            let Output::Page(page) = output else {
                return Err(invalid("Unexpected PDF worker reply"));
            };
            count = u32::try_from(page.page_count).unwrap_or(0);
            pages.push((
                index,
                page.page_label.clone(),
                page.characters
                    .iter()
                    .map(|c| (c.unicode, c.quad))
                    .collect(),
            ));
            index += 1;
        }
        let texts: Vec<PageText<'_>> = pages
            .iter()
            .map(|(i, label, chars)| PageText {
                page_index: *i,
                page_label: label.clone(),
                chars,
            })
            .collect();
        Ok(propose_anchor(&annotation, &texts))
    }
    #[cfg(not(feature = "pdf-metadata"))]
    fn propose(&mut self, _id: AnnotationId) -> Result<Option<Proposal>, ServiceError> {
        Err(invalid(
            "This build has no PDF runtime to search the replacement PDF",
        ))
    }
}

fn edit(edit: Edit) -> AnnotationReply {
    AnnotationReply::Edit(Box::new(edit))
}
