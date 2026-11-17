import type { CSSProperties } from "react";
import { ViewportPortal } from "@xyflow/react";
import type { CanvasRoleChip, DagLens, LensRole, RunProgressSummary } from "../dag-lens";
import { useT } from "../i18n";
import type { Dir, Pt } from "../layout";

export interface ZoneInput {
  kind: "orch" | "tasks" | "review";
  ids: string[];
}

interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

const PAD_X = 28;
const PAD_TOP = 46;
const ORCH_WITH_GROUPS_PAD_TOP = 58;
const PAD_BOTTOM = 28;
const SEAM_CLEARANCE = 14;

export const WORKER_GROUP_PAD = { x: 12, top: 26, bottom: 10 } as const;

export interface WorkerCategoryGroup {
  key: string;
  builtIn: boolean;
  roleKind: LensRole;
  labelKey: string;
  fallbackLabel: string;
  colorVar: string;
  ids: string[];
  busyCount: number;
}

function measureZone(
  ids: string[],
  pos: Map<string, Pt>,
  sizeOf: (id: string) => { w: number; h: number },
): { minX: number; minY: number; maxX: number; maxY: number } | null {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const id of ids) {
    const p = pos.get(id);
    if (!p) continue;
    const s = sizeOf(id);
    minX = Math.min(minX, p.x);
    minY = Math.min(minY, p.y);
    maxX = Math.max(maxX, p.x + s.w);
    maxY = Math.max(maxY, p.y + s.h);
  }
  if (!Number.isFinite(minX)) return null;
  return { minX, minY, maxX, maxY };
}

function clampBox(
  kind: ZoneInput["kind"],
  raw: { minX: number; minY: number; maxX: number; maxY: number },
  dir: Dir,
  seam?: number,
  reviewSeam?: number,
  hasWorkerGroups = false,
): Box {
  const topPad = kind === "orch" && hasWorkerGroups ? ORCH_WITH_GROUPS_PAD_TOP : PAD_TOP;
  let x1 = raw.minX - PAD_X;
  let y1 = raw.minY - topPad;
  let x2 = raw.maxX + PAD_X;
  let y2 = raw.maxY + PAD_BOTTOM;

  if (dir === "LR") {
    if ((kind === "orch" || (kind === "review" && reviewSeam == null)) && seam != null) {
      x2 = Math.min(x2, seam - SEAM_CLEARANCE);
    }
    if (kind === "tasks") {
      if (seam != null) x1 = Math.max(x1, seam + SEAM_CLEARANCE);
      if (reviewSeam != null) x2 = Math.min(x2, reviewSeam - SEAM_CLEARANCE);
    }
    if (kind === "review" && reviewSeam != null) x1 = Math.max(x1, reviewSeam + SEAM_CLEARANCE);
  } else {
    if ((kind === "orch" || (kind === "review" && reviewSeam == null)) && seam != null) {
      y2 = Math.min(y2, seam - SEAM_CLEARANCE);
    }
    if (kind === "tasks") {
      if (seam != null) y1 = Math.max(y1, seam + SEAM_CLEARANCE);
      if (reviewSeam != null) y2 = Math.min(y2, reviewSeam - SEAM_CLEARANCE);
    }
    if (kind === "review" && reviewSeam != null) y1 = Math.max(y1, reviewSeam + SEAM_CLEARANCE);
  }

  return { x: x1, y: y1, w: Math.max(120, x2 - x1), h: Math.max(80, y2 - y1) };
}

export function DagZoneBackdrop({
  dir,
  zones,
  workerGroups,
  lensRole = null,
  pos,
  sizeOf,
  seam,
  reviewSeam,
  progress,
  authorCount,
  reviewerCount,
}: {
  dir: Dir;
  zones: ZoneInput[];
  workerGroups?: WorkerCategoryGroup[];
  lensRole?: LensRole | null;
  pos: Map<string, Pt>;
  sizeOf: (id: string) => { w: number; h: number };
  seam?: number;
  reviewSeam?: number;
  progress: RunProgressSummary;
  authorCount: number;
  reviewerCount: number;
}) {
  const { t } = useT();
  const hasWorkerGroups = Boolean(workerGroups && workerGroups.length > 0);
  const boxes = zones
    .map((z) => {
      const raw = measureZone(z.ids, pos, sizeOf);
      return raw ? { kind: z.kind, box: clampBox(z.kind, raw, dir, seam, reviewSeam, hasWorkerGroups) } : null;
    })
    .filter((entry): entry is { kind: ZoneInput["kind"]; box: Box } => entry !== null);

  const groupBoxes = (workerGroups ?? [])
    .map((group) => {
      const raw = measureZone(group.ids, pos, sizeOf);
      if (!raw) return null;
      return {
        group,
        box: {
          x: raw.minX - WORKER_GROUP_PAD.x,
          y: raw.minY - WORKER_GROUP_PAD.top,
          w: raw.maxX - raw.minX + WORKER_GROUP_PAD.x * 2,
          h: raw.maxY - raw.minY + WORKER_GROUP_PAD.top + WORKER_GROUP_PAD.bottom,
        },
      };
    })
    .filter((entry): entry is { group: WorkerCategoryGroup; box: Box } => entry !== null);

  if (boxes.length === 0) return null;

  let crossMin = Infinity;
  let crossMax = -Infinity;
  for (const { box } of boxes) {
    if (dir === "LR") {
      crossMin = Math.min(crossMin, box.y);
      crossMax = Math.max(crossMax, box.y + box.h);
    } else {
      crossMin = Math.min(crossMin, box.x);
      crossMax = Math.max(crossMax, box.x + box.w);
    }
  }

  const zoneTitle = (kind: ZoneInput["kind"]) =>
    kind === "orch" ? t("dag.zone.orch") : kind === "tasks" ? t("dag.zone.tasks") : t("dag.zone.review");

  const zoneMeta = (kind: ZoneInput["kind"]) => {
    if (kind === "orch") return t("dag.zone.orchMeta", { busy: progress.busyAuthors, total: authorCount });
    if (kind === "tasks") {
      return t("dag.zone.tasksMeta", {
        done: progress.done,
        total: progress.total,
        running: progress.running,
      });
    }
    return t("dag.zone.reviewMeta", {
      reviewing: progress.reviewing + progress.busyReviewers,
      total: reviewerCount,
    });
  };

  return (
    <ViewportPortal>
      <div className="dag-zones" aria-hidden="true">
        {boxes.map(({ kind, box }) => (
          <div
            key={kind}
            className={`dag-zone dag-zone--${kind}`}
            style={{ left: box.x, top: box.y, width: box.w, height: box.h }}
          >
            <div className="dag-zone__head">
              <span className="dag-zone__title">{zoneTitle(kind)}</span>
              <span className="dag-zone__meta">{zoneMeta(kind)}</span>
            </div>
          </div>
        ))}
        {groupBoxes.map(({ group, box }) => {
          const label = group.builtIn ? t(group.labelKey) : group.fallbackLabel;
          const active = lensRole === group.roleKind;
          const dimmed = lensRole != null && !active;
          return (
            <div
              key={`wgroup:${group.key}`}
              className={`dag-worker-group${active ? " dag-worker-group--active" : ""}${dimmed ? " dag-worker-group--dim" : ""}`}
              data-role={group.roleKind}
              style={
                {
                  left: box.x,
                  top: box.y,
                  width: box.w,
                  height: box.h,
                  "--wgroup-accent": group.colorVar,
                } as CSSProperties
              }
            >
              <div className="dag-worker-group__head">
                <span className="dag-worker-group__dot" />
                <span className="dag-worker-group__title">{label}</span>
                <span className="dag-worker-group__count">
                  {group.busyCount > 0
                    ? t("dag.workerGroup.busyMeta", { busy: group.busyCount, total: group.ids.length })
                    : group.ids.length}
                </span>
              </div>
            </div>
          );
        })}
        {seam != null && Number.isFinite(crossMin) && (
          <div
            className={`dag-zone-seam dag-zone-seam--${dir.toLowerCase()}`}
            style={
              dir === "LR"
                ? { left: seam, top: crossMin, height: crossMax - crossMin }
                : { top: seam, left: crossMin, width: crossMax - crossMin }
            }
          />
        )}
        {reviewSeam != null && Number.isFinite(crossMin) && (
          <div
            className={`dag-zone-seam dag-zone-seam--${dir.toLowerCase()} dag-zone-seam--review`}
            style={
              dir === "LR"
                ? { left: reviewSeam, top: crossMin, height: crossMax - crossMin }
                : { top: reviewSeam, left: crossMin, width: crossMax - crossMin }
            }
          />
        )}
      </div>
    </ViewportPortal>
  );
}

export function DagRoleLensBar({
  roles,
  lens,
  onToggleRole,
  onClearPhase,
  onClearAll,
}: {
  roles: CanvasRoleChip[];
  lens: DagLens;
  onToggleRole: (role: LensRole) => void;
  onClearPhase: () => void;
  onClearAll: () => void;
}) {
  const { t } = useT();
  if (roles.length === 0 && !lens.phase) return null;
  const active = lens.role !== null || lens.phase !== null;

  return (
    <div className="dag-lens-bar" role="toolbar" aria-label={t("dag.lens.label")}>
      <span className="dag-lens-bar__label">{t("dag.lens.label")}</span>
      {roles.map((chip) => {
        const selected = lens.role === chip.kind;
        const dimmed = lens.role !== null && !selected;
        const label = t(chip.labelKey);
        const display = label === chip.labelKey ? chip.fallbackLabel : label;
        return (
          <button
            key={chip.kind}
            type="button"
            className={`dag-lens-pill${selected ? " dag-lens-pill--active" : ""}${dimmed ? " dag-lens-pill--dim" : ""}`}
            style={{ "--lens-color": chip.colorVar } as CSSProperties}
            aria-pressed={selected}
            onClick={() => onToggleRole(chip.kind)}
            title={t("dag.lens.roleTip", { role: display })}
          >
            <span className="dag-lens-pill__dot" />
            <span>{display}</span>
            <span className="dag-lens-pill__count">
              {chip.busy > 0 ? `${chip.busy}/${chip.count}` : chip.count}
            </span>
          </button>
        );
      })}
      {lens.phase && (
        <button
          type="button"
          className="dag-lens-pill dag-lens-pill--active dag-lens-pill--phase"
          onClick={onClearPhase}
          title={t("dag.lens.clearPhase")}
        >
          <span>{t("dag.lens.phase", { phase: lens.phase })}</span>
          <span aria-hidden="true">×</span>
        </button>
      )}
      {active && (
        <button type="button" className="dag-lens-bar__reset" onClick={onClearAll}>
          {t("dag.lens.reset")}
        </button>
      )}
    </div>
  );
}
