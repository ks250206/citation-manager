//! #84 actions on several selected papers (row context menu): which requests to send, and
//! a runner that sends them one at a time through the existing client and reports how far
//! it got. No new daemon command: each step is an existing single-paper request.
use cm_protocol::{
    domain::{CollectionId, PaperId, TagId},
    library::{LibraryClient, Reply, ServiceError},
    organization::OrganizationRequest,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// Membership requests that put `papers` into `target`. Viewing another collection makes
/// it a move (one atomic request per paper); otherwise papers are added and keep their
/// other collections. Papers already shown in `target` need nothing.
pub fn collection_requests(
    papers: &[PaperId],
    viewing: Option<CollectionId>,
    target: CollectionId,
) -> Vec<OrganizationRequest> {
    if viewing == Some(target) {
        return vec![];
    }
    papers
        .iter()
        .map(|&paper| match viewing {
            Some(from) => OrganizationRequest::MoveCollectionMember(paper, from, target),
            None => OrganizationRequest::CollectionMember(paper, target, true),
        })
        .collect()
}

/// Tag requests: set membership of `tag` to `present` on every paper.
pub fn tag_requests(papers: &[PaperId], tag: TagId, present: bool) -> Vec<OrganizationRequest> {
    papers
        .iter()
        .map(|&paper| OrganizationRequest::TagMember(paper, tag, present))
        .collect()
}

/// How a bulk run ended. `error` is the first failure; nothing after it was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkReport {
    pub total: usize,
    pub done: usize,
    pub error: Option<ServiceError>,
}

impl BulkReport {
    /// Status line, e.g. "Moved 3 papers to Trash" or "Tagged 2 of 5 papers; stopped: …".
    pub fn summary(&self, verb: &str) -> String {
        let noun = |n: usize| if n == 1 { "paper" } else { "papers" };
        match &self.error {
            None => format!("{verb} {} {}", self.done, noun(self.done)),
            Some(e) => format!(
                "{verb} {} of {} {}; stopped: {e}",
                self.done,
                self.total,
                noun(self.total)
            ),
        }
    }
}

/// One request; it must call its reply exactly once if it returns Ok.
pub type Step =
    Box<dyn FnOnce(&Arc<dyn LibraryClient>, Reply<()>) -> Result<(), ServiceError> + Send>;

pub fn organization_step(request: OrganizationRequest) -> Step {
    Box::new(move |client, done| {
        client.organization(request, Box::new(move |result| done(result.map(|_| ()))))
    })
}

pub fn lifecycle_step(paper: PaperId, deleted: bool) -> Step {
    Box::new(move |client, done| {
        client.lifecycle(
            paper,
            deleted,
            Box::new(move |result| done(result.map(|_| ()))),
        )
    })
}

/// Send the steps in order, each after the previous one succeeded, then report once.
/// Stops at the first failure (a later step could depend on the earlier one, e.g. a move).
pub fn run(client: Arc<dyn LibraryClient>, steps: Vec<Step>, done: Reply<BulkReport>) {
    let total = steps.len();
    next(client, steps.into(), total, 0, done);
}

fn next(
    client: Arc<dyn LibraryClient>,
    mut steps: VecDeque<Step>,
    total: usize,
    finished: usize,
    done: Reply<BulkReport>,
) {
    let Some(step) = steps.pop_front() else {
        return done(Ok(BulkReport {
            total,
            done: finished,
            error: None,
        }));
    };
    // Either the reply or a synchronous submit error finishes this step, never both.
    let slot = Arc::new(Mutex::new(Some(done)));
    let reply_slot = slot.clone();
    let reply_client = client.clone();
    let submitted = step(
        &client,
        Box::new(move |result| {
            let Some(done) = reply_slot.lock().ok().and_then(|mut s| s.take()) else {
                return;
            };
            match result {
                Ok(()) => next(reply_client, steps, total, finished + 1, done),
                Err(error) => done(Ok(BulkReport {
                    total,
                    done: finished,
                    error: Some(error),
                })),
            }
        }),
    );
    if let Err(error) = submitted
        && let Some(done) = slot.lock().ok().and_then(|mut s| s.take())
    {
        done(Ok(BulkReport {
            total,
            done: finished,
            error: Some(error),
        }));
    }
}

/// What a bulk run reports when it ends, kept with the run rather than in shared state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkRun {
    /// Status verb, e.g. "Moved to Trash:".
    pub verb: String,
    /// The run changed the papers themselves (Trash / restore): re-read the open one.
    pub refetch: bool,
}

/// At most one bulk run at a time. A run is identified by its request lane and ticket;
/// it counts as active only while that lane still waits for that ticket, so a run whose
/// ticket was replaced (or whose reply was lost) never blocks later runs.
#[derive(Debug)]
pub struct BulkSlot<L> {
    run: Option<(L, u64, BulkRun)>,
}

impl<L> Default for BulkSlot<L> {
    fn default() -> Self {
        Self { run: None }
    }
}

impl<L: Copy + PartialEq> BulkSlot<L> {
    /// The run in progress, if any. `pending` reports the ticket each lane waits for.
    pub fn active(&self, pending: impl Fn(L) -> Option<u64>) -> Option<&BulkRun> {
        self.run
            .as_ref()
            .filter(|(lane, ticket, _)| pending(*lane) == Some(*ticket))
            .map(|(_, _, run)| run)
    }
    /// Record a run that was just submitted with this lane/ticket.
    pub fn start(&mut self, lane: L, ticket: u64, run: BulkRun) {
        self.run = Some((lane, ticket, run));
    }
    /// The run a completion belongs to; None for any other lane/ticket.
    pub fn finish(&mut self, lane: L, ticket: u64) -> Option<BulkRun> {
        match &self.run {
            Some((l, t, _)) if *l == lane && *t == ticket => self.run.take().map(|(_, _, r)| r),
            _ => None,
        }
    }
}

/// One entry of the context menu's Tags submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagChoice {
    /// One paper whose memberships are known: check box, choosing it flips membership.
    Toggle { present: bool },
    /// Several papers: choosing it adds the tag to all of them.
    AddToAll,
    /// One paper whose memberships have not arrived yet: shown disabled, so an existing
    /// tag can never be offered as "add" (the menu is rebuilt when they arrive).
    Loading,
}

/// Tags submenu state for `selected`. `membership` is the paper the loaded memberships
/// belong to and its tags (None while they load).
pub fn tag_choice(
    tag: TagId,
    selected: &[PaperId],
    membership: Option<(PaperId, &[TagId])>,
) -> TagChoice {
    match (selected, membership) {
        ([one], Some((paper, tags))) if *one == paper => TagChoice::Toggle {
            present: tags.contains(&tag),
        },
        ([_], _) => TagChoice::Loading,
        _ => TagChoice::AddToAll,
    }
}
