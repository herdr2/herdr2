import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fetchAgents, fetchRuns, fetchSnapshot, pauseRun, resumeRun } from "./api";
import { hasDataWhen } from "./branch";
import { authorWorkers, reviewedTasks } from "./review";
import type { DagLens } from "./dag-lens";
import { mergeLiveItem, openRunEvents, type LinkState } from "./live";
import { isMock } from "./mock";
import { AgentsView } from "./components/AgentsView";
import { CoordinatorView } from "./components/CoordinatorView";
import { DaemonView } from "./components/DaemonView";
import { COORD_ID, DagView, type CanvasViewport } from "./components/DagView";
import { NodePanel } from "./components/NodePanel";
import { OverviewBar } from "./components/OverviewBar";
import { RunSidebar } from "./components/RunSidebar";
import { ReviewsView } from "./components/ReviewsView";
import { TasksView } from "./components/TasksView";
import { WorkersView } from "./components/WorkersView";
import { setLang, useT, type Lang } from "./i18n";
import { LAYOUTS, type Item, type LayoutKind, type ModelAvailability, type Profile, type QuotaCard, type RunSummary, type RunSnapshot, type StatsTable, type TalkPlay } from "./types";
import {
  buildWorkbenchHref,
  readWorkbenchLocation,
  selectionToDetailTarget,
  type CoordSection,
  type Page,
} from "./workbench";

const SHOW_WORKERS_KEY = "co-web:show-workers";
const SHOW_REVIEWERS_KEY = "co-web:show-reviewers";
const SHOW_EXEC_KEY = "co-web:show-exec";
const SHOW_REVIEW_EDGES_KEY = "co-web:show-review-edges";
const SIDEBAR_KEY = "co-web:sidebar-collapsed";
const LAYOUT_KEY = "co-web:layout";
const RUN_KEY = "co-web:run";
const THEME_KEY = "co-web:theme";
type Theme = "system" | "light" | "dark";

function writeLocation(
  patch: Parameters<typeof buildWorkbenchHref>[1],
  mode: "push" | "replace" = "push",
): void {
  const next = buildWorkbenchHref(window.location.href, patch);
  const cur = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  if (next !== cur) {
    // Always write null state for cross-page routes so DAG Esc never pops into another workbench.
    if (mode === "push") history.pushState(null, "", next);
    else history.replaceState(null, "", next);
  }
}

function readFlag(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}

function writeFlag(key: string, value: boolean): void {
  try {
    localStorage.setItem(key, value ? "1" : "0");
  } catch {
    /* per session */
  }
}

function readLayout(): LayoutKind {
  try {
    const v = localStorage.getItem(LAYOUT_KEY);
    return v === "layered-tb" ? "layered-tb" : "layered-lr";
  } catch {
    return "layered-lr";
  }
}

function readTheme(): Theme {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

function readSel(): string | null {
  try {
    return new URLSearchParams(window.location.search).get("sel");
  } catch {
    return null;
  }
}

function writeRun(id: string, clearSelection = false): void {
  const url = new URL(window.location.href);
  if (id) url.searchParams.set("run", id);
  else url.searchParams.delete("run");
  if (clearSelection) {
    url.searchParams.delete("sel");
    url.searchParams.delete("task");
    url.searchParams.delete("worker");
    url.searchParams.delete("tab");
  }
  history.replaceState(clearSelection ? null : history.state, "", `${url.pathname}${url.search}${url.hash}`);
}

type CoHist = { coSel: true; coSeq: number };

function isCoHist(v: unknown): v is CoHist {
  return !!v && typeof v === "object" && (v as CoHist).coSel === true && typeof (v as CoHist).coSeq === "number";
}

/** Push or replace `?sel=`. Returns false when the URL is already that value, unless `force`. */
function writeSel(id: string | null, mode: "push" | "replace", state?: unknown, force = false): boolean {
  const url = new URL(window.location.href);
  if (id) url.searchParams.set("sel", id);
  else url.searchParams.delete("sel");
  const next = `${url.pathname}${url.search}${url.hash}`;
  const cur = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  if (next === cur && !force) return false;
  const hist = state === undefined ? window.history.state : state;
  if (mode === "push") history.pushState(hist, "", next);
  else history.replaceState(hist, "", next);
  return true;
}

function typingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) return true;
  if (target.tagName === "IFRAME") return true;
  return Boolean(target.closest("input, textarea, select, iframe, [contenteditable]:not([contenteditable='false'])"));
}

/** Menus and dialogs consume Esc themselves. The canvas must not also step back. */
function escOverlayOpen(): boolean {
  return Boolean(document.querySelector(".run-menu, .run-confirm, .doodle-select__panel, [role='menu'], [role='dialog']"));
}

function applyTheme(theme: Theme): void {
  if (theme === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.dataset.theme = theme;
}

function sortRunSummaries(list: RunSummary[]): RunSummary[] {
  const ranked = (run: RunSummary) => typeof run.order === "number" && Number.isFinite(run.order);
  return [...list].sort((a, b) => {
    const ar = ranked(a);
    const br = ranked(b);
    if (ar && br && a.order !== b.order) return a.order! - b.order!;
    if (ar && !br) return -1;
    if (!ar && br) return 1;
    return b.createdAt.localeCompare(a.createdAt);
  });
}

function depCount(snap: RunSnapshot): number {
  const ids = new Set(snap.tasks.map((task) => task.id));
  let n = 0;
  for (const task of snap.tasks) for (const dep of task.deps) if (ids.has(dep)) n++;
  return n;
}

function LegendArrow({ kind }: { kind: "dispatch" | "executes" | "depends" | "branch" | "review" | "fork" }) {
  return (
    <svg className={`edge-legend__arrow edge-legend__arrow--${kind}`} width="30" height="10" aria-hidden="true">
      <line x1="1" y1="5" x2="23" y2="5" />
      <path d="M22 1.5 L29 5 L22 8.5 Z" />
    </svg>
  );
}

export default function App() {
  const { t, lang } = useT();
  const mock = isMock();
  const [page, setPageState] = useState<Page>(() => readWorkbenchLocation(window.location.search).page);
  const [selectedTaskId, setSelectedTaskIdState] = useState<string | null>(() => readWorkbenchLocation(window.location.search).taskId);
  const [selectedWorkerId, setSelectedWorkerIdState] = useState<string | null>(() => readWorkbenchLocation(window.location.search).workerId);
  const [reviewTaskId, setReviewTaskIdState] = useState<string | null>(() => readWorkbenchLocation(window.location.search).reviewTaskId);
  const [coordTab, setCoordTabState] = useState<CoordSection>(() => readWorkbenchLocation(window.location.search).coordTab);
  const setPage = useCallback((next: Page) => {
    writeLocation({
      page: next,
      taskId: next === "tasks" ? selectedTaskId : null,
      reviewTaskId: next === "reviews" ? reviewTaskId : null,
      workerId: next === "workers" ? selectedWorkerId : null,
      coordTab: next === "coordinator" ? coordTab : undefined,
    });
    setPageState(next);
  }, [selectedTaskId, reviewTaskId, selectedWorkerId, coordTab]);
  const setReviewTaskId = useCallback((id: string | null) => {
    const hadTask = Boolean(readWorkbenchLocation(window.location.search).reviewTaskId);
    writeLocation({ page: "reviews", reviewTaskId: id }, hadTask ? "push" : "replace");
    setReviewTaskIdState(id);
  }, []);
  const setSelectedTaskId = useCallback((id: string | null) => {
    const hadTask = Boolean(readWorkbenchLocation(window.location.search).taskId);
    writeLocation({ page: "tasks", taskId: id }, hadTask ? "push" : "replace");
    setSelectedTaskIdState(id);
  }, []);
  const setSelectedWorkerId = useCallback((id: string | null) => {
    const hadWorker = Boolean(readWorkbenchLocation(window.location.search).workerId);
    writeLocation({ page: "workers", workerId: id }, hadWorker ? "push" : "replace");
    setSelectedWorkerIdState(id);
  }, []);
  const setCoordTab = useCallback((next: CoordSection) => {
    writeLocation({ page: "coordinator", coordTab: next }, "push");
    setCoordTabState(next);
  }, []);
  const openCoordinator = useCallback((tab: CoordSection = "host") => {
    writeLocation({ page: "coordinator", coordTab: tab });
    setCoordTabState(tab);
    setPageState("coordinator");
  }, []);
  const openTask = useCallback((id: string) => {
    writeLocation({ page: "tasks", taskId: id });
    setSelectedTaskIdState(id);
    setPageState("tasks");
  }, []);
  const openWorker = useCallback((id: string) => {
    writeLocation({ page: "workers", workerId: id });
    setSelectedWorkerIdState(id);
    setPageState("workers");
  }, []);
  const openReviews = useCallback((id: string) => {
    writeLocation({ page: "reviews", reviewTaskId: id });
    setReviewTaskIdState(id);
    setPageState("reviews");
  }, []);
  const [runs, setRuns] = useState<RunSummary[]>([]);
  const [trash, setTrash] = useState<RunSummary[]>([]);
  const reloadRunsRef = useRef<() => void>(() => {});
  const [runId, setRunId] = useState("");
  const [snapshot, setSnap] = useState<RunSnapshot | null>(null);
  const snap = snapshot?.run.id === runId ? snapshot : null;
  const runIdRef = useRef(runId);
  runIdRef.current = runId;
  const runsRef = useRef(runs);
  runsRef.current = runs;
  const trashRef = useRef(trash);
  trashRef.current = trash;
  const [liveItems, setLiveItems] = useState<Item[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [runCtrl, setRunCtrl] = useState<"pause" | "resume" | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(readSel);
  const [talk, setTalk] = useState<TalkPlay | null>(null);
  const [camera, setCamera] = useState<"frame" | "center" | "restore">(() => (readSel() ? "center" : "frame"));
  const [restore, setRestore] = useState<{ vp: CanvasViewport; token: number } | null>(null);
  const vpGetter = useRef<(() => CanvasViewport) | null>(null);
  const vps = useRef<CanvasViewport[]>([]);
  const seqRef = useRef<number>(isCoHist(history.state) ? history.state.coSeq : -1);
  const restoreN = useRef(0);
  const bindViewport = useCallback((get: () => CanvasViewport) => {
    vpGetter.current = get;
  }, []);
  const [showWorkers, setShowWorkers] = useState(() => readSel()?.startsWith("worker:") || readSel() === COORD_ID || readFlag(SHOW_WORKERS_KEY, true));
  const [showReviewers, setShowReviewers] = useState(() => readSel()?.startsWith("reviewer:") || readFlag(SHOW_REVIEWERS_KEY, true));
  const [showExec, setShowExec] = useState(() => readFlag(SHOW_EXEC_KEY, true));
  const [showReviewEdges, setShowReviewEdges] = useState(() => readFlag(SHOW_REVIEW_EDGES_KEY, false));
  const [dagLens, setDagLens] = useState<DagLens>({ role: null, phase: null });
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => readFlag(SIDEBAR_KEY, false));
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [layout, setLayout] = useState<LayoutKind>(readLayout);
  const [reorgNonce, setReorgNonce] = useState(0);
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [profile, setProfile] = useState<Profile | null>(null);
  const [stats, setStats] = useState<StatsTable>({});
  const [availability, setAvailability] = useState<Record<string, ModelAvailability>>({});
  const [magpieEnabled, setMagpieEnabled] = useState(false);
  const [quotas, setQuotas] = useState<QuotaCard[]>([]);
  const [quotaError, setQuotaError] = useState<string | null>(null);
  const [agentsError, setAgentsError] = useState<string | null>(null);
  const [link, setLink] = useState<LinkState>(mock ? "live" : "connecting");
  const [streamEpoch, setStreamEpoch] = useState(0);
  const [daemonEpoch, setDaemonEpoch] = useState(0);
  const pullRef = useRef<() => void>(() => {});
  const pullSeqRef = useRef(0);

  useEffect(() => applyTheme(theme), [theme]);
  useEffect(() => {
    if (!selectedId) setTalk(null);
  }, [selectedId]);
  useEffect(() => setTalk(null), [runId]);

  useEffect(() => {
    const onPop = () => {
      const arriving = isCoHist(history.state) ? history.state.coSeq : -1;
      const leaving = seqRef.current;
      const loc = readWorkbenchLocation(window.location.search);
      const sel = loc.sel;
      setPageState(loc.page);
      setSelectedTaskIdState(loc.taskId);
      setSelectedWorkerIdState(loc.workerId);
      setReviewTaskIdState(loc.reviewTaskId);
      setCoordTabState(loc.coordTab);
      if (sel?.startsWith("reviewer:")) setShowReviewers(true);
      if (sel?.startsWith("worker:") || sel === COORD_ID) setShowWorkers(true);
      const linked = new URLSearchParams(window.location.search).get("run");
      if (linked && linked !== runIdRef.current && runsRef.current.some((run) => run.id === linked)) {
        setRunId(linked);
        vps.current = [];
        seqRef.current = arriving;
        setRestore(null);
        setCamera(sel ? "center" : "frame");
        setSelectedId(sel);
        return;
      }
      if (leaving > arriving) {
        const vp = vps.current[leaving];
        if (vp) {
          setRestore({ vp, token: ++restoreN.current });
          setCamera("restore");
        } else {
          setRestore(null);
          setCamera(sel ? "center" : "frame");
        }
      } else {
        setRestore(null);
        setCamera(sel ? "center" : "frame");
      }
      seqRef.current = arriving;
      setSelectedId(sel);
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  useEffect(() => {
    let dead = false;
    const load = () => {
      Promise.all([fetchRuns(false), fetchRuns(true)])
        .then(([active, trashed]) => {
          if (dead) return;
          setRuns(active);
          setTrash(trashed);
          setRunId((cur) => {
            if (cur && active.some((r) => r.id === cur)) return cur;
            // `co web` opens /?run=<id>; the bound Run wins over the last one viewed.
            const linked = new URLSearchParams(window.location.search).get("run");
            if (linked && active.some((r) => r.id === linked)) return linked;
            let stored = "";
            try {
              stored = localStorage.getItem(RUN_KEY) ?? "";
            } catch {
              stored = "";
            }
            return active.find((r) => r.id === stored)?.id ?? active[0]?.id ?? "";
          });
        })
        .catch((e: Error) => {
          if (!dead) setError(e.message);
        });
    };
    reloadRunsRef.current = load;
    load();
    if (mock) return () => { dead = true; };
    const onVis = () => {
      if (document.visibilityState === "visible") load();
    };
    document.addEventListener("visibilitychange", onVis);
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") load();
    }, 5000);
    return () => {
      dead = true;
      document.removeEventListener("visibilitychange", onVis);
      window.clearInterval(timer);
    };
  }, [mock]);

  useEffect(() => {
    ++pullSeqRef.current;
    setError(null);
    setSnap(null);
    setLiveItems([]);
    if (!runId) {
      pullRef.current = () => {};
      return;
    }
    writeRun(runId);
    try {
      localStorage.setItem(RUN_KEY, runId);
    } catch {
      /* ignore */
    }
    let dead = false;
    const pull = () => {
      const reqId = ++pullSeqRef.current;
      fetchSnapshot(runId)
        .then((next) => {
          if (dead || reqId !== pullSeqRef.current) return;
          setSnap(next);
          setError(null);
        })
        .catch((e: Error) => {
          if (!dead && reqId === pullSeqRef.current) setError(e.message);
        });
    };
    pullRef.current = pull;
    pull();
    if (mock) {
      return () => {
        dead = true;
      };
    }
    let timer: number | null = null;
    let queuedItems: Item[] = [];
    let itemRafId: number | null = null;
    const flushItems = () => {
      itemRafId = null;
      if (dead || queuedItems.length === 0) {
        queuedItems = [];
        return;
      }
      const batch = queuedItems;
      queuedItems = [];
      setLiveItems((prev) => {
        let next = prev;
        for (const item of batch) next = mergeLiveItem(next, item);
        return next;
      });
    };
    const stop = openRunEvents(runId, {
      onLink: (next) => {
        if (!dead) setLink(next);
      },
      onChange: (change) => {
        if (dead) return;
        if (change.table === "daemon_events") {
          setDaemonEpoch((n) => n + 1);
          return;
        }
        if (change.table === "runs") reloadRunsRef.current();
        if (timer !== null) return;
        timer = window.setTimeout(() => {
          timer = null;
          if (!dead) pull();
        }, 300);
      },
      onOpen: () => {
        if (!dead) pull();
      },
      onReconnect: () => {
        if (dead) return;
        setStreamEpoch((n) => n + 1);
      },
      onItem: (item) => {
        if (dead) return;
        queuedItems.push(item);
        if (itemRafId === null) {
          itemRafId = window.requestAnimationFrame(flushItems);
        }
      },
    });
    return () => {
      dead = true;
      pullRef.current = () => {};
      if (timer) window.clearTimeout(timer);
      if (itemRafId !== null) window.cancelAnimationFrame(itemRafId);
      stop();
    };
  }, [runId, mock]);

  const reloadAgentsRef = useRef<(force?: boolean) => void>(() => {});
  const lastAgentsAtRef = useRef(0);
  useEffect(() => {
    let dead = false;
    const load = (force = false) => {
      lastAgentsAtRef.current = Date.now();
      fetchAgents(force ? { refresh: true } : undefined)
        .then((next) => {
          if (dead) return;
          setProfile(next.profile);
          setStats(next.stats);
          setAvailability(next.availability ?? {});
          setMagpieEnabled(Boolean(next.magpieEnabled));
          setQuotas(next.quotas ?? []);
          setQuotaError(next.quotaError ?? null);
          setAgentsError(null);
        })
        .catch((e: Error) => {
          if (!dead) setAgentsError(e.message);
        });
    };
    reloadAgentsRef.current = load;
    load(false);
    if (mock) {
      return () => {
        dead = true;
      };
    }
    const maybeLoad = () => {
      if (Date.now() - lastAgentsAtRef.current >= 15_000) load(false);
    };
    const onFocus = () => maybeLoad();
    const onVis = () => {
      if (document.visibilityState === "visible") maybeLoad();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      dead = true;
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [mock]);

  useEffect(() => {
    if (mock || page !== "agents") return;
    if (Date.now() - lastAgentsAtRef.current >= 5_000) {
      reloadAgentsRef.current(false);
    }
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") reloadAgentsRef.current(false);
    }, 5_000);
    return () => window.clearInterval(timer);
  }, [mock, page]);

  const refresh = useCallback(() => pullRef.current(), []);
  const handleToggleSidebar = useCallback(() => {
    setSidebarCollapsed((v) => {
      writeFlag(SIDEBAR_KEY, !v);
      return !v;
    });
  }, []);
  const handleCloseDrawer = useCallback(() => setDrawerOpen(false), []);
  const handleReloadRuns = useCallback(() => reloadRunsRef.current(), []);
  const handleOptimisticRuns = useCallback(
    (fn: (cur: { runs: RunSummary[]; trash: RunSummary[] }) => { runs: RunSummary[]; trash: RunSummary[] }) => {
      const prev = { runs: runsRef.current, trash: trashRef.current };
      const next = fn(prev);
      const sortedRuns = sortRunSummaries(next.runs);
      const sortedTrash = sortRunSummaries(next.trash);
      runsRef.current = sortedRuns;
      trashRef.current = sortedTrash;
      setRuns(sortedRuns);
      setTrash(sortedTrash);
      return prev;
    },
    [],
  );
  const handleTogglePhase = useCallback((p: string) => {
    setDagLens((cur) => ({ ...cur, phase: cur.phase === p ? null : p }));
  }, []);
  const handleStopTalk = useCallback(() => setTalk(null), []);
  const handleOpenCoordHost = useCallback(() => openCoordinator("host"), [openCoordinator]);
  const remember = useCallback((id: string | null, cam: "frame" | "center") => {
    const seq = seqRef.current + 1;
    const vp = vpGetter.current?.() ?? null;
    if (writeSel(id, "push", { coSel: true, coSeq: seq } satisfies CoHist)) {
      if (vp) {
        vps.current[seq] = vp;
        vps.current.length = seq + 1;
      }
      seqRef.current = seq;
    }
    setRestore(null);
    setCamera(cam);
    setSelectedId(id);
  }, []);
  const select = useCallback((id: string | null) => {
    remember(id, "frame");
  }, [remember]);
  const handleClosePanel = useCallback(() => remember(null, "frame"), [remember]);
  const jump = useCallback((id: string) => {
    if (id.startsWith("reviewer:")) setShowReviewers(true);
    if (id.startsWith("worker:") || id === COORD_ID) setShowWorkers(true);
    remember(id, "center");
  }, [remember]);
  const jumpToDag = useCallback((id: string) => {
    if (id.startsWith("reviewer:")) setShowReviewers(true);
    if (id.startsWith("worker:") || id === COORD_ID) setShowWorkers(true);
    writeLocation({ page: "dag", sel: id }, "push");
    seqRef.current = -1;
    vps.current = [];
    setPageState("dag");
    setRestore(null);
    setCamera("center");
    setSelectedId(id);
  }, []);
  const pickRun = useCallback((id: string) => {
    setRunId(id);
    writeRun(id, true);
    seqRef.current = -1;
    vps.current = [];
    setRestore(null);
    setCamera("frame");
    setSelectedId(null);
    setSelectedTaskIdState(null);
    setSelectedWorkerIdState(null);
    setReviewTaskIdState(null);
    setCoordTabState("host");
    setDagLens({ role: null, phase: null });
    setDrawerOpen(false);
  }, []);

  useEffect(() => {
    if (page !== "dag") return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.repeat || event.defaultPrevented) return;
      if (typingTarget(event.target) || escOverlayOpen()) return;
      if (talk) {
        event.preventDefault();
        setTalk(null);
        return;
      }
      if (isCoHist(history.state)) {
        event.preventDefault();
        history.back();
        return;
      }
      if (!selectedId) {
        if (dagLens.role !== null || dagLens.phase !== null) {
          event.preventDefault();
          setDagLens({ role: null, phase: null });
        }
        return;
      }
      event.preventDefault();
      writeSel(null, "replace", null, true);
      seqRef.current = -1;
      setRestore(null);
      setCamera("frame");
      setSelectedId(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [page, selectedId, talk, dagLens]);

  const deps = snap ? depCount(snap) : 0;
  const reviewerCount = snap ? snap.workers.length - authorWorkers(snap.workers).length : 0;
  const workerCount = snap ? snap.workers.length - reviewerCount : 0;
  const reviews = useMemo(() => snap ? reviewedTasks(snap) : [], [snap]);
  const handleOverviewOpen = useCallback((id: string) => {
    if (page === "dag") {
      jump(id);
      return;
    }
    if (page === "reviews" && reviews.some(({ task }) => task.id === id)) {
      setReviewTaskId(id);
      return;
    }
    const target = selectionToDetailTarget(id);
    if (!target) return;
    if (target.page === "coordinator") openCoordinator(target.coordTab);
    else if (target.page === "workers") openWorker(target.workerId);
    else openTask(target.taskId);
  }, [page, jump, reviews, setReviewTaskId, openCoordinator, openWorker, openTask]);
  const reviewsActive = reviews.some(({ board }) => board.standing === "reviewing" || board.standing === "rework");
  const tasksActive = snap?.tasks.some((task) => task.status === "running") ?? false;
  const workersActive = snap?.workers.some((worker) => worker.state === "busy" || worker.state === "starting") ?? false;
  const reviewLiveItems = useMemo(() => {
    const grouped = new Map<string, Item[]>();
    for (const item of liveItems) {
      const items = grouped.get(item.workerId) ?? [];
      items.push(item);
      grouped.set(item.workerId, items);
    }
    return grouped;
  }, [liveItems]);
  const itemTick = useMemo(() => new Map((snap?.workers ?? []).map((worker) => [worker.id, streamEpoch])), [snap?.workers, streamEpoch]);
  const pendingActions = useMemo(() => snap?.pendingMessages.filter((m) => m.needsAction && m.type !== "run_done").length ?? 0, [snap]);
  const coordAlertCount = pendingActions + (snap?.stalls?.length ?? 0);

  async function toggleRun() {
    if (!snap || runCtrl) return;
    const pausing = snap.run.status === "active";
    if (!pausing && snap.run.status !== "paused") return;
    setRunCtrl(pausing ? "pause" : "resume");
    try {
      if (pausing) await pauseRun(snap.run.id);
      else await resumeRun(snap.run.id);
      refresh();
      reloadRunsRef.current();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setRunCtrl(null);
    }
  }

  return (
    <div className="app">
      <header className="topbar">
        <div className="topbar__brand">
          <button className="icon-btn topbar__menu" onClick={() => setDrawerOpen(true)} aria-label={t("side.runs")} title={t("side.runs")}>
            <svg viewBox="0 0 16 16" width="18" height="18" aria-hidden="true">
              <path d="M2.5 4h11M2.5 8h11M2.5 12h11" />
            </svg>
          </button>
          <span className="topbar__mark" aria-hidden="true">
            <svg viewBox="0 0 24 24" width="22" height="22">
              <circle cx="5" cy="12" r="2.6" />
              <circle cx="19" cy="5" r="2.6" />
              <circle cx="19" cy="19" r="2.6" />
              <path d="M7.4 11 L16.6 6 M7.4 13 L16.6 18" />
            </svg>
          </span>
          <div>
            <div className="topbar__title">{t("app.title")}</div>
            <div className="topbar__subtitle">{t("app.subtitle")}</div>
          </div>
        </div>
        <div className="seg page-tabs" role="tablist" aria-label={t("top.dag")}>
          <button className={page === "dag" ? "active" : ""} aria-pressed={page === "dag"} onClick={() => setPage("dag")}>{t("top.dag")}</button>
          <button className={page === "coordinator" ? "active" : ""} data-role="coord" aria-pressed={page === "coordinator"} onClick={() => setPage("coordinator")}>
            {t("page.coordinator")}
            {coordAlertCount > 0 && <span className="page-tabs__count">{coordAlertCount}</span>}
            {snap?.run.status === "active" && coordAlertCount > 0 && <span className="page-tabs__pulse sdot sdot--pulse" aria-label={t("wb.attentionQueue")} />}
          </button>
          <button className={page === "tasks" ? "active" : ""} data-role="task" aria-pressed={page === "tasks"} onClick={() => setPage("tasks")}>
            {t("page.tasks")}
            {snap && snap.tasks.length > 0 && <span className="page-tabs__count">{snap.tasks.length}</span>}
            {tasksActive && <span className="page-tabs__pulse sdot sdot--pulse" aria-label={t("status.running")} />}
          </button>
          <button className={page === "workers" ? "active" : ""} data-role="worker" aria-pressed={page === "workers"} onClick={() => setPage("workers")}>
            {t("page.workers")}
            {snap && snap.workers.length > 0 && <span className="page-tabs__count">{snap.workers.length}</span>}
            {workersActive && <span className="page-tabs__pulse sdot sdot--pulse" aria-label={t("wb.workerActive")} />}
          </button>
          <button className={page === "reviews" ? "active" : ""} data-role="reviewer" aria-pressed={page === "reviews"} onClick={() => setPage("reviews")}>
            {t("page.reviews")}
            {reviews.length > 0 && <span className="page-tabs__count">{reviews.length}</span>}
            {reviewsActive && <span className="page-tabs__pulse sdot sdot--pulse" aria-label={t("review.filterActive")} />}
          </button>
          <button className={page === "daemon" ? "active" : ""} aria-pressed={page === "daemon"} onClick={() => setPage("daemon")}>{t("top.daemon")}</button>
          <button className={page === "agents" ? "active" : ""} aria-pressed={page === "agents"} onClick={() => setPage("agents")}>{t("top.agents")}</button>
        </div>
        <div className="topbar__right">
          <span className={`conn ${mock ? "conn--ok" : error ? "conn--bad" : link === "live" ? "conn--ok" : "conn--wait"}`} title={error ?? (mock ? t("top.mock") : link === "live" ? t("top.connected") : t(link === "reconnecting" ? "top.reconnecting" : "top.connecting"))}>
            <span className={`sdot${link !== "live" && !mock ? " sdot--pulse" : ""}`} />
            {mock ? t("top.mock") : error ? t("top.fetchFailed") : link === "live" ? t("top.connected") : link === "reconnecting" ? t("top.reconnecting") : t("top.connecting")}
          </span>
          <div className="seg" role="group" aria-label={t("top.lang")}>
            {(["zh", "en"] as Lang[]).map((mode) => (
              <button key={mode} className={lang === mode ? "active" : ""} aria-pressed={lang === mode} onClick={() => setLang(mode)}>
                {t(`lang.${mode}`)}
              </button>
            ))}
          </div>
          <div className="seg" role="group" aria-label={t("top.theme")}>
            {(["system", "light", "dark"] as Theme[]).map((mode) => (
              <button
                key={mode}
                className={theme === mode ? "active" : ""}
                aria-pressed={theme === mode}
                onClick={() => {
                  setTheme(mode);
                  try {
                    localStorage.setItem(THEME_KEY, mode);
                  } catch {
                    /* ignore */
                  }
                }}
              >
                {t(`theme.${mode}`)}
              </button>
            ))}
          </div>
        </div>
      </header>
      {error && <p className="err-banner" role="alert">{error}</p>}

      {page === "agents" ? (
        <AgentsView
          profile={profile}
          stats={stats}
          availability={availability}
          magpieEnabled={magpieEnabled}
          quotas={quotas}
          quotaError={quotaError}
          error={agentsError}
          onRefresh={() => reloadAgentsRef.current(true)}
        />
      ) : (
        <div className="body">
          <RunSidebar
            runs={runs}
            trash={trash}
            runId={runId}
            error={null}
            onPick={pickRun}
            onChanged={handleReloadRuns}
            onOptimisticRuns={handleOptimisticRuns}
            collapsed={sidebarCollapsed}
            onToggleCollapsed={handleToggleSidebar}
            drawerOpen={drawerOpen}
            onCloseDrawer={handleCloseDrawer}
          />
          <div className="workspace">
            <div className="toolbar">
              <div className="toolbar__group">
                <span className="toolbar__meta">
                  {snap ? `${snap.tasks.length} ${t("tb.tasks")} · ${deps} ${t("tb.deps")} · ${workerCount} ${t("tb.workers")}${reviewerCount > 0 ? ` · ${reviewerCount} ${t("tb.reviewers")}` : ""}` : ""}
                  {pendingActions > 0 ? ` · ${t("coord.pending", { n: pendingActions })}` : ""}
                </span>
              </div>
              <div className="toolbar__group">
                {snap && (snap.run.status === "active" || snap.run.status === "paused") && (
                  <>
                    <button
                      className="btn btn--ghost"
                      disabled={runCtrl !== null}
                      title={snap.run.status === "paused" ? (snap.run.pauseReason === "budget" ? t("run.pauseBudget") : t("run.pauseUser")) : t("run.pauseHint")}
                      onClick={() => { void toggleRun(); }}
                    >
                      {runCtrl === "pause" ? t("run.pausing") : runCtrl === "resume" ? t("run.resuming") : snap.run.status === "active" ? t("run.pause") : t("run.resume")}
                    </button>
                    {snap.run.status === "paused" && (
                      <span className="toolbar__hint">{snap.run.pauseReason === "budget" ? t("run.pauseBudget") : t("run.pauseUser")}</span>
                    )}
                  </>
                )}
                {page === "dag" && <>
                <span className="field-label">{t("tb.layout")}</span>
                <div className="seg" role="group" aria-label={t("tb.layout")}>
                  {LAYOUTS.map((item) => (
                    <button
                      key={item.kind}
                      className={layout === item.kind ? "active" : ""}
                      aria-pressed={layout === item.kind}
                      title={t(item.title)}
                      onClick={() => {
                        setLayout(item.kind);
                        setRestore(null);
                        setCamera((cur) => (cur === "restore" ? "frame" : cur));
                        try {
                          localStorage.setItem(LAYOUT_KEY, item.kind);
                        } catch {
                          /* ignore */
                        }
                      }}
                    >
                      {t(item.label)}
                    </button>
                  ))}
                </div>
                <button className="btn btn--ghost" title={t("tb.relayoutTitle")} onClick={() => { setRestore(null); setCamera((cur) => (cur === "restore" ? "frame" : cur)); setReorgNonce((n) => n + 1); }}>{t("tb.relayout")}</button>
                <label className="toggle" title={t("tb.showWorkersTitle")}>
                  <input
                    type="checkbox"
                    checked={showWorkers}
                    onChange={() => {
                      setShowWorkers((v) => {
                        writeFlag(SHOW_WORKERS_KEY, !v);
                        return !v;
                      });
                      if (selectedId && (selectedId.startsWith("worker:") || (selectedId === COORD_ID && !(showReviewers && reviewerCount > 0)))) {
                        writeSel(null, "replace", history.state);
                        setRestore(null);
                        setCamera("frame");
                        setSelectedId(null);
                      }
                    }}
                  />
                  <span>{t("tb.showWorkers")}</span>
                </label>
                <label className="toggle" title={t("dag.reviewersTip")}>
                  <input type="checkbox" checked={showReviewers} onChange={() => {
                    setShowReviewers((v) => { writeFlag(SHOW_REVIEWERS_KEY, !v); return !v; });
                    if (selectedId?.startsWith("reviewer:")) remember(null, "frame");
                  }} />
                  <span>{t("dag.reviewers")}</span>
                </label>
                <label className="toggle" title={t("tb.showExecTitle")}>
                  <input
                    type="checkbox"
                    checked={showExec}
                    onChange={() => {
                      setShowExec((v) => {
                        writeFlag(SHOW_EXEC_KEY, !v);
                        return !v;
                      });
                    }}
                  />
                  <span>{t("tb.showExec")}</span>
                </label>
                <label className="toggle" title={t("tb.showReviewEdgesTitle")}>
                  <input
                    type="checkbox"
                    checked={showReviewEdges}
                    onChange={() => {
                      setShowReviewEdges((v) => {
                        const next = !v;
                        writeFlag(SHOW_REVIEW_EDGES_KEY, next);
                        if (next && !showReviewers) {
                          setShowReviewers(true);
                          writeFlag(SHOW_REVIEWERS_KEY, true);
                        }
                        return next;
                      });
                    }}
                  />
                  <span>{t("tb.showReviewEdges")}</span>
                </label>
                </>}
              </div>
            </div>
            {snap && (
              <OverviewBar
                snap={snap}
                activePhase={page === "dag" ? dagLens.phase : null}
                onTogglePhase={page === "dag" ? handleTogglePhase : undefined}
                onOpen={handleOverviewOpen}
              />
            )}
            {page === "daemon" ? (
              <main className="wb-workspace">
                <DaemonView
                  run={snap?.run ?? null}
                  snap={snap}
                  daemonEpoch={daemonEpoch}
                  onOpenTask={openTask}
                  onOpenWorker={openWorker}
                  onOpenCoordinator={openCoordinator}
                />
              </main>
            ) : page === "coordinator" ? (
              <main className="wb-workspace">
                <CoordinatorView
                  key={runId}
                  snap={snap}
                  section={coordTab}
                  onSelectSection={setCoordTab}
                  onJumpToDag={jumpToDag}
                  onOpenTask={openTask}
                  onOpenWorker={openWorker}
                />
              </main>
            ) : page === "tasks" ? (
              <main className="wb-workspace">
                <TasksView
                  key={runId}
                  snap={snap}
                  liveItems={liveItems}
                  streamEpoch={streamEpoch}
                  selectedTaskId={selectedTaskId}
                  onSelectTask={setSelectedTaskId}
                  onJumpToDag={jumpToDag}
                  onOpenWorker={openWorker}
                  onOpenReviews={openReviews}
                  onRefresh={refresh}
                />
              </main>
            ) : page === "workers" ? (
              <main className="wb-workspace">
                <WorkersView
                  key={runId}
                  snap={snap}
                  profile={profile}
                  selectedWorkerId={selectedWorkerId}
                  onSelectWorker={setSelectedWorkerId}
                  onJumpToDag={jumpToDag}
                  onOpenTask={openTask}
                  onOpenCoordinator={handleOpenCoordHost}
                  onOpenReviews={openReviews}
                  onRefresh={refresh}
                  liveItems={liveItems}
                  streamEpoch={streamEpoch}
                />
              </main>
            ) : page === "reviews" ? (
              <main className="reviews-workspace">
                <ReviewsView key={runId} snap={snap} selectedTaskId={reviewTaskId} onSelectTask={setReviewTaskId} onJumpToDag={jumpToDag} onOpenTask={openTask} liveItems={reviewLiveItems} itemTick={itemTick} />
              </main>
            ) : <main className="canvas">
              <div className="canvas__board">
              {snap && (
                <DagView snap={snap} selectedId={selectedId} onSelect={select} layout={layout} reorgNonce={reorgNonce} showWorkers={showWorkers} showReviewers={showReviewers} showExec={showExec} showReviewEdges={showReviewEdges} lens={dagLens} onLensChange={setDagLens} camera={camera} bindViewport={bindViewport} restoreViewport={restore} talk={talk} onStopTalk={handleStopTalk} />
              )}
              {snap && snap.tasks.length > 0 && (
                <div className="edge-legend dag-toolbar__legend" aria-label="legend">
                  {(showWorkers || (showReviewers && reviewerCount > 0)) && (
                    <span className="edge-legend__item">
                      <span className="role-badge role-badge--coord">{t("role.coordinator")}</span>
                      <LegendArrow kind="dispatch" />
                      {showWorkers && <span className="role-badge role-badge--worker">{t("role.worker")}</span>}
                      {showReviewers && reviewerCount > 0 && <span className="role-badge role-badge--reviewer">{t("role.reviewer")}</span>}
                      <span>{t("legend.dispatch")}</span>
                    </span>
                  )}
                  {showWorkers && (
                    <>
                      <span className="edge-legend__item">
                        <span className="role-badge role-badge--worker">{t("role.worker")}</span>
                        <LegendArrow kind="fork" />
                        <span className="role-badge role-badge--worker">{t("role.worker")}</span>
                        <span>{t("legend.fork")}</span>
                      </span>
                      <span className="edge-legend__item">
                        <span className="role-badge role-badge--worker">{t("role.worker")}</span>
                        <LegendArrow kind="executes" />
                        <span className="role-badge role-badge--task">{t("role.task")}</span>
                        <span>{t(showExec ? "legend.executesAlways" : "legend.executes")}</span>
                      </span>
                    </>
                  )}
                  {showReviewers && reviewerCount > 0 && <span className="edge-legend__item">
                    <span className="role-badge role-badge--task">{t("role.task")}</span>
                    <LegendArrow kind="review" />
                    <span className="role-badge role-badge--reviewer">{t("role.reviewer")}</span>
                    <span>{t(showReviewEdges ? "legend.reviewAlways" : "legend.review")}</span>
                  </span>}
                  <span className="edge-legend__item">
                    <span className="legend-chip">A</span>
                    <LegendArrow kind="depends" />
                    <span className="legend-chip">B</span>
                    <span>{t("legend.depends")}</span>
                    <span className="edge-legend__colors">
                      <span className="legend-swatch legend-swatch--pending" />
                      {t("legend.pending")}
                      <span className="legend-swatch legend-swatch--satisfied" />
                      {t("legend.satisfied")}
                    </span>
                  </span>
                  {hasDataWhen(snap.tasks) && (
                    <span className="edge-legend__item">{t("legend.branchData")}</span>
                  )}
                  <span className="edge-legend__item edge-legend__hint">{t("legend.select")}</span>
                </div>
              )}
              {!runId && (
                <div className="empty-run">
                  <p className="empty-run__title">{t("empty.pickTitle")}</p>
                  <p className="empty-run__body">{t("empty.pickBody")}</p>
                </div>
              )}
              </div>
              {snap && selectedId && (
                <NodePanel
                  selectedId={selectedId}
                  snap={snap}
                  liveItems={liveItems}
                  streamEpoch={streamEpoch}
                  onSelect={jump}
                  onClose={handleClosePanel}
                  onRefresh={refresh}
                  playing={talk?.key ?? null}
                  onPlay={setTalk}
                  onOpenReviews={openReviews}
                />
              )}
            </main>}
          </div>
        </div>
      )}
    </div>
  );
}
