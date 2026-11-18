import { useCallback, useEffect, useRef, useState } from "react";
import { fetchAiStatus, fetchDaemonEvents, fetchDaemonHealth, testAiConnection } from "../api";
import { fmtCost, fmtTokens } from "../format";
import { useT } from "../i18n";
import { clockTime, relTime } from "../time";
import type {
  DaemonAiProbeResult,
  DaemonAiStatus,
  DaemonEvent,
  DaemonEventCategory,
  DaemonEventStatus,
  DaemonHealth,
  Run,
  RunSnapshot,
} from "../types";
import { mergeDaemonEventsChronological, sortDaemonEventsChronological, type CoordSection } from "../workbench";
import { WorkbenchEmpty, WorkbenchHeader } from "./WorkbenchShell";

const EVENT_PAGE_SIZE = 60;

export function DaemonView({
  run,
  snap,
  daemonEpoch,
  onOpenTask,
  onOpenWorker,
  onOpenCoordinator,
}: {
  run: Run | null;
  snap: RunSnapshot | null;
  daemonEpoch: number;
  onOpenTask: (taskId: string) => void;
  onOpenWorker: (workerId: string) => void;
  onOpenCoordinator: (tab?: CoordSection) => void;
}) {
  const { t } = useT();
  const [health, setHealth] = useState<DaemonHealth | null>(null);
  const [aiStatus, setAiStatus] = useState<DaemonAiStatus | null>(null);
  const [events, setEvents] = useState<DaemonEvent[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [probing, setProbing] = useState(false);
  const [probeResult, setProbeResult] = useState<DaemonAiProbeResult | null>(null);

  const [scope, setScope] = useState<"run" | "global">(() => (run ? "run" : "global"));
  const [category, setCategory] = useState<"all" | DaemonEventCategory>("all");
  const [statusFilter, setStatusFilter] = useState<"all" | DaemonEventStatus>("all");

  useEffect(() => {
    if (!run && scope === "run") setScope("global");
  }, [run, scope]);

  const effectiveRunId = scope === "run" && run ? run.id : undefined;
  const filterKey = `${effectiveRunId ?? "*"}|${category}|${statusFilter}`;
  const activeFilterRef = useRef(filterKey);
  const prevFilterKeyRef = useRef<string | null>(null);
  const loadedOlderRef = useRef(false);

  const loadAll = useCallback(async (reset = false) => {
    const keyAtStart = filterKey;
    setError(null);
    try {
      const [h, ai, ev] = await Promise.all([
        fetchDaemonHealth(),
        fetchAiStatus(),
        fetchDaemonEvents({
          runId: effectiveRunId,
          category: category === "all" ? undefined : category,
          status: statusFilter === "all" ? undefined : statusFilter,
          limit: EVENT_PAGE_SIZE,
        }),
      ]);
      if (activeFilterRef.current !== keyAtStart) return;
      setHealth(h);
      setAiStatus(ai);
      if (reset || !loadedOlderRef.current) {
        setEvents(sortDaemonEventsChronological(ev.events));
        setHasMore(ev.hasMore);
      } else {
        setEvents((prev) => mergeDaemonEventsChronological(prev, ev.events));
      }
    } catch (e) {
      if (activeFilterRef.current === keyAtStart) {
        setError((e as Error).message);
      }
    } finally {
      if (activeFilterRef.current === keyAtStart) {
        setLoading(false);
      }
    }
  }, [effectiveRunId, category, statusFilter, filterKey]);

  useEffect(() => {
    const filterChanged = prevFilterKeyRef.current !== filterKey;
    prevFilterKeyRef.current = filterKey;
    activeFilterRef.current = filterKey;
    if (filterChanged) {
      loadedOlderRef.current = false;
      setLoading(true);
    }
    void loadAll(filterChanged);
  }, [filterKey, loadAll, daemonEpoch]);

  useEffect(() => {
    const onFocus = () => { void loadAll(false); };
    const onVis = () => {
      if (document.visibilityState === "visible") void loadAll(false);
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVis);
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void loadAll(false);
    }, 5_000);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVis);
      window.clearInterval(timer);
    };
  }, [loadAll]);

  async function handleProbe() {
    if (probing) return;
    setProbing(true);
    setProbeResult(null);
    try {
      const res = await testAiConnection();
      setProbeResult(res);
      await loadAll(false);
    } catch (e) {
      setProbeResult({ ok: false, latencyMs: 0, model: aiStatus?.model ?? "", error: (e as Error).message });
    } finally {
      setProbing(false);
    }
  }

  async function handleLoadMore() {
    if (loadingMore || !hasMore || events.length === 0) return;
    const keyAtStart = filterKey;
    const before = events.reduce((min, item) => (item.id < min ? item.id : min), events[0]!.id);
    setLoadingMore(true);
    try {
      const next = await fetchDaemonEvents({
        runId: effectiveRunId,
        category: category === "all" ? undefined : category,
        status: statusFilter === "all" ? undefined : statusFilter,
        before,
        limit: EVENT_PAGE_SIZE,
      });
      if (activeFilterRef.current !== keyAtStart) return;
      loadedOlderRef.current = true;
      setEvents((prev) => mergeDaemonEventsChronological(prev, next.events));
      setHasMore(next.hasMore);
    } catch (e) {
      if (activeFilterRef.current === keyAtStart) {
        setError((e as Error).message);
      }
    } finally {
      setLoadingMore(false);
    }
  }

  const aiUsage = run?.aiUsage;
  const knownTasks = new Set(snap?.tasks.map((task) => task.id) ?? []);
  const knownWorkers = new Set(snap?.workers.map((w) => w.id) ?? []);

  return (
    <section className="wb daemon-view" data-wb-role="daemon">
      <WorkbenchHeader
        badge={<span className="role-badge role-badge--daemon">{t("top.daemon")}</span>}
        title={t("daemon.title")}
        subtitle={t("daemon.subtitle")}
        actions={(
          <button type="button" className="btn btn--ghost btn--sm" onClick={() => { void loadAll(false); }}>
            {t("daemon.refresh")}
          </button>
        )}
      />

      <div className="daemon-view__body">
        {error && <p className="err-banner" role="alert">{error}</p>}

      <div className="daemon-cards">
        <section className="wb-card daemon-card">
          <div className="daemon-card__head">
            <h4 className="wb-card__title">{t("daemon.healthTitle")}</h4>
            <span className={`daemon-pill ${health?.ok ? "daemon-pill--ok" : "daemon-pill--bad"}`}>
              {health?.ok ? t("daemon.healthOk") : t("daemon.healthErr")}
            </span>
          </div>
          {health ? (
            <div className="daemon-kv">
              <div className="panel-field">
                <span className="panel-field__key">{t("daemon.pid")}</span>
                <span className="panel-field__val mono">{health.pid}</span>
              </div>
              {health.version && (
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.version")}</span>
                  <span className="panel-field__val mono">{health.version}</span>
                </div>
              )}
              <div className="panel-field">
                <span className="panel-field__key">{t("daemon.startedAt")}</span>
                <span className="panel-field__val" title={health.startedAt}>
                  {clockTime(health.startedAt)} · {relTime(health.startedAt)}
                </span>
              </div>
            </div>
          ) : (
            <p className="hint">{loading ? "…" : t("daemon.healthErr")}</p>
          )}
        </section>

        <section className="wb-card daemon-card">
          <div className="daemon-card__head">
            <h4 className="wb-card__title">{t("daemon.aiTitle")}</h4>
            {aiStatus && (
              <span className={`daemon-pill ${aiStatus.enabled && aiStatus.configured ? "daemon-pill--ok" : "daemon-pill--warn"}`}>
                {!aiStatus.enabled ? t("daemon.aiDisabled") : aiStatus.configured ? t("daemon.aiEnabled") : t("daemon.aiUnconfigured")}
              </span>
            )}
          </div>
          {aiStatus && (
            <>
              <div className="daemon-kv">
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.model")}</span>
                  <span className="panel-field__val mono">{aiStatus.model || t("daemon.paramAuto")}</span>
                </div>
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.effort")}</span>
                  <span className="panel-field__val mono">
                    {(() => {
                      const eff = (aiStatus.effort || "low").toLowerCase();
                      const key = `daemon.effort.${eff}`;
                      const label = t(key);
                      return label !== key ? label : eff;
                    })()}
                  </span>
                </div>
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.context")}</span>
                  <span className="panel-field__val mono">
                    {aiStatus.contextWindow ? aiStatus.contextWindow.toUpperCase() : t("daemon.paramAuto")}
                  </span>
                </div>
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.maxTokens")}</span>
                  <span className="panel-field__val mono">
                    {aiStatus.maxTokens ? String(aiStatus.maxTokens) : t("daemon.paramAuto")}
                  </span>
                </div>
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.endpoint")}</span>
                  <span className="panel-field__val mono">{aiStatus.baseUrl}</span>
                </div>
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.keySource")}</span>
                  <span className="panel-field__val mono" title={aiStatus.keyFilePath}>{aiStatus.keySource}</span>
                </div>
                <div className="panel-field">
                  <span className="panel-field__key">{t("daemon.circuit")}</span>
                  <span className="panel-field__val">
                    <span className={`daemon-pill ${aiStatus.circuitOpen ? "daemon-pill--bad" : "daemon-pill--ok"}`}>
                      {aiStatus.circuitOpen
                        ? `${t("daemon.circuitOpen")}${aiStatus.cooldownUntil ? ` (${clockTime(aiStatus.cooldownUntil)})` : ""}`
                        : t("daemon.circuitOk")}
                    </span>
                    {aiStatus.consecutiveFailures > 0 && (
                      <span className="hint"> · {t("daemon.failuresCount", { n: aiStatus.consecutiveFailures })}</span>
                    )}
                  </span>
                </div>
              </div>
              {aiStatus.lastError && <p className="hint daemon-card__err mono">{aiStatus.lastError}</p>}
              <div className="daemon-card__actions">
                <button
                  type="button"
                  className="btn btn--primary btn--sm"
                  disabled={probing}
                  onClick={() => { void handleProbe(); }}
                >
                  {probing ? t("daemon.testingBtn") : t("daemon.testBtn")}
                </button>
                {probeResult && (
                  <span className={`hint ${probeResult.ok ? "daemon-probe--ok" : "daemon-probe--bad"}`}>
                    {probeResult.ok
                      ? t("daemon.probeOk", { ms: probeResult.latencyMs, model: probeResult.model }) + (probeResult.reply ? ` (${probeResult.reply})` : "")
                      : t("daemon.probeFail", { error: probeResult.error ?? "error" })}
                  </span>
                )}
              </div>
            </>
          )}
        </section>

        <section className="wb-card daemon-card">
          <div className="daemon-card__head">
            <h4 className="wb-card__title">{t("daemon.policiesTitle")}</h4>
          </div>
          {aiStatus ? (
            <div className="daemon-switches">
              <div className="daemon-switch">
                <span>{t("daemon.pol.verifyDigest")}</span>
                <span className={`daemon-pill ${aiStatus.switches.verifyDigest ? "daemon-pill--ok" : "daemon-pill--muted"}`}>
                  {aiStatus.switches.verifyDigest ? "on" : "off"}
                </span>
              </div>
              <div className="daemon-switch">
                <span>{t("daemon.pol.handoverCompact")}</span>
                <span className={`daemon-pill ${aiStatus.switches.handoverCompact ? "daemon-pill--ok" : "daemon-pill--muted"}`}>
                  {aiStatus.switches.handoverCompact ? "on" : "off"}
                </span>
              </div>
              <div className="daemon-switch">
                <span>{t("daemon.pol.autoTitle")}</span>
                <span className={`daemon-pill ${aiStatus.switches.autoTitle ? "daemon-pill--ok" : "daemon-pill--muted"}`}>
                  {aiStatus.switches.autoTitle ? "on" : "off"}
                </span>
              </div>
              <div className="daemon-switch">
                <span>{t("daemon.pol.taskEnrich")}</span>
                <span className={`daemon-pill ${aiStatus.switches.taskEnrich ? "daemon-pill--ok" : "daemon-pill--muted"}`}>
                  {aiStatus.switches.taskEnrich ? "on" : "off"}
                </span>
              </div>
              <div className="daemon-switch">
                <span>{t("daemon.pol.conflictPolicy")}</span>
                <span className="daemon-pill daemon-pill--info mono">{aiStatus.switches.conflictPolicy}</span>
              </div>
              <div className="daemon-switch">
                <span>{t("daemon.pol.questionMode")}</span>
                <span className="daemon-pill daemon-pill--info mono">{aiStatus.switches.questionMode}</span>
              </div>
              <div className="daemon-switch">
                <span>{t("daemon.pol.loopDetect")}</span>
                <span className="daemon-pill daemon-pill--info mono">{aiStatus.switches.loopDetect}</span>
              </div>
            </div>
          ) : (
            <p className="hint">—</p>
          )}
        </section>

        <section className="wb-card daemon-card">
          <div className="daemon-card__head">
            <h4 className="wb-card__title">{t("daemon.runUsageTitle")}</h4>
            {run && <span className="mono hint">{run.id}</span>}
          </div>
          {!run ? (
            <p className="hint">{t("daemon.runUsageNoRun")}</p>
          ) : aiUsage && aiUsage.calls > 0 ? (
            <div className="daemon-kv">
              <div className="panel-field">
                <span className="panel-field__key">{t("daemon.calls")}</span>
                <span className="panel-field__val mono">{aiUsage.calls}</span>
              </div>
              <div className="panel-field">
                <span className="panel-field__key">Tokens</span>
                <span className="panel-field__val mono">
                  {t("daemon.tokensSummary", {
                    in: fmtTokens(aiUsage.inputTokens),
                    hit: fmtTokens(aiUsage.cacheHitTokens),
                    out: fmtTokens(aiUsage.outputTokens),
                  })}
                </span>
              </div>
              <div className="panel-field">
                <span className="panel-field__key">{t("wb.cost")}</span>
                <span className="panel-field__val mono">{fmtCost(aiUsage.cost)}</span>
              </div>
              <p className="hint">{t("daemon.runUsageHint")}</p>
            </div>
          ) : (
            <>
              <p className="hint">{t("daemon.runUsageEmpty")}</p>
              <p className="hint">{t("daemon.runUsageHint")}</p>
            </>
          )}
        </section>
      </div>

      <section className="wb-card daemon-events">
        <div className="daemon-events__head">
          <h4 className="wb-card__title">{t("daemon.eventsTitle")}</h4>
          <div className="daemon-events__filters">
            {run && (
              <div className="seg seg--xs" role="group">
                <button type="button" className={scope === "run" ? "active" : ""} onClick={() => setScope("run")}>
                  {t("daemon.scopeRun")}
                </button>
                <button type="button" className={scope === "global" ? "active" : ""} onClick={() => setScope("global")}>
                  {t("daemon.scopeGlobal")}
                </button>
              </div>
            )}
            <div className="wb-filters">
              {(["all", "ai", "gate", "schedule"] as const).map((cat) => (
                <button
                  key={cat}
                  type="button"
                  className={`wb-filter wb-filter--sm${category === cat ? " wb-filter--active" : ""}`}
                  onClick={() => setCategory(cat)}
                >
                  {t(`daemon.cat.${cat}`)}
                </button>
              ))}
            </div>
            <div className="wb-filters">
              {(["all", "ok", "warn", "fallback", "failed"] as const).map((st) => (
                <button
                  key={st}
                  type="button"
                  className={`wb-filter wb-filter--sm${statusFilter === st ? " wb-filter--active" : ""}`}
                  onClick={() => setStatusFilter(st)}
                >
                  {t(`daemon.st.${st}`)}
                </button>
              ))}
            </div>
          </div>
        </div>

        {loading && events.length === 0 ? (
          <p className="hint">…</p>
        ) : events.length === 0 ? (
          <WorkbenchEmpty title={t("daemon.eventsTitle")} body={t("daemon.eventsEmpty")} />
        ) : (
          <>
            {hasMore && (
              <div className="daemon-events__more">
                <button type="button" className="btn btn--ghost btn--sm" disabled={loadingMore} onClick={() => { void handleLoadMore(); }}>
                  {loadingMore ? t("daemon.loadingMore") : t("daemon.loadMore")}
                </button>
              </div>
            )}
            <div className="daemon-events__list">
              {events.map((ev) => {
                const inCurrentRun = Boolean(run && ev.runId === run.id);
                return (
                  <article key={ev.id} className={`daemon-event daemon-event--${ev.status}`}>
                    <div className="daemon-event__top">
                      <span className={`daemon-pill daemon-pill--st-${ev.status}`}>{t(`daemon.st.${ev.status}`)}</span>
                      <span className="micro-badge">{t(`daemon.cat.${ev.category}`)}</span>
                      <span className="micro-badge">{t(`daemon.kind.${ev.kind}`)}</span>
                      <strong className="daemon-event__title">{ev.title}</strong>
                      <span className="hint daemon-event__time" title={ev.ts}>{clockTime(ev.ts)}</span>
                    </div>

                    <div className="daemon-event__meta">
                      {ev.durationMs != null && <span className="hint mono">{ev.durationMs}ms</span>}
                      {ev.tokens && (
                        <span className="hint mono">
                          in={ev.tokens.input}(hit={ev.tokens.cacheHit}) out={ev.tokens.output}
                        </span>
                      )}
                      {ev.cost != null && ev.cost > 0 && <span className="hint mono">{fmtCost(ev.cost)}</span>}
                      {ev.runId && !inCurrentRun && <span className="hint mono">{t("daemon.otherRunHint", { id: ev.runId })}</span>}
                      {ev.taskId && inCurrentRun && knownTasks.has(ev.taskId) && (
                        <button type="button" className="link-btn" onClick={() => onOpenTask(ev.taskId!)}>
                          {t("daemon.jumpTask")} {ev.taskId}
                        </button>
                      )}
                      {ev.workerId && inCurrentRun && knownWorkers.has(ev.workerId) && (
                        <button type="button" className="link-btn" onClick={() => onOpenWorker(ev.workerId!)}>
                          {t("daemon.jumpWorker")} {ev.workerId}
                        </button>
                      )}
                      {ev.messageId && inCurrentRun && (
                        <button type="button" className="link-btn" onClick={() => onOpenCoordinator("messages")}>
                          {t("daemon.jumpMsg")}
                        </button>
                      )}
                    </div>

                    {ev.detail && (
                      <details className="daemon-event__detail">
                        <summary className="hint">{ev.detail.split("\n")[0]?.slice(0, 140)}</summary>
                        <pre className="stream__pre">{ev.detail}</pre>
                      </details>
                    )}
                  </article>
                );
              })}
            </div>
          </>
        )}
      </section>
      </div>
    </section>
  );
}
