import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
  type CSSProperties,
} from "react";
import { deleteRun, renameRun, reorderRuns, restoreRun, trashRun } from "../api";
import { runExecutionStatus } from "../format";
import { useT } from "../i18n";
import { relTime } from "../time";
import type { Run, RunSummary } from "../types";

const FILTER_THRESHOLD = 6;
const DRAG_THRESHOLD = 4;
const MOVE_MS = 200;
const MOVE_EASE = "cubic-bezier(0.2, 0, 0, 1)";
const EDGE_SCROLL = 28;

function reducedMotion(): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function labelOf(run: Run): string {
  return run.name || run.objective || run.id;
}

interface Drag {
  id: string;
  pointerId: number;
  el: HTMLElement;
  startY: number;
  lastY: number;
  grab: number;
  moved: boolean;
}

interface MenuState {
  id: string;
  x: number;
  y: number;
}

export const RunSidebar = memo(function RunSidebar({
  runs,
  trash,
  runId,
  error,
  onPick,
  onChanged,
  onOptimisticRuns,
  collapsed,
  onToggleCollapsed,
  drawerOpen,
  onCloseDrawer,
}: {
  runs: RunSummary[];
  trash: RunSummary[];
  runId: string;
  error: string | null;
  onPick: (id: string) => void;
  onChanged: () => void;
  onOptimisticRuns?: (
    fn: (cur: { runs: RunSummary[]; trash: RunSummary[] }) => { runs: RunSummary[]; trash: RunSummary[] },
  ) => { runs: RunSummary[]; trash: RunSummary[] };
  collapsed: boolean;
  onToggleCollapsed: () => void;
  drawerOpen: boolean;
  onCloseDrawer: () => void;
}) {
  const { t } = useT();
  const [query, setQuery] = useState("");
  const [actionError, setActionError] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ id: string; name: string } | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [trashOpen, setTrashOpen] = useState(false);
  const [confirming, setConfirming] = useState<Run | null>(null);
  const [confirmText, setConfirmText] = useState("");
  const [pendingOrder, setPendingOrder] = useState<string[] | null>(null);
  const [dragOrder, setDragOrder] = useState<string[] | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const nameInput = useRef<HTMLInputElement>(null);
  const confirmInput = useRef<HTMLInputElement>(null);
  const busyIds = useRef(new Set<string>());
  const drag = useRef<Drag | null>(null);
  const dragOrderRef = useRef<string[] | null>(null);
  const suppressClick = useRef(false);
  const detach = useRef<(() => void) | null>(null);
  const rail = collapsed && !drawerOpen;
  const canReorder = !rail && !query.trim() && !editing;

  useLayoutEffect(() => {
    nameInput.current?.focus();
    nameInput.current?.select();
  }, [editing?.id]);
  useLayoutEffect(() => {
    if (!confirming) return;
    confirmInput.current?.focus();
  }, [confirming]);

  useEffect(() => () => detach.current?.(), []);

  const ordered = useMemo(() => {
    if (!pendingOrder) return runs;
    const byId = new Map(runs.map((run) => [run.id, run]));
    const placed = pendingOrder.flatMap((id) => byId.get(id) ?? []);
    const rest = runs.filter((run) => !pendingOrder.includes(run.id));
    return [...placed, ...rest];
  }, [runs, pendingOrder]);

  useEffect(() => {
    if (!pendingOrder) return;
    const ids = runs.map((run) => run.id);
    const kept = pendingOrder.filter((id) => ids.includes(id));
    if (kept.length === ids.length && kept.every((id, index) => id === ids[index])) setPendingOrder(null);
  }, [runs, pendingOrder]);

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return ordered;
    return ordered.filter((run) =>
      run.id === editing?.id ||
      labelOf(run).toLowerCase().includes(q) ||
      run.objective.toLowerCase().includes(q) ||
      run.id.toLowerCase().includes(q) ||
      run.status.includes(q) ||
      runExecutionStatus(run).includes(q) ||
      t(`run.${runExecutionStatus(run)}`).toLowerCase().includes(q),
    );
  }, [ordered, query, editing?.id]);

  const visible = useMemo(() => {
    if (!dragOrder) return shown;
    const byId = new Map(shown.map((run) => [run.id, run]));
    return dragOrder.flatMap((id) => byId.get(id) ?? []);
  }, [dragOrder, shown]);

  function fail(reason: unknown): void {
    setActionError(reason instanceof Error ? reason.message : String(reason));
  }

  async function persistOrder(ids: string[]): Promise<void> {
    setPendingOrder(ids);
    try {
      await reorderRuns(ids);
      setActionError(null);
      onChanged();
    } catch (reason) {
      setPendingOrder(null);
      fail(reason);
    }
  }

  function move(id: string, delta: number): void {
    if (!canReorder) return;
    const ids = ordered.map((run) => run.id);
    const index = ids.indexOf(id);
    const next = index + delta;
    if (index < 0 || next < 0 || next >= ids.length) return;
    [ids[index], ids[next]] = [ids[next]!, ids[index]!];
    void persistOrder(ids);
    requestAnimationFrame(() => tabEl(id)?.querySelector<HTMLElement>("[role=tab]")?.focus());
  }

  async function commitRename(save: boolean): Promise<void> {
    if (!editing) return;
    const current = editing;
    setEditing(null);
    if (!save) return;
    try {
      await renameRun(current.id, current.name);
      setActionError(null);
      onChanged();
    } catch (reason) {
      fail(reason);
    }
  }

  async function moveToTrash(id: string): Promise<void> {
    if (busyIds.current.has(id)) return;
    busyIds.current.add(id);
    const index = ordered.findIndex((run) => run.id === id);
    const prevRunId = runId;
    const prev = onOptimisticRuns?.((cur) => {
      const target = cur.runs.find((run) => run.id === id);
      if (!target) return cur;
      const moved: RunSummary = {
        ...target,
        status: "paused",
        statusBeforeTrash: target.status,
        trashedAt: new Date().toISOString(),
      };
      return {
        runs: cur.runs.filter((run) => run.id !== id),
        trash: [moved, ...cur.trash.filter((run) => run.id !== id)],
      };
    });
    if (id === prevRunId) onPick(ordered[index + 1]?.id ?? ordered[index - 1]?.id ?? "");
    try {
      await trashRun(id);
      setActionError(null);
      onChanged();
    } catch (reason) {
      if (prev) onOptimisticRuns?.(() => prev);
      if (id === prevRunId) onPick(prevRunId);
      fail(reason);
    } finally {
      busyIds.current.delete(id);
    }
  }

  async function restore(id: string): Promise<void> {
    if (busyIds.current.has(id)) return;
    busyIds.current.add(id);
    const prev = onOptimisticRuns?.((cur) => {
      const target = cur.trash.find((run) => run.id === id);
      if (!target) return cur;
      const restored: RunSummary = {
        ...target,
        status: target.statusBeforeTrash ?? "active",
        trashedAt: undefined,
        statusBeforeTrash: undefined,
      };
      return {
        runs: [restored, ...cur.runs.filter((run) => run.id !== id)],
        trash: cur.trash.filter((run) => run.id !== id),
      };
    });
    if (!runId) onPick(id);
    try {
      await restoreRun(id);
      setActionError(null);
      onChanged();
    } catch (reason) {
      if (prev) onOptimisticRuns?.(() => prev);
      fail(reason);
    } finally {
      busyIds.current.delete(id);
    }
  }

  async function purge(run: Run): Promise<void> {
    if (busyIds.current.has(run.id)) return;
    busyIds.current.add(run.id);
    const prevRunId = runId;
    setConfirming(null);
    setConfirmText("");
    const prev = onOptimisticRuns?.((cur) => ({
      runs: cur.runs.filter((item) => item.id !== run.id),
      trash: cur.trash.filter((item) => item.id !== run.id),
    }));
    if (run.id === prevRunId) onPick(ordered.find((item) => item.id !== run.id)?.id ?? "");
    try {
      await deleteRun(run.id);
      setActionError(null);
      onChanged();
    } catch (reason) {
      if (prev) onOptimisticRuns?.(() => prev);
      if (run.id === prevRunId) onPick(prevRunId);
      fail(reason);
    } finally {
      busyIds.current.delete(run.id);
    }
  }

  function openMenu(id: string, x: number, y: number): void {
    const left = Math.max(8, Math.min(x, window.innerWidth - 188));
    const top = Math.max(8, Math.min(y, window.innerHeight - 176));
    setMenu({ id, x: left, y: top });
  }

  function onKey(e: KeyboardEvent<HTMLDivElement>): void {
    if ((e.target as HTMLElement).closest(".run-tab--editing, .run-tab__action")) return;
    if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      e.preventDefault();
      const focused = (document.activeElement as HTMLElement | null)?.closest<HTMLElement>("[data-run]")?.dataset.run ?? runId;
      move(focused, e.key === "ArrowUp" ? -1 : 1);
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") return;
    e.preventDefault();
    const idx = shown.findIndex((run) => run.id === runId);
    const next = e.key === "Home" ? 0 : e.key === "End" ? shown.length - 1 : e.key === "ArrowDown" ? Math.min(shown.length - 1, idx + 1) : Math.max(0, idx - 1);
    const run = shown[next];
    if (run) onPick(run.id);
  }

  const tabEl = (id: string) => listRef.current?.querySelector<HTMLElement>(`[data-run="${CSS.escape(id)}"]`) ?? null;

  function contentY(clientY: number): number {
    const list = listRef.current!;
    return clientY - list.getBoundingClientRect().top + list.scrollTop;
  }

  const reposition = () => {
    const current = drag.current;
    const list = listRef.current;
    const order = dragOrderRef.current;
    if (!current || !current.moved || !list || !order) return;
    const height = current.el.offsetHeight;
    const top = Math.max(0, Math.min(list.scrollHeight - height, contentY(current.lastY) - current.grab));
    const centre = top + height / 2;
    const others = order.filter((id) => id !== current.id);
    let index = 0;
    for (const id of others) {
      const el = tabEl(id);
      if (el && el.offsetTop + el.offsetHeight / 2 < centre) index++;
    }
    if (index !== order.indexOf(current.id)) {
      const next = [...others];
      next.splice(index, 0, current.id);
      dragOrderRef.current = next;
      setDragOrder(next);
    }
    current.el.style.transform = `translateY(${top - current.el.offsetTop}px)`;
  };

  function endDrag(commit: boolean): void {
    const current = drag.current;
    detach.current?.();
    drag.current = null;
    if (!current || !current.moved) return;
    suppressClick.current = commit;
    window.setTimeout(() => { suppressClick.current = false; }, 0);
    const order = dragOrderRef.current;
    dragOrderRef.current = null;
    const from = current.el.style.transform;
    current.el.style.transform = "";
    if (from && !reducedMotion()) {
      current.el.animate([{ transform: from }, { transform: "translateY(0)" }], { duration: MOVE_MS, easing: MOVE_EASE });
    }
    if (commit && order) void persistOrder(order);
    setDragOrder(null);
  }

  function onTabPointerDown(e: PointerEvent<HTMLButtonElement>, id: string): void {
    suppressClick.current = false;
    if (!canReorder || e.button !== 0 || drag.current) return;
    if ((e.target as HTMLElement).closest("[data-run-menu]")) return;
    if (e.pointerType === "touch" && !(e.target as HTMLElement).closest(".run-tab__grip")) return;
    const el = tabEl(id);
    if (!el) return;
    drag.current = {
      id,
      pointerId: e.pointerId,
      el,
      startY: e.clientY,
      lastY: e.clientY,
      grab: contentY(e.clientY) - el.offsetTop,
      moved: false,
    };
    const startOrder = shown.map((run) => run.id);
    const onMove = (ev: globalThis.PointerEvent) => {
      const current = drag.current;
      if (!current || ev.pointerId !== current.pointerId) return;
      current.lastY = ev.clientY;
      if (!current.moved) {
        if (Math.abs(ev.clientY - current.startY) < DRAG_THRESHOLD) return;
        current.moved = true;
        current.el.getAnimations().forEach((animation) => animation.cancel());
        dragOrderRef.current = startOrder;
        setDragOrder(startOrder);
      }
      ev.preventDefault();
      const list = listRef.current;
      if (list) {
        const rect = list.getBoundingClientRect();
        if (ev.clientY < rect.top + EDGE_SCROLL) list.scrollTop -= 8;
        else if (ev.clientY > rect.bottom - EDGE_SCROLL) list.scrollTop += 8;
      }
      reposition();
    };
    const onUp = (ev: globalThis.PointerEvent) => {
      if (drag.current && ev.pointerId === drag.current.pointerId) endDrag(ev.type === "pointerup");
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
    detach.current = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
      detach.current = null;
    };
  }

  const lastTops = useRef(new Map<string, number>());
  useLayoutEffect(() => {
    const list = listRef.current;
    const seen = new Map<string, number>();
    const animate = !reducedMotion();
    for (const el of list ? Array.from(list.querySelectorAll<HTMLElement>("[data-run]")) : []) {
      const id = el.dataset.run!;
      const top = el.offsetTop;
      seen.set(id, top);
      const prev = lastTops.current.get(id);
      const dragged = drag.current?.moved && drag.current.id === id;
      if (!animate || dragged || prev === undefined || prev === top) continue;
      el.getAnimations().forEach((animation) => animation.cancel());
      el.animate([{ transform: `translateY(${prev - top}px)` }, { transform: "translateY(0)" }], { duration: MOVE_MS, easing: MOVE_EASE });
    }
    lastTops.current = seen;
    reposition();
  });

  useEffect(() => {
    if (!menu) return;
    const close = (event: globalThis.PointerEvent) => {
      if ((event.target as HTMLElement | null)?.closest?.(".run-menu")) return;
      setMenu(null);
    };
    const onEsc = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setMenu(null);
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", onEsc);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", onEsc);
    };
  }, [menu]);

  const menuRun = menu ? ordered.find((run) => run.id === menu.id) : undefined;
  const menuIndex = menuRun ? ordered.findIndex((run) => run.id === menuRun.id) : -1;
  const confirmName = confirming ? labelOf(confirming) : "";
  const confirmOk = !!confirming && (confirmText.trim() === confirmName || confirmText.trim() === confirming.id);
  const shownError = actionError || error;

  return (
    <>
      {drawerOpen && <div className="sidebar-backdrop" onClick={onCloseDrawer} aria-hidden="true" />}
      <aside className={`sidebar${rail ? " sidebar--collapsed" : ""}${drawerOpen ? " sidebar--open" : ""}`} aria-label={t("side.runs")}>
        <div className="sidebar__head">
          {!rail && (
            <span className="sidebar__title">
              {t("side.runs")}
              <span className="sidebar__count">{runs.length}</span>
            </span>
          )}
          <div className="sidebar__actions">
            <button
              className="icon-btn sidebar__collapse"
              onClick={drawerOpen ? onCloseDrawer : onToggleCollapsed}
              title={rail ? t("side.expand") : t("side.collapse")}
              aria-label={rail ? t("side.expand") : t("side.collapse")}
              aria-expanded={!rail}
            >
              <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
                <path d="M10 3.5 5.5 8 10 12.5" />
              </svg>
            </button>
          </div>
        </div>
        {!rail && (
          <>
            {(runs.length > FILTER_THRESHOLD || query.trim() !== "") && (
              <div className="sidebar__filter">
                <input className="text-input" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("side.filter")} aria-label={t("side.filter")} spellCheck={false} />
              </div>
            )}
            {shownError && <div className="sidebar__err">{shownError}</div>}
            <div
              className={`sidebar__tabs${dragOrder ? " sidebar__tabs--dragging" : ""}${canReorder ? " sidebar__tabs--sortable" : ""}`}
              role="tablist"
              aria-orientation="vertical"
              aria-label={t("side.runs")}
              ref={listRef}
              onKeyDown={onKey}
            >
              {visible.map((run, index) => {
                const selected = run.id === runId;
                const dragging = dragOrder !== null && drag.current?.id === run.id;
                const name = labelOf(run);
                const status = runExecutionStatus(run);
                const statusToken = status === "paused" ? "blocked" : status === "archived" || status === "cancelled" ? "pending" : status;
                if (editing?.id === run.id) {
                  return (
                    <form
                      key={run.id}
                      data-run={run.id}
                      className="run-tab run-tab--editing"
                      onSubmit={(event) => {
                        event.preventDefault();
                        void commitRename(true);
                      }}
                      onKeyDown={(event) => {
                        event.stopPropagation();
                        if (event.nativeEvent.isComposing) {
                          if (event.key === "Enter") event.preventDefault();
                          return;
                        }
                        if (event.key === "Escape") {
                          event.preventDefault();
                          void commitRename(false);
                        }
                      }}
                    >
                      <input
                        ref={nameInput}
                        className="text-input run-tab__name-input"
                        value={editing.name}
                        onChange={(event) => setEditing({ id: run.id, name: event.target.value })}
                        aria-label={t("side.rename")}
                        placeholder={run.objective || run.id}
                      />
                      <span className="run-tab__rename-hint">{t("side.renameHint")}</span>
                    </form>
                  );
                }
                return (
                  <div key={run.id} data-run={run.id} className={`run-tab-row${dragging ? " run-tab-row--dragging" : ""}`} role="presentation">
                    <button
                      type="button"
                      role="tab"
                      aria-selected={selected}
                      tabIndex={selected || (shown.every((item) => item.id !== runId) && index === 0) ? 0 : -1}
                      className={`run-tab${dragging ? " run-tab--dragging" : ""}`}
                      onClick={() => {
                        if (suppressClick.current) {
                          suppressClick.current = false;
                          return;
                        }
                        onPick(run.id);
                        if (drawerOpen) onCloseDrawer();
                      }}
                      onPointerDown={(event) => onTabPointerDown(event, run.id)}
                      onContextMenu={(event) => {
                        event.preventDefault();
                        openMenu(run.id, event.clientX, event.clientY);
                      }}
                      title={`${name}\n${run.id}\n${t(`run.${status}`)}`}
                    >
                      {canReorder && (
                        <span className="run-tab__grip" title={t("side.reorder")} aria-hidden="true">
                          <svg viewBox="0 0 8 14" width="8" height="14">
                            <circle cx="2" cy="2" r="1.1" />
                            <circle cx="6" cy="2" r="1.1" />
                            <circle cx="2" cy="7" r="1.1" />
                            <circle cx="6" cy="7" r="1.1" />
                            <circle cx="2" cy="12" r="1.1" />
                            <circle cx="6" cy="12" r="1.1" />
                          </svg>
                        </span>
                      )}
                      <span className="run-tab__title">{name}</span>
                      {run.machine && (
                        <span className="sbadge mono" style={{ fontSize: 10, padding: "1px 6px", marginRight: 4 }} title={`Remote machine: ${run.machine}`}>
                          {run.machine}
                        </span>
                      )}
                      <span
                        className="sbadge run-tab__status"
                        style={{ "--st": `var(--st-${statusToken})`, "--st-bg": `var(--st-${statusToken}-bg)` } as CSSProperties}
                      >
                        <span className={`sdot${status === "running" ? " sdot--pulse" : ""}`} aria-hidden="true" />
                        {t(`run.${status}`)}
                      </span>
                      <span className="run-tab__meta">
                        <span className="mono">{run.id}</span>
                        <span title={run.createdAt}>{relTime(run.createdAt)}</span>
                      </span>
                    </button>
                    <button
                      type="button"
                      className="run-tab__action run-tab__menu"
                      data-run-menu=""
                      aria-label={t("side.menu")}
                      title={t("side.menu")}
                      onPointerDown={(event) => event.stopPropagation()}
                      onClick={(event) => {
                        event.stopPropagation();
                        const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
                        openMenu(run.id, rect.left, rect.bottom + 4);
                      }}
                    >
                      ⋯
                    </button>
                    <button
                      type="button"
                      className="run-tab__action run-tab__x"
                      aria-label={`${t("side.toTrash")}：${name}`}
                      title={t("side.toTrash")}
                      onPointerDown={(event) => event.stopPropagation()}
                      onKeyDown={(event) => event.stopPropagation()}
                      onClick={(event) => {
                        event.stopPropagation();
                        setMenu(null);
                        void moveToTrash(run.id);
                      }}
                    >
                      ×
                    </button>
                  </div>
                );
              })}
              {runs.length === 0 && !shownError && <div className="sidebar__empty">{t("side.empty")}</div>}
              {runs.length > 0 && shown.length === 0 && <div className="sidebar__empty">{t("side.noMatch")}</div>}
            </div>
            <button type="button" className="sidebar__removed" onClick={() => setTrashOpen((open) => !open)} aria-expanded={trashOpen} aria-controls="run-trash">
              {t("side.trash", { n: trash.length })}
            </button>
            {trashOpen && (
              <section className="sidebar__trash" id="run-trash" aria-label={t("side.trashTitle")}>
                <p className="sidebar__trash-hint">{t("side.trashHint")}</p>
                {trash.length === 0 && <div className="sidebar__empty">{t("side.trashEmpty")}</div>}
                {trash.map((run) => (
                  <div className="trash-run" key={run.id}>
                    <span className="trash-run__name" title={run.objective}>{labelOf(run)}</span>
                    <span className="trash-run__id mono">{run.id}</span>
                    <div className="run-tab__rename-actions">
                      <button type="button" className="btn btn--ghost btn--sm" onClick={() => void restore(run.id)}>{t("side.restore")}</button>
                      <button
                        type="button"
                        className="btn btn--ghost btn--sm trash-run__delete"
                        onClick={() => {
                          setConfirmText("");
                          setConfirming(run);
                        }}
                      >
                        {t("side.deleteForever")}
                      </button>
                    </div>
                  </div>
                ))}
              </section>
            )}
          </>
        )}
      </aside>
      {menu && menuRun && (
        <div className="run-menu" role="menu" style={{ left: menu.x, top: menu.y }}>
          <button type="button" role="menuitem" onClick={() => { setMenu(null); setEditing({ id: menuRun.id, name: labelOf(menuRun) }); }}>{t("side.rename")}</button>
          <button type="button" role="menuitem" disabled={!canReorder || menuIndex <= 0} onClick={() => { setMenu(null); move(menuRun.id, -1); }}>{t("side.moveUp")}</button>
          <button type="button" role="menuitem" disabled={!canReorder || menuIndex < 0 || menuIndex >= ordered.length - 1} onClick={() => { setMenu(null); move(menuRun.id, 1); }}>{t("side.moveDown")}</button>
          <button type="button" role="menuitem" className="run-menu__danger" onClick={() => { setMenu(null); void moveToTrash(menuRun.id); }}>{t("side.toTrash")}</button>
        </div>
      )}
      {confirming && (
        <div className="run-confirm" role="presentation" onClick={() => { setConfirming(null); setConfirmText(""); }}>
          <form
            className="run-confirm__card"
            role="dialog"
            aria-modal="true"
            aria-label={t("side.confirmDelete")}
            onClick={(event) => event.stopPropagation()}
            onSubmit={(event) => {
              event.preventDefault();
              if (confirmOk) void purge(confirming);
            }}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                setConfirming(null);
                setConfirmText("");
              }
            }}
          >
            <h2>{t("side.confirmDelete")}</h2>
            <p>{t("side.confirmHint")}</p>
            <input
              ref={confirmInput}
              className="text-input"
              value={confirmText}
              onChange={(event) => setConfirmText(event.target.value)}
              placeholder={t("side.confirmPlaceholder")}
              aria-label={t("side.confirmPlaceholder")}
              spellCheck={false}
              autoComplete="off"
            />
            <div className="run-confirm__actions">
              <button type="button" className="btn btn--ghost btn--sm" onClick={() => { setConfirming(null); setConfirmText(""); }}>{t("side.cancel")}</button>
              <button type="submit" className="btn btn--danger btn--sm" disabled={!confirmOk}>{t("side.confirmAction")}</button>
            </div>
          </form>
        </div>
      )}
    </>
  );
});
