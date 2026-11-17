import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { fetchItems } from "../api";
import { LIVE_ITEMS_PER_WORKER } from "../live";
import { fmtCost, fmtTokens, jsonPreview } from "../format";
import { useT } from "../i18n";
import { type ReviewRoundView } from "../review";
import { clockTime, relTime } from "../time";
import type { Attempt, Item, Task, Worker } from "../types";
import { filterStreamItems, isDialogueItem } from "../workbench";

const PAGE = 200;
const STREAM_FULL_DETAILS_KEY = "co-web:stream-full-details";
const streamFullListeners = new Set<(value: boolean) => void>();

function readStreamFullDetails(): boolean {
  try {
    return localStorage.getItem(STREAM_FULL_DETAILS_KEY) === "1";
  } catch {
    return false;
  }
}

function writeStreamFullDetails(next: boolean): void {
  try {
    localStorage.setItem(STREAM_FULL_DETAILS_KEY, next ? "1" : "0");
  } catch {
    /* per session */
  }
  for (const listener of streamFullListeners) {
    listener(next);
  }
}

export function useStreamFullDetails(): [boolean, (next: boolean) => void] {
  const [showFull, setShowFull] = useState<boolean>(readStreamFullDetails);

  useEffect(() => {
    const sync = (next: boolean) => setShowFull(next);
    streamFullListeners.add(sync);
    const onStorage = (event: StorageEvent) => {
      if (event.key === STREAM_FULL_DETAILS_KEY) {
        setShowFull(event.newValue === "1");
      }
    };
    window.addEventListener("storage", onStorage);
    return () => {
      streamFullListeners.delete(sync);
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  const update = (next: boolean) => {
    setShowFull(next);
    writeStreamFullDetails(next);
  };

  return [showFull, update];
}

export function StreamDetailToggle({
  showFull,
  onChange,
  hiddenCount,
}: {
  showFull: boolean;
  onChange: (next: boolean) => void;
  hiddenCount: number;
}) {
  const { t } = useT();
  return (
    <label className={`stream-switch${showFull ? " stream-switch--on" : ""}`} title={t("panel.streamFullTip")}>
      <input
        type="checkbox"
        role="switch"
        checked={showFull}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="stream-switch__track" aria-hidden="true">
        <span className="stream-switch__thumb" />
      </span>
      <span className="stream-switch__label">{t("panel.streamFull")}</span>
      {!showFull && hiddenCount > 0 && (
        <span className="stream-switch__badge">{t("panel.streamHiddenBadge", { n: hiddenCount })}</span>
      )}
    </label>
  );
}

export type StreamRound = ReviewRoundView & { taskTitle: string };

export function userItemMeta(text: string | undefined, fallback: string): { kindLabel: string; summary: string | null } {
  if (!text) return { kindLabel: fallback, summary: null };
  const trimmed = text.trim();
  if (/^(?:任务[^\n]*的只读审查|只读(?:代码)?审查|第\s*\d+[^\n]*轮只读(?:代码)?审查|read-only (?:code )?review)/i.test(trimmed)) {
    const round = /第\s*(\d+)(?:\s*\/\s*\d+)?\s*轮/.exec(trimmed)?.[1];
    return { kindLabel: round ? `第 ${round} 轮只读审查提示词` : "只读代码审查提示词", summary: round ? `Round ${round} · 只读候选快照审查` : "只读候选快照审查" };
  }
  if (/进入返工|审查打回|继续使用原作者上下文/.test(trimmed)) {
    return { kindLabel: "返工提示（审查打回修改）", summary: trimmed.split("\n")[0] };
  }
  if (trimmed.startsWith("coordinator 插话：")) {
    return { kindLabel: "编排者插话", summary: null };
  }
  if (trimmed.includes("你的上一步被 coordinator 中断")) {
    return { kindLabel: "编排者中断插话", summary: null };
  }
  if (
    trimmed.startsWith("你上一轮已结束，但还没有调用 co worker done") ||
    trimmed.includes("尚未收到 co worker done") ||
    trimmed.includes("次收尾提醒：")
  ) {
    return { kindLabel: "编排者收尾提醒", summary: null };
  }
  if (trimmed.includes("请立即调用 co worker done")) {
    return { kindLabel: "编排者中断收尾", summary: null };
  }
  const taskMatch = /^(?:任务\s+([^\n]+)|Task:\s*(.+))$/m.exec(trimmed);
  const taskLine = (taskMatch?.[1] ?? taskMatch?.[2])?.trim() ?? null;
  if (trimmed.includes("继续使用已有上下文，执行下一任务") || trimmed.startsWith("继续在当前会话中完成下一个任务")) {
    return { kindLabel: "编排者派发（复用）", summary: taskLine ? `复用派发：${taskLine}` : "派发下一个任务" };
  }
  if (trimmed.includes("重试任务，上一 attemptId=") || trimmed.includes("重试说明：") || trimmed.includes("重试提示：")) {
    return { kindLabel: "编排者派发（重试）", summary: taskLine ? `重试派发：${taskLine}` : "重试任务提示词" };
  }
  if (trimmed.includes("Current taskId:") || trimmed.includes("你是 co 的 worker")) {
    return { kindLabel: "编排者派发", summary: taskLine ? `派发任务：${taskLine}` : "任务提示词" };
  }
  return { kindLabel: fallback, summary: trimmed.length > 220 ? `${trimmed.slice(0, 60)}…` : null };
}

export function ItemRow({ item }: { item: Item }) {
  const { t } = useT();
  const userMeta = item.kind === "user" ? userItemMeta(item.text, t("item.user")) : null;
  return (
    <li className={`stream__item stream__item--${item.kind}`}>
      <div className="stream__head">
        <span className="stream__kind">{userMeta ? userMeta.kindLabel : t(`item.${item.kind}`)}</span>
        {item.tool?.status && <span className="mono">{item.tool.status}</span>}
        <span className="stream__time" title={clockTime(item.ts)}>{relTime(item.ts)}</span>
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
      ) : item.text ? (
        <p className="stream__text">{item.text}</p>
      ) : null}
      {item.tool && (
        <div>
          <div className="stream__text"><b>{item.tool.title || item.tool.kind || item.tool.id}</b></div>
          {item.tool.locations && item.tool.locations.length > 0 && <div className="stream__locs">{item.tool.locations.join("  ")}</div>}
          {item.tool.input != null && (
            <details>
              <summary>{t("item.input")}</summary>
              <pre className="stream__pre">{jsonPreview(item.tool.input)}</pre>
            </details>
          )}
          {item.tool.output && (
            <details open>
              <summary>{t("item.output")}</summary>
              <pre className="stream__pre">{item.tool.output}</pre>
            </details>
          )}
        </div>
      )}
      {item.usage && (
        <p className="stream__text">
          {item.usage.used != null && item.usage.size ? `上下文 ${item.usage.used} / ${item.usage.size}` : t("panel.contextEmpty")}
          {" · "}
          {t("panel.tokens")} {fmtTokens((item.usage.input ?? 0) + (item.usage.output ?? 0) + (item.usage.cacheRead ?? 0))}
          {" · "}
          {fmtCost(item.usage.cost)}
        </p>
      )}
    </li>
  );
}

export function WorkerTranscript({
  worker,
  liveItems,
  streamEpoch,
  rounds,
  attempts,
  tasks,
  filterAttemptId = null,
  lockAttempt = false,
  filterTaskId = null,
  variant = "panel",
  bare = false,
  title,
}: {
  worker: Worker;
  liveItems: Item[];
  streamEpoch: number;
  rounds?: StreamRound[];
  attempts?: Attempt[];
  tasks?: Task[];
  filterAttemptId?: string | null;
  lockAttempt?: boolean;
  filterTaskId?: string | null;
  variant?: "panel" | "workbench";
  bare?: boolean;
  title?: string;
}) {
  const { t } = useT();
  const lockedAttempt = lockAttempt && filterAttemptId ? filterAttemptId : undefined;
  const scopeKey = lockedAttempt ? `${worker.id}:${lockedAttempt}` : worker.id;
  const [showFull, setShowFull] = useStreamFullDetails();
  const [page, setPage] = useState<Item[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState<string | null>(null);
  const [scopeAttempt, setScopeAttempt] = useState<string>(filterAttemptId ?? "");
  const streamRef = useRef<HTMLUListElement>(null);
  const stick = useRef(true);
  const historyScroll = useRef<{ top: number; height: number } | null>(null);
  const generation = useRef(0);
  const loadedScope = useRef<string | null>(null);
  const coveredLive = useRef(new Map<number, Item>());
  const [liveFloor, setLiveFloor] = useState(0);
  const cachedRef = useRef<Item[]>([]);

  useEffect(() => {
    setScopeAttempt(filterAttemptId ?? "");
  }, [worker.id, filterAttemptId]);

  const matchesScope = (item: Item) => item.workerId === worker.id && (!lockedAttempt || item.attemptId === lockedAttempt);
  const workerLive = liveItems.filter(matchesScope);
  const liveById = new Map(workerLive.map((item) => [item.id, item]));
  for (const [id, item] of coveredLive.current) if (liveById.get(id) !== item) coveredLive.current.delete(id);
  cachedRef.current = [...page, ...workerLive.filter((item) => coveredLive.current.get(item.id) !== item)]
    .filter((item) => matchesScope(item) && item.id >= liveFloor);

  useEffect(() => {
    const gen = ++generation.current;
    const initial = loadedScope.current !== scopeKey;
    const cached = initial ? [] : cachedRef.current;
    const cachedMax = cached.reduce((max, item) => Math.max(max, item.id), 0);
    const liveAtStart = new Map(liveItems.filter(matchesScope).map((item) => [item.id, item]));
    if (initial) {
      coveredLive.current.clear();
      setPage([]);
      setHasMore(false);
      setErr(null);
      setLiveFloor(0);
      stick.current = true;
    }
    setLoading(true);
    fetchItems(worker.id, undefined, PAGE, lockedAttempt)
      .then((fresh) => {
        if (gen !== generation.current) return;
        const rows = fresh.items.filter(matchesScope);
        // HTTP repairs items changed during the outage; only later SSE snapshots may override it.
        for (const item of rows) {
          const prior = liveAtStart.get(item.id);
          if (prior) coveredLive.current.set(item.id, prior);
        }
        const freshMin = rows.reduce((min, item) => Math.min(min, item.id), Infinity);
        if (initial || freshMin > cachedMax + 1) {
          setPage(rows);
          setLiveFloor(Number.isFinite(freshMin) ? freshMin : 0);
          setHasMore(fresh.hasMore);
        } else {
          const map = new Map(cached.map((item) => [item.id, item]));
          for (const item of rows) map.set(item.id, item);
          setPage([...map.values()]);
          setHasMore((prev) => prev || fresh.hasMore);
        }
        loadedScope.current = scopeKey;
        setErr(null);
      })
      .catch((e: Error) => {
        if (gen === generation.current) setErr(e.message);
      })
      .finally(() => {
        if (gen === generation.current) setLoading(false);
      });
    return () => {
      ++generation.current;
    };
  }, [worker.id, streamEpoch, scopeKey]);

  useEffect(() => {
    if (loading || loadedScope.current !== scopeKey) return;
    const recent = liveItems.filter((item) => matchesScope(item) && item.id >= liveFloor);
    if (recent.length < LIVE_ITEMS_PER_WORKER || page.length === 0) return;
    const liveMin = Math.min(...recent.map((item) => item.id));
    const pageMax = page.reduce((max, item) => matchesScope(item) ? Math.max(max, item.id) : max, 0);
    if (liveMin <= pageMax + 1) return;
    setPage(recent);
    setLiveFloor(liveMin);
    setHasMore(true);
  }, [liveItems, worker.id, scopeKey, page, liveFloor, loading]);

  const merged = useMemo(() => {
    const map = new Map<number, Item>();
    for (const item of page) if (item.workerId === worker.id) map.set(item.id, item);
    for (const item of liveItems) {
      if (item.workerId === worker.id && item.id >= liveFloor && coveredLive.current.get(item.id) !== item) map.set(item.id, item);
    }
    return [...map.values()].sort((a, b) => a.id - b.id);
  }, [page, liveItems, worker.id, liveFloor]);

  const matchingAttempts = useMemo(() => {
    if (!attempts) return [] as Attempt[];
    const ofWorker = attempts.filter((a) => a.workerId === worker.id);
    if (scopeAttempt) return ofWorker.filter((a) => a.id === scopeAttempt);
    if (filterTaskId) return ofWorker.filter((a) => a.taskId === filterTaskId);
    return ofWorker;
  }, [attempts, worker.id, scopeAttempt, filterTaskId]);

  const shown = useMemo(() => {
    if (!scopeAttempt && !filterTaskId) return merged;
    if (matchingAttempts.length === 0) return [] as Item[];
    const ids = new Set(matchingAttempts.map((a) => a.id));
    return merged.filter((item) => {
      if (item.attemptId) return ids.has(item.attemptId);
      return matchingAttempts.some((a) => item.ts >= a.startedAt && (!a.endedAt || item.ts <= a.endedAt));
    });
  }, [merged, scopeAttempt, filterTaskId, matchingAttempts]);

  const { visible: displayed, hiddenCount } = useMemo(
    () => filterStreamItems(shown, showFull),
    [shown, showFull],
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
    const oldest = merged[0]?.id;
    if (oldest == null || loading) return;
    const gen = generation.current;
    stick.current = false;
    setLoading(true);
    try {
      const older = await fetchItems(worker.id, oldest, PAGE, lockedAttempt);
      if (gen !== generation.current) return;
      const rows = older.items.filter(matchesScope);
      const stream = streamRef.current;
      if (stream) historyScroll.current = { top: stream.scrollTop, height: stream.scrollHeight };
      setPage((prev) => {
        const map = new Map(prev.filter((item) => item.workerId === worker.id).map((item) => [item.id, item]));
        for (const item of rows) map.set(item.id, item);
        return [...map.values()];
      });
      setLiveFloor((floor) => rows.reduce((min, item) => Math.min(min, item.id), floor));
      setHasMore(older.hasMore);
      setErr(null);
    } catch (e) {
      if (gen === generation.current) setErr((e as Error).message);
    } finally {
      if (gen === generation.current) setLoading(false);
    }
  }

  const groups = useMemo(() => {
    const ordered = [...(rounds ?? [])].sort((a, b) => a.attempt.startedAt.localeCompare(b.attempt.startedAt));
    const buckets = ordered.map((round) => ({ key: round.attempt.id, round: round as StreamRound | undefined, items: [] as Item[], totalCount: 0 }));
    const other = { key: "session", round: undefined as StreamRound | undefined, items: [] as Item[], totalCount: 0 };
    for (const item of shown) {
      const bucket = item.attemptId
        ? buckets.find((group) => group.key === item.attemptId)
        : [...buckets].reverse().find((group) => group.round && item.ts >= group.round.attempt.startedAt && (!group.round.attempt.endedAt || item.ts <= group.round.attempt.endedAt));
      const target = bucket ?? other;
      target.totalCount += 1;
      if (showFull || isDialogueItem(item)) target.items.push(item);
    }
    return other.items.length ? [other, ...buckets] : buckets;
  }, [rounds, shown, showFull]);

  const workerAttemptList = useMemo(
    () => (attempts ? attempts.filter((a) => a.workerId === worker.id && (!filterTaskId || a.taskId === filterTaskId)) : []),
    [attempts, worker.id, filterTaskId],
  );

  const controls = (
    <div className="wb-stream__bar">
      <button type="button" className="btn btn--ghost btn--sm" disabled={!hasMore || loading} onClick={loadEarlier}>
        {loading ? t("panel.loading") : hasMore ? t("panel.loadEarlier") : t("panel.noEarlier")}
      </button>
      {variant === "workbench" && !lockAttempt && workerAttemptList.length > 1 && !rounds && (
        <div className="wb-stream__scopes">
          <button
            type="button"
            className={`wb-filter wb-filter--sm${scopeAttempt === "" ? " wb-filter--active" : ""}`}
            onClick={() => setScopeAttempt("")}
          >
            {t("wb.allAttempts")} ({merged.length})
          </button>
          {workerAttemptList.map((attempt, idx) => {
            const taskTitle = tasks?.find((item) => item.id === attempt.taskId)?.title ?? attempt.taskId;
            return (
              <button
                key={attempt.id}
                type="button"
                className={`wb-filter wb-filter--sm${scopeAttempt === attempt.id ? " wb-filter--active" : ""}`}
                onClick={() => setScopeAttempt(attempt.id)}
                title={attempt.id}
              >
                #{idx + 1} · {taskTitle}
              </button>
            );
          })}
        </div>
      )}
      {bare && <StreamDetailToggle showFull={showFull} onChange={setShowFull} hiddenCount={hiddenCount} />}
    </div>
  );

  const content = (
    <>
      {controls}
      {err && <p className="hint" style={{ color: "var(--danger)" }}>{err}</p>}
      {shown.length === 0 && !loading ? <p className="hint">{t("panel.streamEmpty")}</p>
        : displayed.length === 0 && (!rounds || groups.length === 0) && !loading ? <p className="hint">{t("panel.streamHiddenOnly", { n: hiddenCount })}</p> : (
          <ul
            ref={streamRef}
            className={`stream${rounds ? " reviewer-stream" : ""}${variant === "workbench" ? " wb-stream__list" : ""}`}
            onScroll={(e) => {
              const stream = e.currentTarget;
              stick.current = stream.scrollHeight - stream.clientHeight - stream.scrollTop <= 24;
            }}
          >
            {rounds ? groups.map((group) => (
              <li key={group.key} className="reviewer-stream__round">
                <div className="reviewer-stream__head">
                  <strong>{group.round ? `Round ${group.round.round}` : "会话级记录"}</strong>
                  {group.round && (
                    <span className="hint">
                      {" · "}
                      {group.round.taskTitle}
                      {" · "}
                      {group.round.verdict === "approved"
                        ? "通过"
                        : group.round.verdict === "changes_requested"
                          ? "要求修改"
                          : group.round.attempt.failure
                            ? "审查异常"
                            : group.round.attempt.endedAt
                              ? "已结束"
                              : "审查中"}
                    </span>
                  )}
                </div>
                {group.items.length === 0 ? (
                  !showFull && group.totalCount > 0 ? (
                    <p className="hint">{t("panel.streamHiddenOnly", { n: group.totalCount })}</p>
                  ) : (
                    <p className="hint">{hasMore || loading ? "此轮记录尚未加载，可加载更早记录。" : "尚无该轮会话记录。"}</p>
                  )
                ) : (
                  <ul className="reviewer-stream__items" style={{ listStyle: "none", padding: 0 }}>
                    {group.items.map((item) => <ItemRow key={item.id} item={item} />)}
                  </ul>
                )}
              </li>
            )) : displayed.map((item) => <ItemRow key={item.id} item={item} />)}
          </ul>
        )}
    </>
  );

  if (bare) {
    return <div className="wb-stream">{content}</div>;
  }

  return (
    <section className="panel-group">
      <div className="panel-group__head">
        <span className="panel-group__title">{title ?? (rounds ? "审查会话流（Item Stream）" : t("panel.stream"))}</span>
        <StreamDetailToggle showFull={showFull} onChange={setShowFull} hiddenCount={hiddenCount} />
      </div>
      {content}
    </section>
  );
}
