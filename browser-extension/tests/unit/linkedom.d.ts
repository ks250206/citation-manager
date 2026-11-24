declare module 'linkedom' {
  export function parseHTML(html: string): { document: unknown; window: unknown };
  export const DOMParser: unknown;
}
