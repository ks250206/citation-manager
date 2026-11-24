// Popup rendering with plain DOM. Every page- or host-supplied string is inserted with
// textContent / attribute setters, never as markup.
import type { Collection, Contributor, Corrections, Draft, PreviewCandidate } from '../shared/types';
import type { Controller } from './controller';
import { canCommit, destinationChanged, pdfView, type State } from './state';

type Attrs = Record<string, string | boolean | undefined>;
type Child = Node | string | null | undefined | false;

export function el(doc: Document, tag: string, attrs: Attrs = {}, ...children: Child[]): HTMLElement {
  const node = doc.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === false) continue;
    if (value === true) node.setAttribute(key, '');
    else node.setAttribute(key, value);
  }
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    node.append(typeof child === 'string' ? doc.createTextNode(child) : child);
  }
  return node;
}

const WORKING: Record<string, string> = {
  hello: 'Connecting to Citation Manager…',
  destinations: 'Loading collections and tags…',
  extract: 'Reading this page…',
  preview: 'Resolving metadata and checking duplicates…',
  commit: 'Saving to your library…',
  pdf_job: 'Checking the PDF download…',
  retry_pdf: 'Retrying the PDF download…',
  cancel: 'Cancelling…',
};

export function authorName(c: Contributor): string {
  if (c.literal_name) return c.literal_name;
  return [c.given_name, c.family_name].filter(Boolean).join(' ');
}

function authorLine(c: Contributor): string {
  if (c.literal_name) return c.literal_name;
  if (c.family_name && c.given_name) return `${c.family_name}, ${c.given_name}`;
  return c.family_name ?? c.given_name ?? '';
}

function doiOf(d: Draft): string | null {
  return d.doi ?? d.external_ids.find((e) => e.namespace === 'doi')?.value ?? null;
}

/** Collections in tree order with depth, for the picker. */
export function collectionTree(collections: Collection[]): { c: Collection; depth: number }[] {
  const children = new Map<string | null, Collection[]>();
  for (const c of collections) {
    const list = children.get(c.parent) ?? [];
    list.push(c);
    children.set(c.parent, list);
  }
  const ids = new Set(collections.map((c) => c.id));
  const out: { c: Collection; depth: number }[] = [];
  const seen = new Set<string>();
  const walk = (parent: string | null, depth: number) => {
    const list = (children.get(parent) ?? []).sort((a, b) => a.sort_order - b.sort_order || a.name.localeCompare(b.name));
    for (const c of list) {
      if (seen.has(c.id) || depth > 32) continue;
      seen.add(c.id);
      out.push({ c, depth });
      walk(c.id, depth + 1);
    }
  };
  walk(null, 0);
  // Orphans (parent not in the list) at top level.
  for (const c of collections) if (!seen.has(c.id) && (c.parent === null || !ids.has(c.parent))) out.push({ c, depth: 0 });
  return out;
}

function destinationPicker(doc: Document, state: State, ctl: Controller, disabled: boolean): HTMLElement {
  const box = el(doc, 'fieldset', { id: 'destination', class: 'destination' }, el(doc, 'legend', {}, 'Destination'));
  const select = el(doc, 'select', { id: 'collection', disabled }) as HTMLSelectElement;
  select.append(el(doc, 'option', { value: '' }, 'No collection'));
  for (const { c, depth } of collectionTree(state.destinations?.collections ?? [])) {
    const option = el(doc, 'option', { value: c.id }, `${'  '.repeat(depth)}${c.name}`) as HTMLOptionElement;
    option.selected = state.destination.collection === c.id;
    select.append(option);
  }
  select.addEventListener('change', () => void ctl.setCollection(select.value === '' ? null : select.value));
  box.append(el(doc, 'label', { for: 'collection' }, 'Collection'), select);
  const tags = state.destinations?.tags ?? [];
  if (tags.length > 0) {
    const list = el(doc, 'div', { class: 'tags', id: 'tags' });
    for (const tag of tags) {
      const input = el(doc, 'input', { type: 'checkbox', value: tag.id, disabled }) as HTMLInputElement;
      input.checked = state.destination.tags.includes(tag.id);
      input.addEventListener('change', () => void ctl.setTag(tag.id, input.checked));
      list.append(el(doc, 'label', { class: 'tag' }, input, tag.name));
    }
    box.append(el(doc, 'div', { class: 'label' }, 'Tags'), list);
  }
  return box;
}

function candidateView(doc: Document, candidate: PreviewCandidate, index: number, state: State, ctl: Controller): HTMLElement {
  const d = candidate.draft;
  const rows: [string, string | null][] = [
    ['Title', d.title],
    ['Authors', d.authors.map(authorName).filter(Boolean).slice(0, 12).join('; ') + (d.authors.length > 12 ? ` (+${d.authors.length - 12})` : '') || null],
    ['Year', d.year === null ? null : String(d.year)],
    ['DOI', doiOf(d)],
    ['Journal', d.journal],
    ['URL', d.url],
  ];
  const dl = el(doc, 'dl', { class: 'fields' });
  for (const [label, value] of rows) {
    dl.append(el(doc, 'dt', {}, label), el(doc, 'dd', { 'data-field': label.toLowerCase(), class: value ? undefined : 'missing' }, value ?? '—'));
  }
  const box = el(doc, 'section', { class: 'candidate', 'data-candidate': String(index) }, dl);
  const notes = [...d.diagnostics, ...candidate.diagnostics].slice(0, 8);
  if (notes.length > 0) {
    box.append(el(doc, 'ul', { class: 'diagnostics' }, ...notes.map((n) => el(doc, 'li', { class: n.error ? 'error' : undefined }, `${n.field}: ${n.message}`))));
  }
  if (candidate.matches.length > 0) {
    const list = el(doc, 'ul', { class: 'matches', id: `matches-${index}` });
    for (const m of candidate.matches.slice(0, 8)) {
      list.append(el(doc, 'li', {}, `${m.kind}: ${m.title}${m.year ? ` (${m.year})` : ''}${m.doi ? ` doi:${m.doi}` : ''}`));
    }
    box.append(el(doc, 'div', { class: 'label warn' }, 'Possible duplicates in your library'), list);
    if (candidate.exact_duplicate) {
      box.append(el(doc, 'p', { class: 'hint' }, 'This paper is already in your library. Merge it in the app if needed; browser capture can only skip it.'));
    }
  }
  const decision = el(doc, 'div', { class: 'decision', role: 'radiogroup' });
  for (const action of ['Create', 'Skip'] as const) {
    const input = el(doc, 'input', { type: 'radio', name: `decision-${index}`, value: action, id: `decision-${index}-${action}` }) as HTMLInputElement;
    input.checked = state.decisions[index] === action;
    input.addEventListener('change', () => ctl.dispatch({ type: 'decision', index, action }));
    decision.append(el(doc, 'label', { for: input.id }, input, action === 'Create' ? 'Save as new' : 'Skip'));
  }
  box.append(decision);
  return box;
}

function correctionForm(doc: Document, state: State, ctl: Controller): HTMLElement {
  const d = state.preview?.candidates[0]?.draft;
  const c = state.corrections;
  const field = (id: string, label: string, value: string, multiline = false) => {
    const input = multiline
      ? (el(doc, 'textarea', { id, rows: '3' }) as HTMLTextAreaElement)
      : (el(doc, 'input', { id, type: 'text' }) as HTMLInputElement);
    input.value = value;
    return el(doc, 'label', { class: 'field' }, el(doc, 'span', {}, label), input);
  };
  const details = el(doc, 'details', { id: 'corrections', open: state.preview?.provisional ?? false });
  details.append(
    el(doc, 'summary', {}, state.preview?.provisional ? 'Complete the metadata' : 'Correct the metadata'),
    field('fix-title', 'Title', c.title ?? d?.title ?? ''),
    field('fix-authors', 'Authors (one per line, "Family, Given")', (c.authors ?? d?.authors.map(authorLine) ?? []).join('\n'), true),
    field('fix-year', 'Year', c.year ?? (d?.year ? String(d.year) : '')),
    field('fix-doi', 'DOI', c.doi ?? (d ? doiOf(d) ?? '' : '')),
    field('fix-journal', 'Journal', c.journal ?? d?.journal ?? ''),
  );
  const apply = el(doc, 'button', { type: 'button', id: 'apply-corrections' }, 'Apply and preview again');
  apply.addEventListener('click', () => {
    const value = (id: string) => (doc.getElementById(id) as HTMLInputElement | HTMLTextAreaElement | null)?.value ?? '';
    // Only fields the user changed become corrections; unchanged ones stay page data.
    const original = { title: d?.title ?? '', authors: (d?.authors.map(authorLine) ?? []).join('\n'), year: d?.year ? String(d.year) : '', doi: d ? doiOf(d) ?? '' : '', journal: d?.journal ?? '' };
    const next: Corrections = { ...state.corrections };
    if (value('fix-title').trim() !== original.title) next.title = value('fix-title');
    if (value('fix-authors').trim() !== original.authors) next.authors = value('fix-authors').split('\n');
    if (value('fix-year').trim() !== original.year) next.year = value('fix-year');
    if (value('fix-doi').trim() !== original.doi) next.doi = value('fix-doi');
    if (value('fix-journal').trim() !== original.journal) next.journal = value('fix-journal');
    void ctl.correct(next);
  });
  details.append(
    el(doc, 'p', { class: 'hint' }, 'Corrections are checked again by the library before saving. Other fields can be edited in the app.'),
    apply,
  );
  return details;
}

function button(doc: Document, id: string, label: string, onClick: () => void, attrs: Attrs = {}): HTMLElement {
  const b = el(doc, 'button', { type: 'button', id, ...attrs }, label);
  b.addEventListener('click', onClick);
  return b;
}

function reviewView(doc: Document, state: State, ctl: Controller): HTMLElement[] {
  const preview = state.preview;
  if (!preview) return [];
  const out: HTMLElement[] = [];
  if (preview.provisional) {
    out.push(el(doc, 'p', { class: 'banner', id: 'provisional' }, 'Little metadata was found on this page. It can be saved provisionally (title and URL) and completed below or later in the app.'));
  }
  if (state.snapshot?.html_truncated) {
    out.push(el(doc, 'p', { class: 'hint' }, 'This page is large; only its first part was read.'));
  }
  preview.candidates.forEach((candidate, i) => out.push(candidateView(doc, candidate, i, state, ctl)));
  if (preview.hidden_matches > 0) {
    out.push(
      el(
        doc,
        'p',
        { class: 'hint warn', id: 'hidden-matches', 'data-count': String(preview.hidden_matches) },
        `Possible duplicates: ${preview.hidden_matches} matching paper(s) already in your library (details are shown only in the app). An exact duplicate cannot be saved from the browser; merge it in the app.`,
      ),
    );
  }
  // Other daemon warnings, minus the generic import-API note and the hidden-match line above.
  for (const warning of preview.warnings.filter((w) => !/^Commit requires|existing-library match/.test(w)).slice(0, 4)) {
    out.push(el(doc, 'p', { class: 'hint warn' }, warning));
  }

  const pdfs = el(doc, 'fieldset', { id: 'pdfs', class: 'pdfs' }, el(doc, 'legend', {}, 'PDF'));
  const pdfOption = (index: number | null, label: string, title?: string) => {
    const input = el(doc, 'input', { type: 'radio', name: 'pdf', value: index === null ? 'none' : String(index), id: `pdf-${index ?? 'none'}` }) as HTMLInputElement;
    input.checked = state.pdf === index;
    input.addEventListener('change', () => ctl.dispatch({ type: 'pdf', index }));
    return el(doc, 'label', { for: input.id, class: 'pdf', title }, input, label);
  };
  preview.pdf_candidates.forEach((p, i) => {
    let host = p.url;
    try {
      const u = new URL(p.url);
      host = `${u.host}${u.pathname.length > 40 ? `${u.pathname.slice(0, 37)}…` : u.pathname}`;
    } catch {
      // keep the raw text
    }
    pdfs.append(pdfOption(i, `${host} (${p.origin.replace('_', '-')})`, p.url));
  });
  pdfs.append(pdfOption(null, preview.pdf_candidates.length === 0 ? 'No PDF found on this page' : 'Do not download a PDF'));
  pdfs.append(el(doc, 'p', { class: 'hint' }, 'The library downloads public PDFs itself after saving. PDFs that need your login: download them and attach them in the app.'));
  out.push(pdfs);
  out.push(destinationPicker(doc, state, ctl, false));
  out.push(correctionForm(doc, state, ctl));
  const changed = destinationChanged(state);
  out.push(
    el(
      doc,
      'div',
      { class: 'actions' },
      button(doc, 'commit', changed ? 'Updating preview…' : 'Save to library', () => void ctl.commit(), { disabled: !canCommit(state), class: 'primary' }),
      button(doc, 'cancel', 'Cancel', () => void ctl.cancel()),
    ),
  );
  return out;
}

function savedView(doc: Document, state: State, ctl: Controller): HTMLElement[] {
  const saved = state.saved;
  if (!saved) return [];
  const out: HTMLElement[] = [];
  out.push(
    saved.paper
      ? el(doc, 'p', { class: 'success', id: 'saved', 'data-paper': saved.paper }, 'Saved to your library. Open Citation Manager to read and annotate it.')
      : el(doc, 'p', { class: 'success', id: 'saved' }, 'Nothing was saved (skipped).'),
  );
  const pdf = pdfView(state);
  const box = el(doc, 'div', { id: 'pdf-status', 'data-state': pdf.kind });
  switch (pdf.kind) {
    case 'none':
      box.append(el(doc, 'p', {}, 'No PDF was requested.'));
      break;
    case 'not_submitted':
      box.append(el(doc, 'p', { class: 'warn' }, `The paper is saved, but the PDF download could not be started: ${pdf.message}`));
      break;
    case 'pending':
      box.append(el(doc, 'p', {}, `PDF download ${pdf.job.state.replace('_', ' ')} (attempt ${pdf.job.attempts} of ${pdf.job.max_attempts || '?'}).`));
      if (!pdf.polling) {
        box.append(
          el(doc, 'p', { class: 'hint' }, 'The download continues in the background; the app shows the result.'),
          button(doc, 'refresh-pdf', 'Check again', () => void ctl.refreshJob()),
        );
      }
      break;
    case 'attached':
      box.append(el(doc, 'p', { class: 'success' }, pdf.job.detected_committed ? 'The PDF was already attached.' : 'PDF attached.'));
      break;
    case 'failed':
      box.append(
        el(doc, 'p', { class: 'warn' }, `The paper is saved, but the PDF download ${pdf.job.state}: ${pdf.job.error ?? 'unknown error'}`),
        el(doc, 'p', { class: 'hint' }, 'If the PDF needs your login, download it in the browser and attach it in the app.'),
        button(doc, 'retry-pdf', 'Retry PDF download', () => void ctl.retryPdf()),
      );
      break;
  }
  out.push(box);
  return out;
}

const PAIRING = [
  'Browser capture needs a one-time pairing with your library.',
  'Run in a terminal:  papers-native-host pair',
  '(or pair from the app settings when available), then check again.',
];

const INSTALL = [
  'The Citation Manager native host is not installed for this browser, or does not allow this extension.',
  'Install the app (Linux: scripts/install_linux.py), or run:',
  'papers-native-host register --library <your library> --pair',
];

function lines(doc: Document, text: string[], cls: string): HTMLElement {
  return el(doc, 'div', { class: cls }, ...text.map((t) => el(doc, 'p', {}, t)));
}

export function render(root: HTMLElement, state: State, ctl: Controller): void {
  const doc = root.ownerDocument;
  const body: HTMLElement[] = [];
  const phase = state.phase;
  root.dataset.phase = phase.kind;
  switch (phase.kind) {
    case 'starting':
      body.push(el(doc, 'p', { class: 'status' }, 'Starting…'));
      break;
    case 'working':
      body.push(el(doc, 'p', { class: 'status', id: 'working', 'data-op': phase.op }, WORKING[phase.op] ?? 'Working…'));
      break;
    case 'pairing':
      body.push(
        el(doc, 'p', { class: 'warn', id: 'pairing' }, phase.error.message),
        lines(doc, PAIRING, 'instructions'),
        button(doc, 'check-again', 'Check again', () => void ctl.start()),
      );
      break;
    case 'ready': {
      const offline = el(doc, 'input', { type: 'checkbox', id: 'offline' }) as HTMLInputElement;
      offline.checked = state.offline;
      offline.addEventListener('change', () => ctl.dispatch({ type: 'offline', on: offline.checked }));
      body.push(
        destinationPicker(doc, state, ctl, false),
        el(doc, 'label', { class: 'option' }, offline, 'Do not look up the DOI online (use page metadata only)'),
        el(doc, 'div', { class: 'actions' }, button(doc, 'save', 'Save this page', () => void ctl.save(), { class: 'primary' })),
      );
      break;
    }
    case 'review':
      body.push(...reviewView(doc, state, ctl));
      break;
    case 'saved':
      body.push(...savedView(doc, state, ctl));
      break;
    case 'error': {
      body.push(el(doc, 'p', { class: 'error', id: 'error', 'data-code': phase.error.code }, phase.error.message));
      if (phase.recovery === 'install') body.push(lines(doc, INSTALL, 'instructions'));
      const actions = el(doc, 'div', { class: 'actions' });
      if (phase.recovery === 'retry') actions.append(button(doc, 'recover', 'Retry', () => void ctl.recover(), { class: 'primary' }));
      if (phase.recovery === 'repreview') actions.append(button(doc, 'recover', 'Preview again', () => void ctl.recover(), { class: 'primary' }));
      actions.append(button(doc, 'back', 'Back', () => ctl.dispatch({ type: 'back' })));
      body.push(actions);
      if (state.saved) body.push(el(doc, 'p', { class: 'hint' }, 'The paper itself is saved.'));
      break;
    }
  }
  root.replaceChildren(...body);
}
