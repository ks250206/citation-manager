// UTF-8 helpers. Limits in the contract are byte limits of the UTF-8 JSON payload, while JS
// strings are UTF-16, so every bound is measured and cut on encoded bytes.
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8');

export function utf8Length(text: string): number {
  // Fast path for ASCII-only strings (most markup).
  let ascii = true;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) > 0x7f) {
      ascii = false;
      break;
    }
  }
  return ascii ? text.length : encoder.encode(text).length;
}

/**
 * Replaces lone UTF-16 surrogates with U+FFFD so the string is valid Unicode and therefore
 * valid UTF-8 JSON for the host (serde_json rejects escaped lone surrogates).
 */
export function wellFormed(text: string): string {
  // Matches a high surrogate not followed by a low one, or a low surrogate not preceded by a high one.
  return text.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, '�');
}

export interface Truncated {
  text: string;
  truncated: boolean;
}

/** Cuts `text` to at most `maxBytes` UTF-8 bytes on a code point boundary. */
export function truncateUtf8(text: string, maxBytes: number): Truncated {
  const clean = wellFormed(text);
  if (maxBytes <= 0) return { text: '', truncated: clean.length > 0 };
  if (clean.length <= maxBytes && utf8Length(clean) <= maxBytes) {
    return { text: clean, truncated: false };
  }
  const bytes = encoder.encode(clean);
  if (bytes.length <= maxBytes) return { text: clean, truncated: false };
  let cut = maxBytes;
  // Back up over continuation bytes (10xxxxxx) so the cut never splits a code point.
  while (cut > 0 && ((bytes[cut] ?? 0) & 0xc0) === 0x80) cut--;
  return { text: decoder.decode(bytes.subarray(0, cut)), truncated: true };
}
