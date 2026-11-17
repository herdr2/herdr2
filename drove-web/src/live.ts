import { eventsUrl } from "./api";
import type { Item } from "./types";

export type LinkState = "connecting" | "live" | "reconnecting";

export const LIVE_ITEMS_PER_WORKER = 400;

/** Keep recent item snapshots, including updates, without retaining the whole Run. */
export function mergeLiveItem(items: Item[], item: Item): Item[] {
  const recent = items.filter((row) => row.workerId === item.workerId && row.id !== item.id);
  recent.push(item);
  recent.sort((a, b) => b.id - a.id);
  return [...items.filter((row) => row.workerId !== item.workerId), ...recent.slice(0, LIVE_ITEMS_PER_WORKER)];
}

export interface RunChange {
  table?: string;
  id?: string | number;
  runId?: string;
}

/**
 * One EventSource per open Run. Closing the returned function drops that
 * subscription so a Run switch cannot deliver into the next one.
 * The browser reconnects while the stream is only CONNECTING; a CLOSED
 * stream is opened again here. The second and later opens are reconnects.
 * Every open also reconciles the initial snapshot/subscription window via onOpen.
 */
export function openRunEvents(
  runId: string,
  handlers: {
    onChange: (change: RunChange) => void;
    onItem: (item: Item) => void;
    onLink: (state: LinkState) => void;
    onOpen?: () => void;
    onReconnect: () => void;
  },
): () => void {
  let dead = false;
  let opened = false;
  let es: EventSource | null = null;
  let retryTimer: number | null = null;
  let attempt = 0;

  const clearRetry = () => {
    if (retryTimer != null) {
      window.clearTimeout(retryTimer);
      retryTimer = null;
    }
  };

  const connect = () => {
    if (dead) return;
    clearRetry();
    const prev = es;
    es = null;
    prev?.close();
    const source = new EventSource(eventsUrl(runId));
    es = source;
    source.addEventListener("open", () => {
      if (dead || source !== es) return;
      const again = opened;
      opened = true;
      attempt = 0;
      handlers.onLink("live");
      handlers.onOpen?.();
      if (again) handlers.onReconnect();
    });
    source.addEventListener("error", () => {
      if (dead || source !== es) return;
      handlers.onLink(opened ? "reconnecting" : "connecting");
      if (source.readyState === EventSource.CLOSED) {
        const delay = Math.min(10_000, 500 * 2 ** attempt);
        attempt += 1;
        retryTimer = window.setTimeout(connect, delay);
      }
    });
    source.addEventListener("change", (ev) => {
      if (dead || source !== es) return;
      // Transcript rows arrive as `item` events; refetching the snapshot for each would be wasted work.
      let change: RunChange = {};
      try {
        change = JSON.parse((ev as MessageEvent).data) as RunChange;
        if (change.table === "items") return;
      } catch {
        /* an unparsable frame still triggers a refresh */
      }
      handlers.onChange(change);
    });
    source.addEventListener("item", (ev) => {
      if (dead || source !== es) return;
      const raw = (ev as MessageEvent).data;
      if (typeof raw !== "string") return;
      try {
        const item = JSON.parse(raw) as Item;
        if (typeof item.id !== "number" || typeof item.workerId !== "string" || typeof item.kind !== "string") return;
        handlers.onItem(item);
      } catch {
        /* ignore a bad frame */
      }
    });
  };

  handlers.onLink("connecting");
  connect();
  return () => {
    dead = true;
    clearRetry();
    const prev = es;
    es = null;
    prev?.close();
  };
}
