import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { fetchItems, fetchReviewDiff } from "../api";
import { Estimate } from "./Estimate";
import { ReviewRequirement } from "./ReviewRequirement";
import { agentModel, attemptTokens, fmtCost, fmtDuration, fmtTokens, jsonPreview, shortId, workerNumbers } from "../format";
import { useT } from "../i18n";
import { COORD_ID, workerNodeId } from "../relate";
import {
  authorWorkers,
  buildReviewTimeline,
  reviewCircuitView,
  reviewerNodeId,
  reviewerNumbers,
  reviewedTasks,
  taskRequiresReview,
  workerCustomRole,
  type ReviewBoard,
  type ReviewRoundView,
  type ReviewStanding,
  type ReviewTimelineStep,
} from "../review";
import { clockTime, relTime } from "../time";
import type { Item, ReviewDiffSlice, RunSnapshot, Task, Worker } from "../types";
import { filterStreamItems } from "../workbench";
import { StreamDetailToggle, useStreamFullDetails } from "./WorkerTranscript";

export interface ReviewsViewProps {
  snap: RunSnapshot | null;
  selectedTaskId: string | null;
  onSelectTask: (taskId: string | null) => void;
  /** Jump back to DAG canvas and select/focus a node (`<taskId>`, `worker:<id>`, or `reviewer:<id>`). */
  onJumpToDag: (selectionId: string) => void;
  onOpenTask?: (taskId: string) => void;
  liveItems: Map<string, Item[]>;
  itemTick: Map<string, number>;
}

type ReviewFilter = "all" | "active" | "approved" | "rejected" | "waiting";
type VerdictTone = "approved" | "changes" | "running" | "failed" | "submitted";
type DiffMode = "delta" | "full";
type DiffKind = "file" | "meta" | "hunk" | "add" | "del" | "ctx";
type TranscriptScope = "task" | "all";

interface DiffRow {
  kind: DiffKind;
  text: string;
  sign: string;
  oldNo: number | null;
  newNo: number | null;
}

interface ReviewCast {
  author?: Worker;
  reviewers: Worker[];
  submits: number;
  reworks: number;
}

function usePhrase(): { t: ReturnType<typeof useT>["t"]; phrase: (zh: string, en: string) => string } {
  const { t, lang } = useT();
  return { t, phrase: (zh, en) => (lang === "en" ? en : zh) };
}

function standingMatches(standing: ReviewStanding, filter: ReviewFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "active":
      return standing === "reviewing" || standing === "rework";
    case "approved":
      return standing === "approved";
    case "rejected":
      return standing === "rejected" || standing === "broken" || standing === "stopped";
    case "waiting":
      return standing === "waiting";
  }
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function badgeNum(prefix: "W" | "R", num: number | undefined): string {
  return num != null ? `${prefix}${num}` : `${prefix}—`;
}

function severitySlug(severity: string | undefined): string {
  const slug = (severity ?? "").toLowerCase().replace(/[^a-z0-9_-]+/g, "");
  return slug || "info";
}

function reviewCast(task: Task, snap: RunSnapshot): ReviewCast {
  const steps = buildReviewTimeline(task, snap.attempts, snap.workers);
  const reviewers: Worker[] = [];
  const seen = new Set<string>();
  let author: Worker | undefined;
  let submits = 0;
  let reworks = 0;
  for (const step of steps) {
    if (step.kind === "author_submit") {
      submits += 1;
      if (!author && step.worker) author = step.worker;
    } else if (step.kind === "author_rework") {
      reworks += 1;
      if (!author && step.worker) author = step.worker;
    } else if (step.worker && !seen.has(step.worker.id)) {
      seen.add(step.worker.id);
      reviewers.push(step.worker);
    }
  }
  const fallbackId = task.reviewState?.reviewerWorkerId;
  if (fallbackId && !seen.has(fallbackId)) {
    const worker = snap.workers.find((item) => item.id === fallbackId);
    if (worker) reviewers.push(worker);
  }
  if (!author) {
    const attempt = snap.attempts
      .filter((item) => item.taskId === task.id && item.kind !== "review")
      .sort((a, b) => (a.startedAt < b.startedAt ? -1 : a.startedAt > b.startedAt ? 1 : a.id < b.id ? -1 : 1))
      .at(-1);
    if (attempt) author = snap.workers.find((item) => item.id === attempt.workerId);
  }
  return { author, reviewers, submits, reworks };
}

function verdictOf(round: ReviewRoundView): VerdictTone {
  if (round.verdict === "approved") return "approved";
  if (round.verdict === "changes_requested") return "changes";
  if (round.attempt.failure || round.attempt.outcome === "failed") return "failed";
  if (!round.attempt.endedAt && !round.attempt.outcome) return "running";
  return "submitted";
}

function priorReviewSummary(steps: ReviewTimelineStep[], index: number): string | undefined {
  for (let i = index - 1; i >= 0; i -= 1) {
    const step = steps[i];
    if (step?.kind === "review_round" && step.roundView.summary) return step.roundView.summary;
  }
  return undefined;
}

function parseUnifiedDiff(source: string): DiffRow[] {
  const raw = source.split("\n");
  if (raw.length > 0 && raw[raw.length - 1] === "") raw.pop();
  const rows: DiffRow[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  for (const line of raw) {
    if (line.startsWith("diff --git") || (!inHunk && (line.startsWith("--- ") || line.startsWith("+++ ")))) {
      inHunk = false;
      rows.push({ kind: "file", text: line, sign: "", oldNo: null, newNo: null });
      continue;
    }
    if (line.startsWith("@@")) {
      const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
      oldNo = match ? Number(match[1]) : 0;
      newNo = match ? Number(match[2]) : 0;
      inHunk = true;
      rows.push({ kind: "hunk", text: line, sign: "@@", oldNo: null, newNo: null });
      continue;
    }
    if (!inHunk) {
      rows.push({ kind: "meta", text: line, sign: "", oldNo: null, newNo: null });
      continue;
    }
    if (line.startsWith("+")) {
      rows.push({ kind: "add", text: line.slice(1), sign: "+", oldNo: null, newNo });
      newNo += 1;
      continue;
    }
    if (line.startsWith("-")) {
      rows.push({ kind: "del", text: line.slice(1), sign: "-", oldNo, newNo: null });
      oldNo += 1;
      continue;
    }
    if (line.startsWith("\\")) {
      rows.push({ kind: "meta", text: line, sign: "\\", oldNo: null, newNo: null });
      continue;
    }
    const text = line.startsWith(" ") ? line.slice(1) : line;
    rows.push({ kind: "ctx", text, sign: " ", oldNo, newNo });
    oldNo += 1;
    newNo += 1;
  }
  return rows;
}

function reviewAttemptIds(snap: RunSnapshot, reviewerId: string, taskId?: string): Set<string> {
  const ids = new Set<string>();
  for (const attempt of snap.attempts) {
    if (attempt.workerId !== reviewerId || attempt.kind !== "review") continue;
    if (taskId && attempt.taskId !== taskId) continue;
    ids.add(attempt.id);
  }
  return ids;
}

function includeTranscriptItem(item: Item, ids: Set<string>, reviewer: Worker): boolean {
  if (item.workerId !== reviewer.id) return false;
  if (item.attemptId && ids.has(item.attemptId)) return true;
  return !item.attemptId && Boolean(reviewer.currentAttemptId && ids.has(reviewer.currentAttemptId));
}

function mergeItems(fetched: readonly Item[], live: readonly Item[], reviewerId: string): Item[] {
  const map = new Map<number, Item>();
  for (const item of fetched) {
    if (item.workerId === reviewerId) map.set(item.id, item);
  }
  for (const item of live) {
    if (item.workerId === reviewerId) map.set(item.id, item);
  }
  return [...map.values()].sort((a, b) => a.id - b.id);
}

function StandingBadge({ standing }: { standing: ReviewStanding }) {
  const { t } = usePhrase();
  return <span className={`rv-standing rv-standing--${standing}`}>{t(`review.standing.${standing}`)}</span>;
}

function RoundMark({ round, max }: { round: number; max: number }) {
  const { t } = usePhrase();
  return (
    <span className="rv-round" title={t("review.round", { round, max })}>
      {t("review.roundShort", { round, max })}
    </span>
  );
}

function VerdictBadge({ round }: { round: ReviewRoundView }) {
  const { t, phrase } = usePhrase();
  const tone = verdictOf(round);
  const text = tone === "approved"
    ? t("review.verdict.approved")
    : tone === "changes"
      ? t("review.verdict.changes")
      : tone === "failed"
        ? phrase("异常失败", "Failed")
        : tone === "running"
          ? phrase("审查进行中", "Review in progress")
          : t("review.verdict.submitted");
  const code = tone === "approved" ? "approved" : tone === "changes" ? "changes_requested" : null;
  const failure = round.attempt.failure;
  return (
    <span className={`rv-verdict rv-verdict--${tone}`}>
      {text}
      {code && <span className="rv-verdict__code">{code}</span>}
      {tone === "failed" && failure && <span className="rv-verdict__code">{t(`fail.${failure.kind}`)}</span>}
    </span>
  );
}

function UsageLine({ attempt }: { attempt: ReviewTimelineStep["attempt"] }) {
  const { t } = usePhrase();
  return (
    <p className="rv-step__usage">
      <span>{t("panel.duration")} {fmtDuration(attempt.startedAt, attempt.endedAt)}</span>
      <span>
        {t("panel.tokens")}{" "}
        <Estimate text={fmtTokens(attemptTokens(attempt))} estimated={attempt.estimated?.tokens} />
      </span>
      <span>
        {t("panel.cost")}{" "}
        <Estimate text={fmtCost(attempt.cost)} estimated={attempt.estimated?.cost} />
      </span>
    </p>
  );
}

function DiffViewer({
  taskId,
  patchArtifactId,
  deltaPatchArtifactId,
}: {
  taskId: string;
  patchArtifactId?: string;
  deltaPatchArtifactId?: string;
}) {
  const { t, phrase } = usePhrase();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<DiffMode>(deltaPatchArtifactId ? "delta" : "full");
  const [text, setText] = useState("");
  const [truncated, setTruncated] = useState(false);
  const [nextOffset, setNextOffset] = useState(0);
  const [totalBytes, setTotalBytes] = useState(0);
  const [status, setStatus] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [error, setError] = useState<string | null>(null);
  const gen = useRef(0);
  const artifactId = mode === "delta" && deltaPatchArtifactId ? deltaPatchArtifactId : patchArtifactId;
  const both = Boolean(deltaPatchArtifactId && patchArtifactId);

  useEffect(() => {
    if (!open || !artifactId) return;
    const token = ++gen.current;
    let cancel = false;
    setStatus("loading");
    setError(null);
    fetchReviewDiff(taskId, artifactId)
      .then((slice) => {
        if (cancel || token !== gen.current) return;
        applySlice(slice, true);
        setStatus("ready");
      })
      .catch((err: unknown) => {
        if (cancel || token !== gen.current) return;
        setError(errorMessage(err));
        setStatus("error");
      });
    return () => {
      cancel = true;
    };
  }, [open, artifactId, taskId]);

  function applySlice(slice: ReviewDiffSlice, replace: boolean) {
    setText((prev) => (replace ? slice.content : prev + slice.content));
    setTotalBytes(slice.totalBytes);
    // Server slices strings with JS .slice(offset, offset + limit) (UTF-16 code units).
    const loaded = slice.offset + slice.content.length;
    setNextOffset(loaded);
    setTruncated(slice.truncated && slice.content.length > 0);
  }

  async function loadMore() {
    if (!artifactId || status === "loading") return;
    const token = gen.current;
    setStatus("loading");
    setError(null);
    try {
      const slice = await fetchReviewDiff(taskId, artifactId, { offset: nextOffset });
      if (token !== gen.current) return;
      if (!slice.content) {
        setTruncated(false);
        setStatus("ready");
        return;
      }
      applySlice(slice, false);
      setStatus("ready");
    } catch (err: unknown) {
      if (token !== gen.current) return;
      setError(errorMessage(err));
      setStatus("error");
    }
  }

  function choose(next: DiffMode) {
    if (next === mode) return;
    setMode(next);
    setText("");
    setTruncated(false);
    setError(null);
    setStatus(open ? "loading" : "idle");
  }

  const rows = useMemo(() => (text ? parseUnifiedDiff(text) : []), [text]);

  return (
    <div className="rv-diff-box">
      <div className="rv-diff-toolbar">
        {both ? (
          <div className="rv-diff-modes" role="group" aria-label={phrase("候选补丁", "Candidate patch")}>
            <button
              type="button"
              className={`rv-diff-mode${mode === "delta" ? " rv-diff-mode--on" : ""}`}
              aria-pressed={mode === "delta"}
              onClick={() => choose("delta")}
            >
              {t("review.diffDelta")}
              <span className="rv-diff-mode__hint">Delta Diff</span>
            </button>
            <button
              type="button"
              className={`rv-diff-mode${mode === "full" ? " rv-diff-mode--on" : ""}`}
              aria-pressed={mode === "full"}
              onClick={() => choose("full")}
            >
              {t("review.diffFull")}
              <span className="rv-diff-mode__hint">Full Diff</span>
            </button>
          </div>
        ) : (
          <span className="rv-diff-kind">{deltaPatchArtifactId && mode === "delta" ? t("review.diffDelta") : t("review.diffFull")}</span>
        )}
        <button
          type="button"
          className="rv-diff-toggle"
          aria-expanded={open}
          title={phrase("展开查看候选 Diff", "Expand the candidate diff")}
          onClick={() => setOpen((value) => !value)}
        >
          {open ? t("review.hideDiff") : phrase("展开查看候选 Diff", "Expand candidate diff")}
        </button>
      </div>
      {open && (
        <div className="rv-diff-body" aria-busy={status === "loading"}>
          {status === "loading" && text.length === 0 && <p className="rv-muted">{t("panel.loading")}</p>}
          {error && <p className="rv-error">{t("top.fetchFailed")} · {error}</p>}
          {rows.length > 0 && (
            <div className="rv-diff-pre">
              {rows.map((row, index) => (
                <div key={index} className={`rv-diff-line rv-diff-line--${row.kind}`}>
                  <span className="rv-diff-no">{row.oldNo ?? ""}</span>
                  <span className="rv-diff-no">{row.newNo ?? ""}</span>
                  <span className="rv-diff-sign">{row.sign}</span>
                  <code className="rv-diff-code">{row.text.length > 0 ? row.text : " "}</code>
                </div>
              ))}
            </div>
          )}
          {status === "ready" && rows.length === 0 && !error && <p className="rv-muted">{t("panel.none")}</p>}
          {totalBytes > 0 && text.length > 0 && (
            <p className="rv-diff-progress">{nextOffset}/{totalBytes}</p>
          )}
          {truncated && (
            <button type="button" className="rv-diff-more" disabled={status === "loading"} onClick={() => void loadMore()}>
              {phrase("加载更多补丁内容", "Load more of the patch")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function Findings({ findings }: { findings: ReviewRoundView["findings"] }) {
  const { t } = usePhrase();
  if (findings.length === 0) return null;
  return (
    <div className="rv-findings-wrap">
      <h4 className="rv-step__label">{t("review.findings")}</h4>
      <ul className="rv-findings">
        {findings.map((finding, index) => (
          <li key={`${finding.file ?? ""}:${finding.message}:${index}`} className="rv-finding">
            <span className={`rv-sev rv-sev--${severitySlug(finding.severity)}`}>{finding.severity || t("panel.none")}</span>
            {finding.file && <code className="rv-file">{finding.file}</code>}
            <span className="rv-finding__message">{finding.message}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function Timeline({ task, snap }: { task: Task; snap: RunSnapshot }) {
  const { t, phrase } = usePhrase();
  const steps = useMemo(
    () => buildReviewTimeline(task, snap.attempts, snap.workers),
    [task, snap.attempts, snap.workers],
  );
  const lastAuthorId = [...steps].reverse().find((step) => step.kind !== "review_round")?.attempt.id;
  const candidateSummary = task.candidateResult?.summary ?? (task.status === "succeeded" ? task.result?.summary : undefined);
  if (steps.length === 0) return <p className="rv-muted">{t("review.noRounds")}</p>;
  return (
    <ol className="rv-steps">
      {steps.map((step, index) => {
        const custom = step.worker ? workerCustomRole(step.worker) : null;
        if (step.kind === "review_round") {
          const round = step.roundView;
          return (
            <li key={step.attempt.id} className="rv-step rv-step--review_round" data-round={round.round}>
              <header className="rv-step__head">
                <span className="rv-pill rv-pill--reviewer">{badgeNum("R", step.reviewerNum)}</span>
                <span className="rv-step__title">{t("role.reviewer")} · {t("review.stepRound", { round: round.round })} · {t("review.readOnly")}</span>
                <VerdictBadge round={round} />
              </header>
              <p className="rv-step__who">{agentModel(step.attempt.agent, step.attempt.model, t("panel.none"))}</p>
              <UsageLine attempt={step.attempt} />
              {round.summary && (
                <p className="rv-step__summary"><span className="rv-step__label">{t("review.summary")}</span> {round.summary}</p>
              )}
              <Findings findings={round.findings} />
              {(round.deltaPatchArtifactId || round.patchArtifactId) && (
                <DiffViewer
                  taskId={task.id}
                  patchArtifactId={round.patchArtifactId}
                  deltaPatchArtifactId={round.deltaPatchArtifactId}
                />
              )}
              {step.attempt.failure && (
                <p className="rv-step__failure">{t("panel.failure")} · {step.attempt.failure.message}</p>
              )}
            </li>
          );
        }
        const prior = step.kind === "author_rework" ? priorReviewSummary(steps, index) : undefined;
        const isCoordinator = step.attempt.workerId === "coordinator";
        return (
          <li key={step.attempt.id} className={`rv-step rv-step--${step.kind}`}>
            <header className="rv-step__head">
              <span className="rv-pill rv-pill--author">
                {isCoordinator ? <span className="rv-chip">COORDINATOR</span> : badgeNum("W", step.workerNum)}
                {custom && <span className="rv-chip">{custom}</span>}
              </span>
              <span className="rv-step__title">
                {isCoordinator
                  ? phrase("主 Agent · 接管修复并收口", "Coordinator · Takeover & complete")
                  : `${phrase("执行者", "Author")} · ${step.kind === "author_submit"
                    ? phrase("初次提交候选", "Initial candidate")
                    : t("review.stepRework", { n: step.reworkIndex })}`}
              </span>
            </header>
            <p className="rv-step__who">{agentModel(step.attempt.agent, step.attempt.model, t("panel.none"))}</p>
            <UsageLine attempt={step.attempt} />
            {prior && (
              <p className="rv-step__note"><span className="rv-step__label">{phrase("返工原因", "Rework reason")}</span> {prior}</p>
            )}
            {step.attempt.hint && (
              <p className="rv-step__note"><span className="rv-step__label">{t("panel.reason")}</span> {step.attempt.hint}</p>
            )}
            {step.attempt.routeReason && (
              <p className="rv-step__note"><span className="rv-step__label">{t("panel.route")}</span> {step.attempt.routeReason}</p>
            )}
            {step.attempt.id === lastAuthorId && candidateSummary && (
              <p className="rv-step__note"><span className="rv-step__label">{t("panel.summary")}</span> {candidateSummary}</p>
            )}
            {step.attempt.failure && (
              <p className="rv-step__failure">{t("panel.failure")} · {t(`fail.${step.attempt.failure.kind}`)} — {step.attempt.failure.message}</p>
            )}
          </li>
        );
      })}
    </ol>
  );
}

function TranscriptItem({ item }: { item: Item }) {
  const { t } = usePhrase();
  const longUser = item.kind === "user" && (item.text?.length ?? 0) > 280;
  return (
    <li className={`rv-item rv-item--${item.kind}`}>
      <header className="rv-item__head">
        <span className="rv-item__kind">{t(`item.${item.kind}`)}</span>
        {item.tool?.status && <span className="rv-item__status">{item.tool.status}</span>}
        <time dateTime={item.ts} title={clockTime(item.ts)}>{relTime(item.ts)}</time>
      </header>
      {item.kind === "thought" ? (
        <details className="rv-item__thought" open>
          <summary>{item.text?.slice(0, 80) || t("item.thought")}</summary>
          <p className="rv-item__text">{item.text}</p>
        </details>
      ) : longUser ? (
        <details>
          <summary>{item.text?.slice(0, 80)}…</summary>
          <p className="rv-item__text">{item.text}</p>
        </details>
      ) : item.text && item.kind !== "tool" ? (
        <p className="rv-item__text">{item.text}</p>
      ) : null}
      {item.tool && (
        <div className="rv-tool">
          <div className="rv-tool__title">{item.tool.title || item.tool.kind || item.tool.id}</div>
          {item.tool.locations && item.tool.locations.length > 0 && (
            <div className="rv-tool__locs">
              {item.tool.locations.map((loc, index) => <code key={`${loc}:${index}`} className="rv-file">{loc}</code>)}
            </div>
          )}
          {item.tool.input != null && (
            <details>
              <summary>{t("item.input")}</summary>
              <pre className="rv-pre">{jsonPreview(item.tool.input)}</pre>
            </details>
          )}
          {item.tool.output && (
            <details open>
              <summary>{t("item.output")}</summary>
              <pre className="rv-pre">{item.tool.output}</pre>
            </details>
          )}
        </div>
      )}
      {item.usage && (
        <p className="rv-item__text">
          {t("panel.tokens")} {fmtTokens((item.usage.input ?? 0) + (item.usage.output ?? 0) + (item.usage.cacheRead ?? 0))}
          {" · "}
          {fmtCost(item.usage.cost)}
        </p>
      )}
    </li>
  );
}

function ReviewerTranscript({
  snap,
  task,
  reviewer,
  reviewNums,
  reviewers,
  onPickReviewer,
  liveItems,
  itemTick,
  board,
}: {
  snap: RunSnapshot;
  task: Task;
  reviewer: Worker;
  reviewNums: Map<string, number>;
  reviewers: Worker[];
  onPickReviewer: (id: string) => void;
  liveItems: Map<string, Item[]>;
  itemTick: Map<string, number>;
  board: ReviewBoard;
}) {
  const { t, phrase } = usePhrase();
  const [showFull, setShowFull] = useStreamFullDetails();
  const [scope, setScope] = useState<TranscriptScope>("task");
  const [page, setPage] = useState<Item[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const tick = itemTick.get(reviewer.id) ?? 0;
  const ids = useMemo(
    () => reviewAttemptIds(snap, reviewer.id, scope === "task" ? task.id : undefined),
    [snap, reviewer.id, scope, task.id],
  );
  const roundByAttempt = useMemo(() => {
    const map = new Map<string, number>();
    for (const round of board.rounds) map.set(round.attempt.id, round.round);
    return map;
  }, [board.rounds]);

  useEffect(() => {
    let cancel = false;
    setLoading(true);
    setError(null);
    setPage([]);
    fetchItems(reviewer.id)
      .then((fresh) => {
        if (cancel) return;
        setPage(fresh.items.filter((item) => item.workerId === reviewer.id));
        setHasMore(fresh.hasMore);
      })
      .catch((err: unknown) => {
        if (!cancel) setError(errorMessage(err));
      })
      .finally(() => {
        if (!cancel) setLoading(false);
      });
    return () => {
      cancel = true;
    };
  }, [reviewer.id]);

  const merged = useMemo(
    () => mergeItems(page, liveItems.get(reviewer.id) ?? [], reviewer.id),
    [page, liveItems, reviewer.id, tick],
  );
  const visible = merged.filter((item) => includeTranscriptItem(item, ids, reviewer));
  const { visible: displayed, hiddenCount } = useMemo(
    () => filterStreamItems(visible, showFull),
    [visible, showFull],
  );
  const groups: { attemptId: string; items: Item[] }[] = [];
  const groupAt = new Map<string, number>();
  for (const item of displayed) {
    const key = item.attemptId ?? reviewer.currentAttemptId ?? "live";
    const at = groupAt.get(key);
    if (at == null) {
      groupAt.set(key, groups.length);
      groups.push({ attemptId: key, items: [item] });
    } else {
      groups[at]?.items.push(item);
    }
  }

  async function loadEarlier() {
    const oldest = page.reduce((min, item) => Math.min(min, item.id), Number.POSITIVE_INFINITY);
    if (!Number.isFinite(oldest)) return;
    setLoading(true);
    try {
      const fresh = await fetchItems(reviewer.id, oldest);
      setPage((prev) => mergeItems(prev, fresh.items, reviewer.id));
      setHasMore(fresh.hasMore);
      setError(null);
    } catch (err: unknown) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  const num = reviewNums.get(reviewer.id);
  return (
    <div className="rv-transcript">
      <header className="rv-transcript__bar">
        <div>
          <strong>{badgeNum("R", num)}</strong>
          <span className="rv-plaque">{t("role.reviewer")} · {phrase("只读快照", "read-only snapshot")}</span>
        </div>
        {reviewers.length > 1 && (
          <div className="rv-reviewer-switch" role="tablist" aria-label={t("role.reviewer")}>
            {reviewers.map((worker) => {
              const on = worker.id === reviewer.id;
              return (
                <button
                  key={worker.id}
                  type="button"
                  role="tab"
                  aria-selected={on}
                  className={`rv-reviewer-switch__btn${on ? " rv-reviewer-switch__btn--on" : ""}`}
                  onClick={() => onPickReviewer(worker.id)}
                >
                  {badgeNum("R", reviewNums.get(worker.id))}
                </button>
              );
            })}
          </div>
        )}
        <div className="rv-scope" role="group">
          <button
            type="button"
            className={`rv-scope__btn${scope === "task" ? " rv-scope__btn--on" : ""}`}
            aria-pressed={scope === "task"}
            onClick={() => setScope("task")}
          >
            {phrase("当前任务的审查回合", "This task's review attempts")}
          </button>
          <button
            type="button"
            className={`rv-scope__btn${scope === "all" ? " rv-scope__btn--on" : ""}`}
            aria-pressed={scope === "all"}
            onClick={() => setScope("all")}
          >
            {phrase("该审查员的全部审查回合", "All review rounds by this reviewer")}
          </button>
        </div>
        <StreamDetailToggle showFull={showFull} onChange={setShowFull} hiddenCount={hiddenCount} />
      </header>
      <p className="rv-muted">{t("review.readonly")}</p>
      {hasMore && (
        <button type="button" className="rv-transcript__earlier" disabled={loading} onClick={() => void loadEarlier()}>
          {t("panel.loadEarlier")}
        </button>
      )}
      {loading && page.length === 0 && <p className="rv-muted">{t("panel.loading")}</p>}
      {error && <p className="rv-error">{t("top.fetchFailed")} · {error}</p>}
      {!loading && visible.length === 0 && <p className="rv-muted">{t("panel.streamEmpty")}</p>}
      {!loading && visible.length > 0 && displayed.length === 0 && (
        <p className="rv-muted">{t("panel.streamHiddenOnly", { n: hiddenCount })}</p>
      )}
      {groups.map((group) => {
        const round = roundByAttempt.get(group.attemptId);
        return (
          <section key={group.attemptId} className="rv-transcript__group">
            <h4 className="rv-transcript__round">
              {badgeNum("R", num)}
              {" · "}
              {round != null ? t("review.stepRound", { round }) : shortId(group.attemptId)}
            </h4>
            <ul className="rv-items">
              {group.items.map((item) => <TranscriptItem key={item.id} item={item} />)}
            </ul>
          </section>
        );
      })}
    </div>
  );
}

function AuthorCard({
  task,
  author,
  authorNums,
  submits,
  reworks,
  onJumpToDag,
}: {
  task: Task;
  author?: Worker;
  authorNums: Map<string, number>;
  submits: number;
  reworks: number;
  onJumpToDag: (selectionId: string) => void;
}) {
  const { t, phrase } = usePhrase();
  const custom = author ? workerCustomRole(author) : null;
  const isolated = Boolean(author?.worktree) || task.isolation === "worktree";
  const body = (
    <>
      <span className="rv-role__kicker">{phrase("执行者", "Author worker")}</span>
      <span className="rv-role__id">{author ? badgeNum("W", authorNums.get(author.id)) : "W—"}</span>
      <span className="rv-role__name">{t("review.authorCard")}</span>
      {custom && <span className="rv-chip" title={t("role.custom")}>{custom}</span>}
      <span className="rv-field">
        <span className="rv-field__key">{t("panel.agent")} / {t("panel.model")}</span>
        <span>{agentModel(author?.agent, author?.model, t("panel.none"))}</span>
      </span>
      <span className="rv-field">
        <span className="rv-field__key">{t("panel.cwd")}</span>
        <span className={`rv-iso rv-iso--${isolated ? "worktree" : "main"}`}>
          {isolated ? "worktree" : phrase("主目录", "main directory")}
        </span>
        <code className="rv-path">{author ? (author.worktree ?? author.cwd) : t("panel.none")}</code>
      </span>
      <span className="rv-counts">
        <span>{phrase(`提交 ${submits} 次`, `${submits} submission${submits === 1 ? "" : "s"}`)}</span>
        <span>{phrase(`返工 ${reworks} 次`, `${reworks} rework${reworks === 1 ? "" : "s"}`)}</span>
      </span>
      {author && <span className="rv-role__jump">{t("review.jumpDag")}</span>}
    </>
  );
  if (!author) {
    return <div className="rv-role rv-role--author rv-role--empty">{body}<span className="rv-muted">{t("panel.noRelated")}</span></div>;
  }
  return (
    <button type="button" className="rv-role rv-role--author" onClick={() => onJumpToDag(workerNodeId(author.id))}>
      {body}
    </button>
  );
}

function ReviewerCard({
  task,
  author,
  reviewer,
  reviewers,
  reviewNums,
  board,
  onJumpToDag,
  onPickReviewer,
}: {
  task: Task;
  author?: Worker;
  reviewer?: Worker;
  reviewers: Worker[];
  reviewNums: Map<string, number>;
  board: ReviewBoard;
  onJumpToDag: (selectionId: string) => void;
  onPickReviewer: (id: string) => void;
}) {
  const { t, phrase } = usePhrase();
  const executed = reviewer ? board.rounds.filter((round) => round.attempt.workerId === reviewer.id).length : 0;
  const compared = Boolean(author?.agent && reviewer?.agent);
  const hetero = compared && author?.agent !== reviewer?.agent;
  const body = (
    <>
      <span className="rv-role__kicker">{phrase("审查员", "Reviewer agent")}</span>
      <span className="rv-role__id">{reviewer ? badgeNum("R", reviewNums.get(reviewer.id)) : "R—"}</span>
      <span className="rv-role__name">{t("review.reviewerCard")}</span>
      <span className="rv-plaque">{t("role.reviewer")} · {phrase("只读快照", "read-only snapshot")}</span>
      {compared && (
        <span
          className={`rv-hetero rv-hetero--${hetero ? "yes" : "no"}`}
          title={phrase(
            `作者 ${author?.agent}，审查员 ${reviewer?.agent}`,
            `Author ${author?.agent}, reviewer ${reviewer?.agent}`,
          )}
        >
          {hetero ? t("review.heterogeneous") : phrase("同构 Agent", "Same agent")}
        </span>
      )}
      <span className="rv-field">
        <span className="rv-field__key">{t("panel.agent")} / {t("panel.model")}</span>
        <span>{agentModel(reviewer?.agent ?? task.review?.agent, reviewer?.model ?? task.review?.model, t("panel.none"))}</span>
      </span>
      <span className="rv-field">
        <span className="rv-field__key">{phrase("只读快照", "Read-only snapshot")}</span>
        <code className="rv-path">{reviewer?.readOnlyDir || t("panel.none")}</code>
      </span>
      <span className="rv-counts">
        <span>{phrase(`已执行 ${executed} 轮审查`, `${executed} review round${executed === 1 ? "" : "s"} run`)}</span>
      </span>
      {reviewer && <span className="rv-role__jump">{t("review.jumpDag")}</span>}
    </>
  );
  return (
    <div className="rv-role-slot">
      {reviewers.length > 1 && (
        <div className="rv-reviewer-switch" role="tablist" aria-label={t("role.reviewer")}>
          {reviewers.map((worker) => {
            const on = worker.id === reviewer?.id;
            return (
              <button
                key={worker.id}
                type="button"
                role="tab"
                aria-selected={on}
                className={`rv-reviewer-switch__btn${on ? " rv-reviewer-switch__btn--on" : ""}`}
                onClick={() => onPickReviewer(worker.id)}
              >
                {badgeNum("R", reviewNums.get(worker.id))}
              </button>
            );
          })}
        </div>
      )}
      {reviewer ? (
        <button type="button" className="rv-role rv-role--reviewer" onClick={() => onJumpToDag(reviewerNodeId(reviewer.id))}>
          {body}
        </button>
      ) : (
        <div className="rv-role rv-role--reviewer rv-role--empty">{body}<span className="rv-muted">{t("panel.noRelated")}</span></div>
      )}
    </div>
  );
}

function ReviewDetail({
  task,
  board,
  snap,
  authorNums,
  reviewNums,
  onJumpToDag,
  onOpenTask,
  liveItems,
  itemTick,
  outsideFilter,
}: {
  task: Task;
  board: ReviewBoard;
  snap: RunSnapshot;
  authorNums: Map<string, number>;
  reviewNums: Map<string, number>;
  onJumpToDag: (selectionId: string) => void;
  onOpenTask?: (taskId: string) => void;
  liveItems: Map<string, Item[]>;
  itemTick: Map<string, number>;
  outsideFilter: boolean;
}) {
  const { t, phrase } = usePhrase();
  const cast = useMemo(() => reviewCast(task, snap), [task, snap]);
  const [tab, setTab] = useState<"process" | "session">("process");
  const [reviewerId, setReviewerId] = useState<string | null>(cast.reviewers.at(-1)?.id ?? null);
  const reviewer = cast.reviewers.find((worker) => worker.id === reviewerId) ?? cast.reviewers.at(-1);
  const approved = task.reviewState?.approvedTreeSha;
  const circuit = reviewCircuitView(task, t);
  return (
    <article className="rv-detail">
      <header className="rv-detail__bar">
        <div className="rv-detail__title">
          <h2>{task.title}</h2>
          <code className="rv-key">{task.key}</code>
        </div>
        <StandingBadge standing={board.standing} />
        <RoundMark round={board.round} max={board.maxRounds} />
        {circuit && (
          <span className={`circuit-pill circuit-pill--${circuit.tone}`} title={circuit.description}>
            {circuit.shortLabel}
          </span>
        )}
        {onOpenTask && (
          <button type="button" className="rv-jump" onClick={() => onOpenTask(task.id)}>
            {t("wb.openInTasks")}
          </button>
        )}
        <button type="button" className="rv-jump" onClick={() => onJumpToDag(task.id)}>
          {t("review.jumpDag")}
        </button>
      </header>
      {outsideFilter && <p className="rv-filter-note">{phrase("该任务不在当前筛选中", "This task is outside the current filter")}</p>}
      {circuit && (circuit.tone === "handoff" || circuit.tone === "post_handoff" || circuit.tone === "awaiting") && (
        <div className={`circuit-banner circuit-banner--${circuit.tone}`}>
          <div className="circuit-banner__head">
            <span className="circuit-banner__title">{t("review.circuit.title")} · {circuit.label}</span>
            <span className="circuit-banner__meta mono">
              R{circuit.rounds}/{circuit.maxRounds}
              {circuit.overrides > 0 ? ` · ${t("review.circuit.overrides", { n: circuit.overrides })}` : ""}
            </span>
          </div>
          <p className="circuit-banner__desc">{circuit.description}</p>
          {circuit.history.length > 0 && (
            <ul className="circuit-banner__history">
              {circuit.history.map((rec) => (
                <li key={`${rec.round}:${rec.at}`}>
                  <strong className="mono">R{rec.round}{rec.postHandoff ? "*" : ""}</strong>
                  {" · "}
                  <span>{rec.summary}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      <div className="rv-role-grid">
        <AuthorCard
          task={task}
          author={cast.author}
          authorNums={authorNums}
          submits={cast.submits}
          reworks={cast.reworks}
          onJumpToDag={onJumpToDag}
        />
        <ReviewerCard
          task={task}
          author={cast.author}
          reviewer={reviewer}
          reviewers={cast.reviewers}
          reviewNums={reviewNums}
          board={board}
          onJumpToDag={onJumpToDag}
          onPickReviewer={setReviewerId}
        />
      </div>
      <section className="rv-criteria">
        <h3 className="rv-criteria__title">{t("review.criteria")}</h3>
        <p className="rv-criteria__text">{task.review?.criteria?.trim() || t("panel.none")}</p>
        <dl className="rv-criteria__meta">
          <div>
            <dt>{phrase("最大轮次", "Max rounds")}</dt>
            <dd>{board.maxRounds}</dd>
          </div>
          <div>
            <dt>{phrase("当前轮次", "Current round")}</dt>
            <dd>{t("review.roundShort", { round: board.round, max: board.maxRounds })}</dd>
          </div>
          {approved && (
            <div>
              <dt>{phrase("批准树", "Approved tree")} approvedTreeSha</dt>
              <dd><code className="rv-criteria__sha" title={approved}>{approved}</code></dd>
            </div>
          )}
        </dl>
      </section>
      <div className="rv-subtabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={tab === "process"}
          className={`rv-subtab${tab === "process" ? " rv-subtab--on" : ""}`}
          onClick={() => setTab("process")}
        >
          {phrase("审查闭环时间线", "Review loop")}
          <span className="rv-subtab__hint">Process & Diff</span>
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "session"}
          className={`rv-subtab${tab === "session" ? " rv-subtab--on" : ""}`}
          onClick={() => setTab("session")}
        >
          {phrase("Reviewer 会话交互流", "Reviewer transcript")}
          <span className="rv-subtab__hint">{t("review.sessionTab")}</span>
        </button>
      </div>
      {tab === "process" ? (
        <div role="tabpanel" className="rv-panel">
          <Timeline task={task} snap={snap} />
        </div>
      ) : reviewer ? (
        <div role="tabpanel" className="rv-panel">
          <ReviewerTranscript
            snap={snap}
            task={task}
            reviewer={reviewer}
            reviewNums={reviewNums}
            reviewers={cast.reviewers}
            onPickReviewer={setReviewerId}
            liveItems={liveItems}
            itemTick={itemTick}
            board={board}
          />
        </div>
      ) : (
        <p className="rv-muted">{t("review.noRounds")}</p>
      )}
    </article>
  );
}

function TaskCard({
  task,
  board,
  snap,
  authorNums,
  reviewNums,
  selected,
  onSelect,
}: {
  task: Task;
  board: ReviewBoard;
  snap: RunSnapshot;
  authorNums: Map<string, number>;
  reviewNums: Map<string, number>;
  selected: boolean;
  onSelect: (taskId: string) => void;
}) {
  const { t, phrase } = usePhrase();
  const cast = useMemo(() => reviewCast(task, snap), [task, snap]);
  const author = cast.author;
  const reviewer = cast.reviewers.at(-1);
  const custom = author ? workerCustomRole(author) : null;
  const circuit = reviewCircuitView(task, t);
  return (
    <button
      type="button"
      className={`rv-task-card${selected ? " rv-task-card--on" : ""}`}
      aria-current={selected ? "true" : undefined}
      data-standing={board.standing}
      onClick={() => onSelect(task.id)}
    >
      <span className="rv-task-card__title">{task.title}</span>
      <code className="rv-key">{task.key}</code>
      <span className="rv-task-card__meta">
        <ReviewRequirement required />
        <StandingBadge standing={board.standing} />
        <RoundMark round={board.round} max={board.maxRounds} />
        {circuit && (
          <span className={`circuit-pill circuit-pill--${circuit.tone}`} title={circuit.description}>
            {circuit.shortLabel}
          </span>
        )}
      </span>
      <span className="rv-pair">
        <span className="rv-pill rv-pill--author">
          <span className="rv-pill__role">{phrase("执行者", "Author")}</span>
          <span className="rv-pill__id">{author ? badgeNum("W", authorNums.get(author.id)) : "W—"}</span>
          {custom && <span className="rv-chip">{custom}</span>}
        </span>
        <span className="rv-pair__arrow" aria-hidden="true">⇢</span>
        <span className="rv-pill rv-pill--reviewer">
          <span className="rv-pill__role">{t("role.reviewer")}</span>
          <span className="rv-pill__id">{reviewer ? badgeNum("R", reviewNums.get(reviewer.id)) : "R—"}</span>
        </span>
      </span>
    </button>
  );
}

function EmptyState({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <div className="rv-empty">
      <h2>{title}</h2>
      {children}
    </div>
  );
}

export function ReviewsView({
  snap,
  selectedTaskId,
  onSelectTask,
  onJumpToDag,
  onOpenTask,
  liveItems,
  itemTick,
}: ReviewsViewProps) {
  const { t, phrase } = usePhrase();
  const [filter, setFilter] = useState<ReviewFilter>("all");
  const requested = useRef<string | null | undefined>(undefined);
  const reviewed = useMemo(() => (snap ? reviewedTasks(snap) : []), [snap]);
  const unreviewed = useMemo(() => snap?.tasks.filter((task) => !taskRequiresReview(task, snap.attempts)) ?? [], [snap]);
  const filtered = useMemo(
    () => reviewed.filter((row) => standingMatches(row.board.standing, filter)),
    [reviewed, filter],
  );
  const authorNums = useMemo(
    () => (snap ? workerNumbers(authorWorkers(snap.workers)) : new Map<string, number>()),
    [snap],
  );
  const reviewNums = useMemo(
    () => (snap ? reviewerNumbers(snap.workers) : new Map<string, number>()),
    [snap],
  );
  const counts = useMemo(() => {
    const tally = { all: reviewed.length, active: 0, approved: 0, rejected: 0, waiting: 0 };
    for (const row of reviewed) {
      if (standingMatches(row.board.standing, "active")) tally.active += 1;
      else if (row.board.standing === "approved") tally.approved += 1;
      else if (standingMatches(row.board.standing, "rejected")) tally.rejected += 1;
      else if (row.board.standing === "waiting") tally.waiting += 1;
    }
    return tally;
  }, [reviewed]);

  useEffect(() => {
    if (!snap) return;
    const inReviewed = selectedTaskId != null && reviewed.some(({ task }) => task.id === selectedTaskId);
    if (inReviewed) {
      requested.current = undefined;
      return;
    }
    const next = filtered[0]?.task.id ?? null;
    if (next === selectedTaskId || requested.current === next) return;
    requested.current = next;
    onSelectTask(next);
  }, [snap, selectedTaskId, reviewed, filtered, onSelectTask]);

  const inReviewed = Boolean(snap && selectedTaskId && reviewed.some(({ task }) => task.id === selectedTaskId));
  const displayId = inReviewed ? selectedTaskId : (filtered[0]?.task.id ?? null);
  const displayed = reviewed.find((row) => row.task.id === displayId) ?? null;
  const outsideFilter = Boolean(displayed && !filtered.some((row) => row.task.id === displayed.task.id));

  return (
    <section className="rv" aria-label={t("page.reviews")}>
      <header className="rv-header">
        <div className="rv-header__lead">
          <h1>{t("page.reviews")}</h1>
          <p className="rv-header__hint">{t("review.required")} {reviewed.length} · {t("review.notRequired")} {unreviewed.length}</p>
        </div>
        {snap && reviewed.length > 0 && (
          <div className="rv-filters" role="tablist" aria-label={t("review.title")}>
            <button type="button" role="tab" aria-selected={filter === "all"} className={`rv-filter${filter === "all" ? " rv-filter--on" : ""}`} onClick={() => setFilter("all")}>
              {t("review.filterAll")} ({counts.all})
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === "active"}
              className={`rv-filter${filter === "active" ? " rv-filter--on" : ""}`}
              title={`${t("review.standing.reviewing")} / ${t("review.standing.rework")}`}
              onClick={() => setFilter("active")}
            >
              {t("review.standing.reviewing")} / {t("phase.rework")} ({counts.active})
            </button>
            <button type="button" role="tab" aria-selected={filter === "approved"} className={`rv-filter${filter === "approved" ? " rv-filter--on" : ""}`} onClick={() => setFilter("approved")}>
              {t("review.filterApproved")} ({counts.approved})
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === "rejected"}
              className={`rv-filter${filter === "rejected" ? " rv-filter--on" : ""}`}
              title={`${t("review.standing.rejected")} / ${t("review.standing.broken")} / ${t("review.standing.stopped")}`}
              onClick={() => setFilter("rejected")}
            >
              {t("review.filterRejected")} / {phrase("异常", "Error")} ({counts.rejected})
            </button>
            <button type="button" role="tab" aria-selected={filter === "waiting"} className={`rv-filter${filter === "waiting" ? " rv-filter--on" : ""}`} title={t("review.standing.waiting")} onClick={() => setFilter("waiting")}>
              {phrase("待开始", "Waiting")} ({counts.waiting})
            </button>
          </div>
        )}
      </header>
      {!snap && (
        <EmptyState title={t("empty.pickTitle")}>
          <p>{t("empty.pickBody")}</p>
          <p className="rv-muted">{t("panel.loading")}</p>
        </EmptyState>
      )}
      {snap && snap.tasks.length === 0 && (
        <EmptyState title={t("review.emptyRun")}>
          <p>
            {phrase(
              "在任务上配置 review（指定审查 agent、criteria、maxRounds，并把 differentAgent 设为 true），即可由另一名 Agent 在只读快照里做异构审查；不通过则打回原作者返工。",
              "Set review on a task (reviewer agent, criteria, maxRounds, and differentAgent: true) to have a different agent review a read-only snapshot. A rejection sends the author back to rework.",
            )}
          </p>
          <p>{t("review.readonly")}</p>
          <button type="button" className="rv-jump" onClick={() => onJumpToDag(COORD_ID)}>
            {phrase("返回", "Back to")} {t("top.dag")}
          </button>
        </EmptyState>
      )}
      {snap && snap.tasks.length > 0 && (
        <div className="rv-body">
          <div className="rv-list">
            <h2 className="rv-list__title">{t("review.required")} ({reviewed.length})</h2>
            {filtered.length === 0 && <p className="rv-muted">{t("select.noMatch")}</p>}
            {filtered.map((row) => (
              <TaskCard
                key={row.task.id}
                task={row.task}
                board={row.board}
                snap={snap}
                authorNums={authorNums}
                reviewNums={reviewNums}
                selected={row.task.id === displayId}
                onSelect={onSelectTask}
              />
            ))}
            {unreviewed.length > 0 && (
              <section className="rv-list__unreviewed" aria-label={t("review.notRequired")}>
                <h2 className="rv-list__title">{t("review.notRequired")} ({unreviewed.length})</h2>
                <p className="rv-list__hint">{t("review.notRequiredListHint")}</p>
                {unreviewed.map((task) => (
                  <button type="button" key={task.id} className="rv-task-card" title={t("review.jumpDag")} onClick={() => onJumpToDag(task.id)}>
                    <span className="rv-task-card__title">{task.title}</span>
                    <code className="rv-key">{task.key}</code>
                    <span className="rv-task-card__meta">
                      <ReviewRequirement required={false} />
                      <span>{t(`status.${task.status}`)}</span>
                    </span>
                  </button>
                ))}
              </section>
            )}
          </div>
          {displayed ? (
            <ReviewDetail
              key={displayed.task.id}
              task={displayed.task}
              board={displayed.board}
              snap={snap}
              authorNums={authorNums}
              reviewNums={reviewNums}
              onJumpToDag={onJumpToDag}
              onOpenTask={onOpenTask}
              liveItems={liveItems}
              itemTick={itemTick}
              outsideFilter={outsideFilter}
            />
          ) : (
            <div className="rv-detail rv-detail--empty">
              <p className="rv-muted">{reviewed.length === 0 ? t("review.emptyRun") : phrase("从左侧选择一个开启审查的任务", "Pick a reviewed task on the left")}</p>
              <p className="rv-muted">{t("review.notRequiredListHint")}</p>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
