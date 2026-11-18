import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { fetchCoordinatorTranscript } from "../api";
import { fmtTokens } from "../format";
import { useT } from "../i18n";
import { isMock } from "../mock";
import { coordinatorHostLabel } from "../relate";
import { clockTime, relTime } from "../time";
import type { CoordinatorTranscriptSlice, HostTranscriptItem, Run } from "../types";
import { filterStreamItems, hostUserItemMeta, isDialogueItem, parseHostToolText } from "../workbench";
import { StreamDetailToggle, useStreamFullDetails } from "./WorkerTranscript";
import { WorkbenchEmpty } from "./WorkbenchShell";

const HOST_PAGE = 500;
const MAX_INITIAL_DIALOGUE_PAGES = 3;

type HostTranscriptMeta = Omit<CoordinatorTranscriptSlice, "items">;

function extractMeta(slice: CoordinatorTranscriptSlice): HostTranscriptMeta {
  return {
    available: slice.available,
    host: slice.host,
    agent: slice.agent,
    hostSession: slice.hostSession,
    match: slice.match,
    sourceFile: slice.sourceFile,
    models: slice.models,
    tokens: slice.tokens,
    firstTs: slice.firstTs,
    lastTs: slice.lastTs,
    total: slice.total,
    start: slice.start,
    hasMore: slice.hasMore,
  };
}

export function HostItemRow({ item }: { item: HostTranscriptItem }) {
  const { t } = useT();
  const userMeta = item.kind === "user"
    ? hostUserItemMeta(item.text, {
        user: t("item.user"),
        coNotify: t("item.coNotify"),
        contextSummary: t("item.contextSummary"),
      })
    : null;
  const toolMeta = item.kind === "tool"
    ? (item.title ? { title: item.title, detail: item.detail ?? null } : parseHostToolText(item.text))
    : null;

  return (
    <li className={`stream__item stream__item--${item.kind}`}>
      <div className="stream__head">
        <span className="stream__kind">{userMeta ? userMeta.kindLabel : t(`item.${item.kind}`)}</span>
        {item.status && <span className="mono">{item.status}</span>}
        {item.ts && (
          <span className="stream__time" title={clockTime(item.ts)}>
            {relTime(item.ts)}
          </span>
        )}
      </div>
      {item.kind === "thought" ? (
        <details>
          <summary>{item.text?.slice(0, 42) || t("item.thought")}</summary>
          <p className="stream__text">{item.text}</p>
        </details>
      ) : item.kind === "user" && userMeta?.summary ? (
        <details>
          <summary>{userMeta.summary}</summary>
          <p className="stream__text">{item.text}</p>
        </details>
      ) : item.kind === "tool" && toolMeta ? (
        <div>
          <div className="stream__text"><b>{toolMeta.title}</b></div>
          {toolMeta.detail && <div className="stream__locs">{toolMeta.detail}</div>}
        </div>
      ) : item.text ? (
        <p className="stream__text">{item.text}</p>
      ) : null}
    </li>
  );
}

export function HostTranscript({
  run,
  variant = "panel",
}: {
  run: Run;
  variant?: "panel" | "workbench";
}) {
  const { t } = useT();
  const mock = isMock();
  const [showFull, setShowFull] = useStreamFullDetails();
  const showFullRef = useRef(showFull);
  showFullRef.current = showFull;

  const [meta, setMeta] = useState<HostTranscriptMeta | null>(null);
  const [itemsByIndex, setItemsByIndex] = useState<Map<number, HostTranscriptItem>>(() => new Map());
  const [loading, setLoading] = useState(true);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const streamRef = useRef<HTMLUListElement>(null);
  const stick = useRef(true);
  const historyScroll = useRef<{ top: number; height: number } | null>(null);
  const generation = useRef(0);

  useEffect(() => {
    const gen = ++generation.current;
    let dead = false;
    setMeta(null);
    setItemsByIndex(new Map());
    setErr(null);
    stick.current = true;
    setLoading(true);

    void (async () => {
      try {
        const first = await fetchCoordinatorTranscript(run.id, { limit: HOST_PAGE });
        if (dead || gen !== generation.current) return;
        const map = new Map<number, HostTranscriptItem>();
        for (const item of first.items) map.set(item.index, item);

        if (first.available && !showFullRef.current) {
          let oldest = first.items[0]?.index ?? first.start;
          let pages = 1;
          while (![...map.values()].some(isDialogueItem) && oldest > 0 && pages < MAX_INITIAL_DIALOGUE_PAGES) {
            const older = await fetchCoordinatorTranscript(run.id, { before: oldest, limit: HOST_PAGE });
            if (dead || gen !== generation.current) return;
            if (older.items.length === 0) break;
            for (const item of older.items) map.set(item.index, item);
            oldest = Math.min(oldest, older.items[0]?.index ?? older.start);
            pages += 1;
          }
        }

        if (dead || gen !== generation.current) return;
        setMeta(extractMeta(first));
        setItemsByIndex(map);
        setErr(null);
      } catch (e) {
        if (!dead && gen === generation.current) setErr((e as Error).message);
      } finally {
        if (!dead && gen === generation.current) setLoading(false);
      }
    })();

    return () => {
      dead = true;
      ++generation.current;
    };
  }, [run.id]);

  async function pullTail(manual: boolean) {
    const gen = generation.current;
    if (manual) {
      setRefreshing(true);
      stick.current = true;
    }
    try {
      const slice = await fetchCoordinatorTranscript(run.id, { limit: HOST_PAGE });
      if (gen !== generation.current) return;
      setMeta(extractMeta(slice));
      setItemsByIndex((prev) => {
        const prevMax = prev.size ? Math.max(...prev.keys()) : -1;
        const next = slice.total <= prevMax ? new Map<number, HostTranscriptItem>() : new Map(prev);
        for (const item of slice.items) next.set(item.index, item);
        return next;
      });
      setErr(null);
    } catch (e) {
      if (gen === generation.current) setErr((e as Error).message);
    } finally {
      if (manual && gen === generation.current) setRefreshing(false);
    }
  }

  useEffect(() => {
    if (mock || run.status !== "active") return;
    let dead = false;
    const timer = window.setInterval(() => {
      if (!dead && document.visibilityState === "visible") {
        void pullTail(false);
      }
    }, 10_000);
    return () => {
      dead = true;
      window.clearInterval(timer);
    };
  }, [run.id, run.status, mock]);

  const items = useMemo(
    () => [...itemsByIndex.values()].sort((a, b) => a.index - b.index),
    [itemsByIndex],
  );
  const oldestIndex = items.length > 0 ? items[0].index : (meta?.start ?? 0);
  const hasEarlier = Boolean(meta?.available) && oldestIndex > 0;

  const { visible: displayed, hiddenCount } = useMemo(
    () => filterStreamItems(items, showFull),
    [items, showFull],
  );

  useLayoutEffect(() => {
    const stream = streamRef.current;
    if (!stream) return;
    const history = historyScroll.current;
    if (history) {
      stream.scrollTop = history.top + stream.scrollHeight - history.height;
      historyScroll.current = null;
    } else if (stick.current) {
      stream.scrollTop = stream.scrollHeight;
    }
  }, [displayed, showFull]);

  async function loadEarlier() {
    if (!hasEarlier || loading || loadingEarlier) return;
    const gen = generation.current;
    stick.current = false;
    const stream = streamRef.current;
    if (stream) historyScroll.current = { top: stream.scrollTop, height: stream.scrollHeight };
    setLoadingEarlier(true);
    try {
      const older = await fetchCoordinatorTranscript(run.id, { before: oldestIndex, limit: HOST_PAGE });
      if (gen !== generation.current) return;
      setItemsByIndex((prev) => {
        const next = new Map(prev);
        for (const item of older.items) next.set(item.index, item);
        return next;
      });
      setErr(null);
    } catch (e) {
      if (gen === generation.current) {
        historyScroll.current = null;
        setErr((e as Error).message);
      }
    } finally {
      if (gen === generation.current) setLoadingEarlier(false);
    }
  }

  const streamBody = (
    <div className="wb-stream">
      <div className="wb-stream__bar">
        <div className="wb-actions">
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            disabled={!hasEarlier || loading || loadingEarlier}
            onClick={() => void loadEarlier()}
          >
            {loadingEarlier ? t("panel.loading") : hasEarlier ? t("panel.loadEarlier") : t("panel.noEarlier")}
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            disabled={loading || refreshing}
            onClick={() => void pullTail(true)}
          >
            {refreshing ? t("panel.loading") : t("wb.refresh")}
          </button>
        </div>
        {variant === "workbench" && (
          <StreamDetailToggle showFull={showFull} onChange={setShowFull} hiddenCount={hiddenCount} />
        )}
      </div>

      {err && <p className="hint" style={{ color: "var(--danger)" }}>{err}</p>}

      {items.length === 0 && !loading ? (
        <p className="hint">{t("panel.streamEmpty")}</p>
      ) : displayed.length === 0 && !loading ? (
        <p className="hint">{t("panel.streamHiddenOnly", { n: hiddenCount })}</p>
      ) : (
        <ul
          ref={streamRef}
          className={`stream${variant === "workbench" ? " wb-stream__list" : ""}`}
          onScroll={(e) => {
            const s = e.currentTarget;
            stick.current = s.scrollHeight - s.clientHeight - s.scrollTop <= 24;
          }}
        >
          {displayed.map((item) => <HostItemRow key={item.index} item={item} />)}
        </ul>
      )}
    </div>
  );

  const effectiveAgent = meta?.agent || run.coordinator.agent;
  const effectiveModels = meta?.models?.length
    ? meta.models
    : (run.coordinator.model ? [run.coordinator.model] : []);

  if (variant === "workbench") {
    return (
      <div className="wb-card">
        <div className="wb-card__head">
          <div>
            <h3 className="wb-card__title">{t("wb.coordHost")}</h3>
            <p className="hint">
              {coordinatorHostLabel(run, meta?.host)} · <span className="mono">{run.coordinator.hostSession}</span>
              {meta?.sourceFile ? ` · ${meta.sourceFile}` : ""}
              {meta?.match ? ` · ${meta.match}` : ""}
            </p>
          </div>
        </div>

        {(meta?.available || effectiveAgent || effectiveModels.length > 0) && (
          <div className="wb-chips">
            {meta?.available && <span className="micro-badge">{t("wb.totalItems", { n: meta.total })}</span>}
            {effectiveAgent && <span className="micro-badge mono">{effectiveAgent}</span>}
            {effectiveModels.map((m) => (
              <span key={m} className="micro-badge mono">{m}</span>
            ))}
            {meta?.tokens && (
              <span className="micro-badge">
                In {fmtTokens(meta.tokens.input)} · Out {fmtTokens(meta.tokens.output)} · Cache {fmtTokens(meta.tokens.cacheRead)}
              </span>
            )}
          </div>
        )}

        {!meta?.available && err && <p className="hint" style={{ color: "var(--danger)" }}>{err}</p>}

        {loading && items.length === 0 ? (
          <p className="hint">{t("panel.loading")}</p>
        ) : !meta?.available ? (
          <WorkbenchEmpty title={t("wb.hostUnavailableTitle")} body={t("wb.hostUnavailableBody")} />
        ) : (
          streamBody
        )}
      </div>
    );
  }

  return (
    <section className="panel-group">
      <div className="panel-group__head">
        <span className="panel-group__title">{t("wb.coordHost")}</span>
        {meta?.available && (
          <StreamDetailToggle showFull={showFull} onChange={setShowFull} hiddenCount={hiddenCount} />
        )}
      </div>
      {(effectiveAgent || effectiveModels.length > 0) && (
        <div className="wb-chips" style={{ marginBottom: 6 }}>
          {effectiveAgent && <span className="micro-badge mono">{effectiveAgent}</span>}
          {effectiveModels.map((m) => (
            <span key={m} className="micro-badge mono">{m}</span>
          ))}
        </div>
      )}
      {!meta?.available && err && <p className="hint" style={{ color: "var(--danger)" }}>{err}</p>}
      {loading && items.length === 0 ? (
        <p className="hint">{t("panel.loading")}</p>
      ) : !meta?.available ? (
        <p className="hint">{t("wb.hostUnavailableTitle")} — {t("wb.hostUnavailableBody")}</p>
      ) : (
        streamBody
      )}
    </section>
  );
}
