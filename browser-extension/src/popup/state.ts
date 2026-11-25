// Popup state machine. Pure: `reduce(state, event)` returns the next state; the controller
// performs the host calls and feeds their outcomes back as events.
import type { Op } from '../shared/messages';
import type {
  Action,
  CommitResult,
  Corrections,
  Destination,
  DestinationsResult,
  ErrorInfo,
  HelloResult,
  JobStatus,
  Outcome,
  PageSnapshot,
  PreviewResult,
} from '../shared/types';

/** What the user can do about an error. */
export type Recovery = 'pair' | 'install' | 'retry' | 'repreview' | 'back' | 'none';

export type Phase =
  | { kind: 'starting' }
  | { kind: 'ready' }
  | { kind: 'pairing'; error: ErrorInfo }
  | { kind: 'working'; op: Op }
  | { kind: 'review' }
  | { kind: 'saved' }
  | { kind: 'error'; op: Op; error: ErrorInfo; recovery: Recovery };

export interface State {
  phase: Phase;
  hello: HelloResult | null;
  destinations: DestinationsResult | null;
  /** Destination chosen in the picker. */
  destination: Destination;
  offline: boolean;
  snapshot: PageSnapshot | null;
  preview: PreviewResult | null;
  /** Destination and corrections the current preview was made with. */
  previewDestination: Destination | null;
  corrections: Corrections;
  /** Corrections of a preview that failed, for Retry (the last good preview stays for Back). */
  pendingCorrections: Corrections | null;
  decisions: Action[];
  pdf: number | null;
  saved: CommitResult | null;
  job: JobStatus | null;
  /** `capture.pdf_job` polls made for the current job state. */
  polls: number;
  /** Commit conflicts for the current page (an older daemon commits a hidden duplicate as conflict). */
  conflicts: number;
}

export const MAX_POLLS = 5;

export function initialState(destination: Destination = { collection: null, tags: [] }, offline = false): State {
  return {
    phase: { kind: 'starting' },
    hello: null,
    destinations: null,
    destination,
    offline,
    snapshot: null,
    preview: null,
    previewDestination: null,
    corrections: {},
    pendingCorrections: null,
    decisions: [],
    pdf: null,
    saved: null,
    job: null,
    polls: 0,
    conflicts: 0,
  };
}

export type Event =
  | { type: 'begin'; op: Op }
  | { type: 'hello'; outcome: Outcome<HelloResult> }
  | { type: 'destinations'; outcome: Outcome<DestinationsResult> }
  | { type: 'extracted'; outcome: Outcome<PageSnapshot> }
  | { type: 'previewed'; outcome: Outcome<PreviewResult>; destination: Destination; corrections: Corrections }
  | { type: 'committed'; outcome: Outcome<CommitResult> }
  | { type: 'job'; outcome: Outcome<JobStatus>; op: 'pdf_job' | 'retry_pdf' }
  | { type: 'decision'; index: number; action: Action }
  | { type: 'pdf'; index: number | null }
  | { type: 'collection'; id: string | null }
  | { type: 'tag'; id: string; on: boolean }
  | { type: 'offline'; on: boolean }
  | { type: 'failed'; op: Op; error: ErrorInfo }
  | { type: 'repoll' }
  | { type: 'back' };

export function recoveryFor(error: ErrorInfo, op: Op, hasPreview: boolean): Recovery {
  switch (error.code) {
    case 'not_paired':
    case 'unauthorized':
      return 'pair';
    case 'host_missing':
    case 'host_forbidden':
    case 'unsupported_version':
      return 'install';
    case 'conflict':
      return 'repreview';
    case 'not_found':
      return op === 'commit' ? 'repreview' : 'none';
    default:
      if (error.retryable) return 'retry';
      // e.g. the daemon refused to create an exact duplicate: go back and choose Skip.
      return hasPreview ? 'back' : 'none';
  }
}

const sameDestination = (a: Destination | null, b: Destination): boolean =>
  a !== null &&
  a.collection === b.collection &&
  a.tags.length === b.tags.length &&
  [...a.tags].sort().join() === [...b.tags].sort().join();

/** The preview no longer matches the chosen destination (a new preview is needed). */
export function destinationChanged(state: State): boolean {
  return state.preview !== null && !sameDestination(state.previewDestination, state.destination);
}

export function isTerminalJob(job: JobStatus | null): boolean {
  return job === null || job.state === 'succeeded' || job.state === 'failed' || job.state === 'cancelled';
}

function fail(state: State, op: Op, error: ErrorInfo): State {
  const recovery = recoveryFor(error, op, state.preview !== null);
  if (recovery === 'pair') return { ...state, phase: { kind: 'pairing', error } };
  return { ...state, phase: { kind: 'error', op, error, recovery } };
}

export const DUPLICATE_MESSAGE =
  'This paper is already in your library (same DOI or identifier). Browser capture cannot create a duplicate or merge; open the app to merge it, or cancel.';

/** The daemon skipped a row the preview reported as an exact duplicate: nothing was saved. */
export const DUPLICATE_SKIPPED_MESSAGE =
  'This paper is already in your library (same DOI or identifier), so nothing was saved. Browser capture cannot create a duplicate or merge; open the app to merge it.';

/**
 * A Create on a hidden exact duplicate is skipped by the daemon (#27, see `committed`). An
 * older daemon, or a duplicate only the desktop can see, answers it with `conflict`, the
 * same code as "library changed, preview again": the first conflict suggests previewing
 * again; a second one for the same page with a reported duplicate is an existing paper.
 */
function commitFailed(state: State, error: ErrorInfo): State {
  if (error.code === 'commit_unknown') {
    // Possibly saved: drop the preview so the same commit is not offered again.
    return fail({ ...state, preview: null, previewDestination: null }, 'commit', error);
  }
  const preview = state.preview;
  const suspect = preview !== null && (preview.hidden_duplicates > 0 || preview.candidates.some((c) => c.exact_duplicate));
  if (error.code !== 'conflict' || !suspect) return fail(state, 'commit', error);
  const conflicts = state.conflicts + 1;
  if (conflicts >= 2 || preview.candidates.some((c) => c.exact_duplicate)) {
    return fail({ ...state, conflicts }, 'commit', { code: 'duplicate', message: DUPLICATE_MESSAGE, retryable: false });
  }
  return fail({ ...state, conflicts }, 'commit', {
    ...error,
    message: 'Your library may already contain this paper (matches are hidden from browser capture), or it changed since the preview. Preview again to check.',
  });
}

export function reduce(state: State, event: Event): State {
  switch (event.type) {
    case 'begin':
      return { ...state, phase: { kind: 'working', op: event.op } };
    case 'hello': {
      if (!event.outcome.ok) return fail(state, 'hello', event.outcome.error);
      const hello = event.outcome.result;
      if (hello.protocol !== 1) {
        return fail({ ...state, hello }, 'hello', {
          code: 'unsupported_version',
          message: `The native host speaks protocol ${hello.protocol}; this extension needs 1.`,
          retryable: false,
        });
      }
      if (!hello.paired) {
        return {
          ...state,
          hello,
          phase: {
            kind: 'pairing',
            error: { code: 'not_paired', message: 'Browser capture is not paired with your library yet.', retryable: false },
          },
        };
      }
      return { ...state, hello };
    }
    case 'destinations': {
      if (!event.outcome.ok) return fail(state, 'destinations', event.outcome.error);
      const destinations = event.outcome.result;
      // Drop remembered choices that no longer exist.
      const collections = new Set(destinations.collections.map((c) => c.id));
      const tags = new Set(destinations.tags.map((t) => t.id));
      const collection =
        state.destination.collection && collections.has(state.destination.collection) ? state.destination.collection : null;
      return {
        ...state,
        destinations,
        destination: { collection, tags: state.destination.tags.filter((t) => tags.has(t)) },
        phase: { kind: 'ready' },
      };
    }
    case 'extracted':
      if (!event.outcome.ok) return fail(state, 'extract', event.outcome.error);
      return { ...state, snapshot: event.outcome.result, corrections: {}, pendingCorrections: null, conflicts: 0 };
    case 'previewed': {
      if (!event.outcome.ok) return fail({ ...state, pendingCorrections: event.corrections }, 'preview', event.outcome.error);
      const preview = event.outcome.result;
      return {
        ...state,
        preview,
        previewDestination: { collection: event.destination.collection, tags: [...event.destination.tags] },
        corrections: event.corrections,
        pendingCorrections: null,
        // The capture grant can only Create or Skip. An exact duplicate cannot be created
        // (the daemon refuses it), so it defaults to Skip; merging happens in the app.
        decisions: preview.candidates.map((c) => (c.exact_duplicate ? 'Skip' : 'Create')),
        pdf: preview.pdf_candidates.length > 0 ? 0 : null,
        phase: { kind: 'review' },
      };
    }
    case 'committed': {
      if (!event.outcome.ok) return commitFailed(state, event.outcome.error);
      const saved = event.outcome.result;
      if (saved.paper === null && saved.created === 0 && saved.skipped > 0 && state.decisions.includes('Create')) {
        // Save as new was chosen, but the daemon skipped the row as an exact duplicate of an
        // existing paper (hidden from this grant). The preview is used up; nothing was saved.
        const cleared = { ...state, saved: null, job: null, polls: 0, preview: null, previewDestination: null };
        return fail(cleared, 'commit', { code: 'duplicate', message: DUPLICATE_SKIPPED_MESSAGE, retryable: false });
      }
      return { ...state, saved, job: saved.pdf_job, polls: 0, preview: null, phase: { kind: 'saved' } };
    }
    case 'job': {
      if (!event.outcome.ok) {
        // A failing status check does not undo the save: stay on the result.
        return fail({ ...state }, event.op, event.outcome.error);
      }
      const polls = event.op === 'retry_pdf' ? 0 : state.polls + 1;
      return { ...state, job: event.outcome.result, polls, phase: { kind: 'saved' } };
    }
    case 'decision': {
      if (event.index < 0 || event.index >= state.decisions.length) return state;
      const decisions = [...state.decisions];
      decisions[event.index] = event.action;
      return { ...state, decisions };
    }
    case 'pdf': {
      const count = state.preview?.pdf_candidates.length ?? 0;
      if (event.index !== null && (event.index < 0 || event.index >= count)) return state;
      return { ...state, pdf: event.index };
    }
    case 'collection':
      return { ...state, destination: { ...state.destination, collection: event.id } };
    case 'tag': {
      const tags = state.destination.tags.filter((t) => t !== event.id);
      if (event.on) tags.push(event.id);
      return { ...state, destination: { ...state.destination, tags } };
    }
    case 'offline':
      return { ...state, offline: event.on };
    case 'failed':
      return fail(state, event.op, event.error);
    case 'repoll':
      return { ...state, polls: 0 };
    case 'back': {
      // Leaving an error drops the corrections of a failed preview.
      const left: State = { ...state, pendingCorrections: null };
      if (left.saved !== null && left.preview === null) return { ...left, phase: { kind: 'saved' } };
      if (left.preview && left.previewDestination) {
        // A failed re-preview (e.g. after a destination change) leaves the last good preview:
        // go back to it with the destination it was made for, so it can be committed.
        return { ...left, destination: { ...left.previewDestination }, phase: { kind: 'review' } };
      }
      return { ...left, phase: left.preview ? { kind: 'review' } : { kind: 'ready' } };
    }
  }
}

/** Whether the commit button may be used. */
export function canCommit(state: State): boolean {
  return (
    state.phase.kind === 'review' &&
    state.preview !== null &&
    !destinationChanged(state) &&
    state.decisions.some((d) => d === 'Create')
  );
}

/** The PDF part of a saved result, for display. */
export type PdfView =
  | { kind: 'none' }
  | { kind: 'not_submitted'; message: string }
  | { kind: 'pending'; job: JobStatus; polling: boolean }
  | { kind: 'attached'; job: JobStatus }
  | { kind: 'failed'; job: JobStatus };

export function pdfView(state: State): PdfView {
  const saved = state.saved;
  if (!saved) return { kind: 'none' };
  if (saved.pdf_error) return { kind: 'not_submitted', message: saved.pdf_error };
  const job = state.job;
  if (!job) return { kind: 'none' };
  if (job.state === 'succeeded') return { kind: 'attached', job };
  if (job.state === 'failed' || job.state === 'cancelled') return { kind: 'failed', job };
  return { kind: 'pending', job, polling: state.polls < MAX_POLLS };
}
