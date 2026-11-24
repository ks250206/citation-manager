// Popup controller: runs the save flow against the background and feeds outcomes into the
// pure state machine. All side effects are injected, so the flow is unit-tested with fakes.
import type { PopupRequest } from '../shared/messages';
import { buildCaptureRequest, normalizeCorrections } from '../shared/request';
import type {
  CommitResult,
  Corrections,
  Destination,
  DestinationsResult,
  HelloResult,
  JobStatus,
  Outcome,
  PageSnapshot,
  PreviewResult,
} from '../shared/types';
import { canCommit, destinationChanged, initialState, isTerminalJob, MAX_POLLS, reduce, type Event, type State } from './state';

type Body = PopupRequest extends infer R ? (R extends { rid: number } ? Omit<R, 'rid'> : never) : never;

export interface Client {
  request(body: Body): Promise<Outcome<unknown>>;
}

export interface ControllerDeps {
  client: Client;
  /** The tab to save: `?tab=` or the active tab of the popup's window. */
  tabId(): Promise<number>;
  sleep(ms: number): Promise<void>;
  /** Remembers the destination and offline choice (best effort). */
  remember(destination: Destination, offline: boolean): void;
  pollMs?: number;
}

export class Controller {
  state: State;
  private readonly listeners: ((state: State) => void)[] = [];

  constructor(
    private readonly deps: ControllerDeps,
    initial: State = initialState(),
  ) {
    this.state = initial;
  }

  subscribe(listener: (state: State) => void): void {
    this.listeners.push(listener);
  }

  dispatch(event: Event): State {
    this.state = reduce(this.state, event);
    for (const listener of this.listeners) listener(this.state);
    return this.state;
  }

  private async call<T>(body: Body): Promise<Outcome<T>> {
    return (await this.deps.client.request(body)) as Outcome<T>;
  }

  /** `hello` (never starts the daemon), then the destination list if paired. */
  async start(): Promise<void> {
    this.dispatch({ type: 'begin', op: 'hello' });
    const hello = await this.call<HelloResult>({ op: 'hello' });
    this.dispatch({ type: 'hello', outcome: hello });
    if (this.state.phase.kind !== 'working') return;
    await this.loadDestinations();
  }

  async loadDestinations(): Promise<void> {
    this.dispatch({ type: 'begin', op: 'destinations' });
    this.dispatch({ type: 'destinations', outcome: await this.call<DestinationsResult>({ op: 'destinations' }) });
  }

  /** The explicit Save action: read the page, then preview. */
  async save(): Promise<void> {
    this.dispatch({ type: 'begin', op: 'extract' });
    let tabId: number;
    try {
      tabId = await this.deps.tabId();
    } catch {
      this.dispatch({
        type: 'failed',
        op: 'extract',
        error: { code: 'no_access', message: 'No page to save in this window.', retryable: false },
      });
      return;
    }
    this.dispatch({ type: 'extracted', outcome: await this.call<PageSnapshot>({ op: 'extract', tabId }) });
    if (this.state.snapshot && this.state.phase.kind === 'working') await this.preview({});
  }

  /** (Re-)preview the current snapshot with the chosen destination and corrections. */
  async preview(corrections: Corrections = this.state.corrections): Promise<void> {
    const snapshot = this.state.snapshot;
    if (!snapshot) return this.save();
    const destination: Destination = { collection: this.state.destination.collection, tags: [...this.state.destination.tags] };
    this.deps.remember(destination, this.state.offline);
    this.dispatch({ type: 'begin', op: 'preview' });
    const capture = buildCaptureRequest(snapshot, destination, this.state.offline, corrections);
    const outcome = await this.call<PreviewResult>({ op: 'preview', capture });
    this.dispatch({ type: 'previewed', outcome, destination, corrections });
  }

  /** Manual correction from the review form: validated, then sent as a new preview. */
  async correct(input: Corrections): Promise<void> {
    let corrections: Corrections;
    try {
      corrections = normalizeCorrections(input);
    } catch (error) {
      this.dispatch({
        type: 'failed',
        op: 'preview',
        error: { code: 'invalid_input', message: (error as Error).message, retryable: false },
      });
      return;
    }
    await this.preview(corrections);
  }

  /** Destination changes after a preview need a new preview (destination is part of it). */
  async setCollection(id: string | null): Promise<void> {
    this.dispatch({ type: 'collection', id });
    if (this.state.phase.kind === 'review' && destinationChanged(this.state)) await this.preview();
  }

  async setTag(id: string, on: boolean): Promise<void> {
    this.dispatch({ type: 'tag', id, on });
    if (this.state.phase.kind === 'review' && destinationChanged(this.state)) await this.preview();
  }

  async commit(): Promise<void> {
    const preview = this.state.preview;
    if (!preview || !canCommit(this.state)) return;
    const decisions = this.state.decisions.map((action, candidate_index) => ({ candidate_index, action }));
    this.dispatch({ type: 'begin', op: 'commit' });
    const outcome = await this.call<CommitResult>({ op: 'commit', preview_id: preview.preview_id, decisions, pdf: this.state.pdf });
    this.dispatch({ type: 'committed', outcome });
    if (outcome.ok) await this.poll();
  }

  /** Follows the PDF job a few times; the daemon keeps working after the popup closes. */
  async poll(): Promise<void> {
    const pollMs = this.deps.pollMs ?? 1500;
    while (this.state.phase.kind === 'saved' && !isTerminalJob(this.state.job) && this.state.polls < MAX_POLLS) {
      await this.deps.sleep(pollMs);
      const job = this.state.job as JobStatus;
      const outcome = await this.call<JobStatus>({ op: 'pdf_job', job: job.id });
      this.dispatch({ type: 'job', outcome, op: 'pdf_job' });
    }
  }

  /** "Check again" after the automatic polls stopped while the job was still active. */
  async refreshJob(): Promise<void> {
    if (this.state.phase.kind !== 'saved' || isTerminalJob(this.state.job)) return;
    this.dispatch({ type: 'repoll' });
    await this.poll();
  }

  async retryPdf(): Promise<void> {
    const job = this.state.job;
    if (!job) return;
    this.dispatch({ type: 'begin', op: 'retry_pdf' });
    this.dispatch({ type: 'job', outcome: await this.call<JobStatus>({ op: 'retry_pdf', job: job.id }), op: 'retry_pdf' });
    await this.poll();
  }

  /** Discard the open preview. */
  async cancel(): Promise<void> {
    const preview = this.state.preview;
    if (preview) await this.call({ op: 'cancel', preview_id: preview.preview_id });
    this.state = { ...this.state, preview: null, previewDestination: null, snapshot: null, corrections: {} };
    this.dispatch({ type: 'back' });
  }

  /** The error view's primary action. */
  async recover(): Promise<void> {
    const phase = this.state.phase;
    if (phase.kind !== 'error') return;
    if (phase.recovery === 'repreview') return this.state.snapshot ? this.preview() : this.save();
    if (phase.recovery !== 'retry') {
      this.dispatch({ type: 'back' });
      return;
    }
    switch (phase.op) {
      case 'hello':
        return this.start();
      case 'destinations':
        return this.loadDestinations();
      case 'extract':
        return this.save();
      case 'preview':
        return this.preview();
      case 'commit':
        return this.commit_retry();
      case 'pdf_job':
        this.dispatch({ type: 'back' });
        return this.poll();
      case 'retry_pdf':
        return this.retryPdf();
      case 'cancel':
        this.dispatch({ type: 'back' });
        return;
    }
  }

  /** Commit again after a retryable failure (busy / daemon unavailable): same preview. */
  private async commit_retry(): Promise<void> {
    this.dispatch({ type: 'back' });
    await this.commit();
  }
}
