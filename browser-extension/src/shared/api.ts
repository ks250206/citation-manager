// The subset of the WebExtension API the extension uses, typed locally (no @types/chrome
// dependency). Firefox exposes the promise-based `browser` namespace; Chrome MV3 returns
// promises from `chrome.*` for every call used here.

export interface Tab {
  id?: number;
  url?: string;
  title?: string;
  active?: boolean;
  windowId?: number;
}

export interface MessageSender {
  id?: string;
  url?: string;
  origin?: string;
  tab?: Tab;
  frameId?: number;
}

export interface Event<F> {
  addListener(listener: F): void;
  removeListener(listener: F): void;
}

export interface Port {
  name: string;
  sender?: MessageSender;
  postMessage(message: unknown): void;
  disconnect(): void;
  onMessage: Event<(message: unknown, port: Port) => void>;
  onDisconnect: Event<(port: Port) => void>;
  error?: { message?: string } | null;
}

export interface InjectionResult {
  frameId: number;
  result?: unknown;
  documentId?: string;
}

export interface WebExtApi {
  runtime: {
    id: string;
    lastError?: { message?: string } | null;
    getURL(path: string): string;
    connect(info: { name: string }): Port;
    sendNativeMessage(application: string, message: unknown): Promise<unknown>;
    onConnect: Event<(port: Port) => void>;
    onMessage: Event<(message: unknown, sender: MessageSender, sendResponse: (r: unknown) => void) => unknown>;
    onMessageExternal?: Event<(message: unknown, sender: MessageSender) => unknown>;
    onConnectExternal?: Event<(port: Port) => void>;
  };
  scripting: {
    executeScript(injection: { target: { tabId: number; frameIds?: number[] }; files: string[] }): Promise<InjectionResult[]>;
  };
  tabs: {
    get(tabId: number): Promise<Tab>;
    query(query: { active?: boolean; currentWindow?: boolean }): Promise<Tab[]>;
  };
  storage?: {
    local: {
      get(keys: string[]): Promise<Record<string, unknown>>;
      set(items: Record<string, unknown>): Promise<void>;
    };
  };
}

export function webext(): WebExtApi {
  const g = globalThis as unknown as { browser?: WebExtApi; chrome?: WebExtApi };
  const api = g.browser ?? g.chrome;
  if (!api?.runtime) throw new Error('WebExtension API not available');
  return api;
}
