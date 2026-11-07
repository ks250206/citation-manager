//! Native request ownership. No UI, I/O, dispatch, or public protocol contract.
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Lane {
    Query,
    Editor,
    Organization,
    Membership,
    Import,
    Doi,
    Pdf,
    Attachments,
    LoadSettings,
    SaveSettings,
    Export,
}

pub(crate) fn invalidate_lookup(pending: &mut HashMap<Lane, u64>) -> bool {
    pending.remove(&Lane::Doi).is_some()
}

/// Return the new ticket and whether the host must cancel its old DOI token.
/// Invalidating the ticket also rejects a reply already queued before cancellation.
pub(crate) fn begin_request(
    pending: &mut HashMap<Lane, u64>,
    sequence: &mut u64,
    lane: Lane,
) -> (u64, bool) {
    let cancel_lookup =
        matches!(lane, Lane::Editor | Lane::Import | Lane::Pdf) && invalidate_lookup(pending);
    *sequence += 1;
    pending.insert(lane, *sequence);
    (*sequence, cancel_lookup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::OperationCancels;

    #[test]
    fn ownership_requests_reject_queued_doi_replies_without_cancelling_pdf() {
        for lane in [Lane::Editor, Lane::Import, Lane::Pdf] {
            let mut pending = HashMap::new();
            let mut sequence = 0;
            let mut cancels = OperationCancels::default();
            let doi = cancels.start_doi();
            let (old_doi, _) = begin_request(&mut pending, &mut sequence, Lane::Doi);
            let pdf = cancels.start_pdf();
            let reserved_progress_ticket = sequence + 1;
            let (owned, cancel_lookup) = begin_request(&mut pending, &mut sequence, lane);
            if cancel_lookup {
                cancels.cancel_doi();
            }
            assert!(cancel_lookup);
            assert!(doi.is_cancelled());
            assert!(!pdf.is_cancelled());
            // Same ticket gate used by native receive(): an already-queued DOI
            // completion cannot reach the handler that replaces/cancels a preview.
            let mut preview_replacements = 0;
            if pending.get(&Lane::Doi) == Some(&old_doi) {
                preview_replacements += 1;
            }
            assert_eq!(preview_replacements, 0);
            assert_eq!(pending.get(&lane), Some(&owned));
            assert_eq!(owned, reserved_progress_ticket);
        }
    }

    #[test]
    fn unrelated_requests_keep_the_live_lookup_ticket() {
        for lane in [
            Lane::Query,
            Lane::Organization,
            Lane::Membership,
            Lane::Attachments,
            Lane::LoadSettings,
            Lane::SaveSettings,
            Lane::Export,
        ] {
            let mut pending = HashMap::new();
            let mut sequence = 0;
            let (doi, _) = begin_request(&mut pending, &mut sequence, Lane::Doi);
            let (_, cancelled) = begin_request(&mut pending, &mut sequence, lane);
            assert!(!cancelled);
            assert_eq!(pending.get(&Lane::Doi), Some(&doi));
        }
    }

    #[test]
    fn review_navigation_invalidates_old_lookup_but_a_later_lookup_can_finish() {
        let mut pending = HashMap::new();
        let mut sequence = 0;
        let (old, _) = begin_request(&mut pending, &mut sequence, Lane::Doi);
        assert!(invalidate_lookup(&mut pending));
        assert!(!invalidate_lookup(&mut pending));
        let (commit, cancelled) = begin_request(&mut pending, &mut sequence, Lane::Pdf);
        assert!(!cancelled);
        assert_ne!(pending.get(&Lane::Doi), Some(&old));
        assert_eq!(pending.remove(&Lane::Pdf), Some(commit));
        let (fresh, _) = begin_request(&mut pending, &mut sequence, Lane::Doi);
        assert_ne!(old, fresh);
        assert_eq!(pending.get(&Lane::Doi), Some(&fresh));
    }
}
