// Test helpers: an inert DOM (linkedom never runs page scripts) and repository fixtures.
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { DOMParser, parseHTML } from 'linkedom';

declare const __REPO_ROOT__: string;
export const REPO_ROOT: string = __REPO_ROOT__;

// The extractor falls back to DOMParser where `document.implementation` is missing.
(globalThis as unknown as { DOMParser: unknown }).DOMParser = DOMParser;

export function dom(html: string): Document {
  return parseHTML(html).document as unknown as Document;
}

export function fixture(name: string): string {
  return readFileSync(join(REPO_ROOT, 'fixtures/html', name), 'utf8');
}

export function fixtureNames(): string[] {
  return readdirSync(join(REPO_ROOT, 'fixtures/html')).filter((f) => f.endsWith('.html'));
}
