// Entry of `content.js`. The build wraps this module as `var __cmCapture = (() => {...})()`
// and appends `__cmCapture.run();`, so the snapshot is the script's completion value, which
// `scripting.executeScript` hands back to the background. The content script registers no
// listeners and sends no runtime messages: it runs once per explicit save and ends.
import { extractSnapshot } from './extract';
import type { PageSnapshot } from '../shared/types';

export function run(): PageSnapshot {
  return extractSnapshot(document, location.href);
}
