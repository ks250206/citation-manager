//! Explicit decisions for every reviewed row; no automatic confirmation/budget override.
use cm_protocol::import::*;
use cm_protocol::library::ServiceError;
/// PDF review temporarily pauses file batching, retaining its exact progress.
#[derive(Default)]
pub struct ReviewSessions {
    pub active: Review,
    pub paused_file: Option<Review>,
    pub replaced_file: Option<String>,
}
impl ReviewSessions {
    pub fn cancel_active(
        &mut self,
        mut cancel: impl FnMut(&PreviewId) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        if let Some(preview) = &self.active.preview {
            cancel(&preview.id)?;
        }
        self.active.preview = None;
        self.active.decisions.clear();
        Ok(())
    }
    pub fn pause_file(
        &mut self,
        cancel: impl FnMut(&PreviewId) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        self.cancel_active(cancel)?;
        let previous = std::mem::take(&mut self.active);
        if previous.from_file {
            self.paused_file = Some(previous);
        }
        Ok(())
    }
    pub fn replace_file(
        &mut self,
        cancel: impl FnMut(&PreviewId) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        self.cancel_active(cancel)?;
        if let Some(file) = self
            .paused_file
            .as_ref()
            .or(self.active.from_file.then_some(&self.active))
        {
            self.replaced_file = Some(format!(
                "Previous bibliography stopped at {} / {} processed, {} committed, {} skipped. Earlier commits remain saved; the new file starts a new review.",
                file.batch_start, file.total, file.committed, file.skipped
            ));
        }
        self.active = Review::default();
        self.paused_file = None;
        Ok(())
    }
    pub fn finish_pdf(
        &mut self,
        cancel: impl FnMut(&PreviewId) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        // An Attach/Retry report must not discard an unrelated manual/file preview.
        if self.active.pdf_index.is_some() {
            self.cancel_active(cancel)?;
            self.active = Review::default();
        }
        Ok(())
    }
    pub fn resume_file(
        &mut self,
        cancel: impl FnMut(&PreviewId) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError> {
        if self.paused_file.is_some() {
            self.cancel_active(cancel)?;
            self.active = self.paused_file.take().expect("checked paused file");
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct Review {
    pub preview: Option<ImportPreview>,
    pub decisions: Vec<Option<ImportAction>>,
    pub row: usize,
    pub batch_start: usize,
    pub batch_count: usize,
    pub total: usize,
    pub committed: usize,
    pub skipped: usize,
    pub from_file: bool,
    pub pdf_index: Option<usize>,
}
impl Review {
    pub fn set_preview(&mut self, preview: ImportPreview) {
        self.decisions = vec![None; preview.candidates.len()];
        self.row = 0;
        self.batch_count = preview.candidates.len();
        if !self.from_file {
            self.total = self.batch_count;
        }
        self.preview = Some(preview);
    }
    pub fn choose(&mut self, index: usize, action: ImportAction) -> bool {
        let Some(candidate) = self.preview.as_ref().and_then(|p| p.candidates.get(index)) else {
            return false;
        };
        if !matches!(action, ImportAction::Skip) && candidate.diagnostics.iter().any(|d| d.error) {
            return false;
        }
        if matches!(action, ImportAction::Create)
            && (candidate.matches.iter().any(|m| m.kind.exact())
                || !candidate.batch_matches.is_empty())
        {
            return false;
        }
        self.decisions[index] = Some(action);
        true
    }
    /// A user action may choose Create for all currently reviewed eligible rows;
    /// error and duplicate rows still require individual reviewed Merge/Skip.
    pub fn choose_valid_creates(&mut self) {
        for i in 0..self.decisions.len() {
            if self.decisions[i].is_none() {
                self.choose(i, ImportAction::Create);
            }
        }
    }
    pub fn commit(&self) -> Option<ImportCommit> {
        let preview = self.preview.as_ref()?;
        Some(ImportCommit {
            preview_id: preview.id.clone(),
            decisions: self
                .decisions
                .iter()
                .enumerate()
                .map(|(candidate_index, action)| {
                    Some(ImportDecision {
                        candidate_index,
                        action: action.clone()?,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        })
    }
}
