import { useEffect, useMemo, useState } from "react";
import { DoodleSelect } from "./DoodleSelect";
import { useT } from "../i18n";
import { answeredQuestionIds, messageHop, messageParty, openQuestions } from "../relate";
import { authorWorkers } from "../review";
import { workerNumbers } from "../format";
import { clockTime, relTime } from "../time";
import type { Message, MessageParty, MessageType, RunSnapshot, TalkPlay } from "../types";

const ALL = "__all__";
const NEEDS = "__needs__";

export const MSG_LABEL: Record<MessageType, string> = {
  dispatch: "任务派发",
  question: "提问",
  escalation: "升级",
  failed: "失败",
  done: "完成",
  budget: "预算",
  run_done: "运行结束",
  note: "进展",
  reply: "回复",
  system: "系统",
};

export function msgTypeLabel(m: Message): string {
  if (m.type === "dispatch") {
    if (m.subject.startsWith("发起审查")) return "审查派发";
    if (m.subject.startsWith("审查返工") || m.subject.startsWith("返工派发")) return "返工派发";
    if (m.subject.startsWith("重试派发")) return "重试派发";
    if (m.subject.startsWith("复用派发")) return "复用派发";
    return "任务派发";
  }
  if (m.type === "note" && m.from === "coordinator") {
    return m.subject.startsWith("收尾提醒") ? "收尾提醒" : "编排者插话";
  }
  if (m.type === "reply") {
    return "编排者回复";
  }
  return MSG_LABEL[m.type] ?? m.type;
}

export function Party({
  label,
  party,
  onSelect,
}: {
  label: string;
  party: MessageParty | null;
  onSelect?: (id: string) => void;
}) {
  const { t } = useT();
  if (!party) {
    return (
      <span className="party">
        <span className="party__label">{label}</span>
        <span className="dim">—</span>
      </span>
    );
  }
  const roleLabel = party.reviewer
    ? t("role.reviewer")
    : party.role === "coord"
      ? t("role.coordinator")
      : party.role === "worker"
        ? t("role.worker")
        : party.role === "task"
          ? t("role.task")
          : t("role.system");
  const roleKind = party.reviewer ? "reviewer" : party.role === "system" ? "task" : party.role;
  const badgeNum = party.reviewer ? party.reviewerNum : party.workerNum;
  const badgeText = party.reviewer
    ? (badgeNum != null ? `R${badgeNum}` : null)
    : badgeNum != null
      ? `W${badgeNum}`
      : null;
  const who =
    party.role === "worker"
      ? [party.name, party.task].filter(Boolean).join(" · ")
      : party.name;
  const body = (
    <>
      <span className={`role-badge role-badge--${roleKind}`}>{roleLabel}</span>
      {badgeText && <span className="worker-num">{badgeText}</span>}
      {who && <span className="party__who">{who}</span>}
    </>
  );
  return (
    <span className="party" title={t("msg.partyTitle", { raw: party.raw })}>
      <span className="party__label">{label}</span>
      {party.select && onSelect ? (
        <button
          type="button"
          className="party__btn"
          onClick={(e) => {
            e.stopPropagation();
            onSelect(party.select!);
          }}
          onKeyDown={(e) => e.stopPropagation()}
        >
          {body}
        </button>
      ) : (
        <span className="party__btn party__btn--static">{body}</span>
      )}
    </span>
  );
}

interface ThreadGroup {
  root: Message;
  replies: Message[];
}

function threadGroups(talk: Message[]): ThreadGroup[] {
  const byId = new Map(talk.map((m) => [m.id, m]));
  const groups = new Map<string, ThreadGroup>();
  const rootOf = (m: Message) => (m.threadId && m.threadId !== m.id && byId.has(m.threadId) ? m.threadId : m.id);
  for (const m of talk) if (rootOf(m) === m.id) groups.set(m.id, { root: m, replies: [] });
  for (const m of talk) {
    const r = rootOf(m);
    if (r !== m.id) groups.get(r)?.replies.push(m);
  }
  for (const g of groups.values()) {
    g.replies.sort((a, b) => (a.createdAt < b.createdAt ? -1 : a.createdAt > b.createdAt ? 1 : a.id < b.id ? -1 : 1));
  }
  return [...groups.values()].sort((a, b) =>
    a.root.createdAt < b.root.createdAt ? -1 : a.root.createdAt > b.root.createdAt ? 1 : a.root.id < b.root.id ? -1 : 1,
  );
}

function shortPartyTag(p: MessageParty | null, t: (k: string) => string): string {
  if (!p) return "—";
  if (p.reviewer) return p.reviewerNum != null ? `R${p.reviewerNum}` : t("role.reviewer");
  if (p.role === "coord") return t("role.coordinator");
  if (p.role === "worker") return p.workerNum != null ? `W${p.workerNum}` : t("role.worker");
  if (p.role === "task") return p.name ?? t("role.task");
  return t("role.system");
}

function MessageCard({
  m,
  snap,
  wNums,
  reply,
  open,
  escalated,
  answered,
  taskLabel,
  wide = false,
  onSelect,
}: {
  m: Message;
  snap: RunSnapshot;
  wNums: Map<string, number>;
  reply: boolean;
  open: boolean;
  escalated: boolean;
  answered: boolean;
  taskLabel: string | null;
  wide?: boolean;
  onSelect?: (id: string) => void;
}) {
  const { t } = useT();
  const fromCoord = m.from === "coordinator";
  const typeClass = m.type === "done" ? "worker_done" : m.type === "failed" ? "escalation" : m.type;
  const handled = !open && !escalated && m.needsAction && Boolean(m.handledAt);
  const fromParty = messageParty(m.from, m, snap, wNums);
  const toParty = messageParty(m.to, m, snap, wNums);
  const dirText = `${shortPartyTag(fromParty, t)} → ${shortPartyTag(toParty, t)}`;
  const isDispatch = m.type === "dispatch";
  const bodyLines = m.body ? m.body.split("\n") : [];
  const dispatchLead = isDispatch && bodyLines.length > 0 && bodyLines[0].startsWith("路由策略：") ? bodyLines[0] : null;
  const dispatchRest = isDispatch ? (dispatchLead ? bodyLines.slice(1).join("\n") : m.body) : null;

  const head = (
    <div className="msg__head">
      {reply && <span className="msg__reply-mark" aria-hidden="true">↳</span>}
      <span className={`msg__type msg__type--${typeClass}`}>{msgTypeLabel(m)}</span>
      <span className={`msg__dir${fromCoord ? " msg__dir--out" : " msg__dir--in"}`}>{dirText}</span>
      {open && <span className="msg__tag msg__tag--ask">{t("msg.waiting")}</span>}
      {escalated && <span className="msg__tag msg__tag--esc">{t("msg.needsAction")}</span>}
      {answered && <span className="msg__tag msg__tag--ok">{t("msg.answered")}</span>}
      {!answered && handled && <span className="msg__tag msg__tag--ok">{t("msg.handled")}</span>}
      <span className="msg__time" title={clockTime(m.createdAt)}>
        {relTime(m.createdAt)}
      </span>
    </div>
  );

  const content = (
    <>
      {!(reply && /^(re:|回复：)/i.test(m.subject)) && <div className="msg__subject">{m.subject}</div>}
      {isDispatch ? (
        <>
          {dispatchLead && <p className="msg__body">{dispatchLead}</p>}
          {dispatchRest && (
            <details className="msg__details" onClick={(e) => e.stopPropagation()}>
              <summary>查看派发规格与交接详情</summary>
              <p className="msg__body">{dispatchRest}</p>
            </details>
          )}
        </>
      ) : (
        m.body && m.body !== m.subject && <p className="msg__body">{m.body}</p>
      )}
      {m.options && m.options.length > 0 && (
        <div className="msg__meta hint">
          选项：{m.options.map((opt) => `${opt === m.recommend ? "★ " : ""}${opt}`).join(" / ")}
        </div>
      )}
      {m.why && <div className="msg__meta hint">推荐理由：{m.why}</div>}
      {m.aiDraft && (
        <div className="msg__meta hint">
          <strong>{t("msg.aiDraftTitle")} ({Math.round(m.aiDraft.confidence * 100)}%{m.aiDraft.autoReplied ? " · auto" : ""}):</strong>{" "}
          <span>{m.aiDraft.answer}</span>
          {m.aiDraft.sources.length > 0 && (
            <div className="dim">{t("msg.aiDraftSources")}: {m.aiDraft.sources.join(" / ")}</div>
          )}
        </div>
      )}
      {!reply && m.taskId && taskLabel && (
        <div className="msg__meta dim">
          关联任务：
          {onSelect ? (
            <button
              type="button"
              className="link-btn"
              onClick={(e) => {
                e.stopPropagation();
                onSelect(m.taskId!);
              }}
              onKeyDown={(e) => e.stopPropagation()}
            >
              {taskLabel}
            </button>
          ) : (
            <span>{taskLabel}</span>
          )}
        </div>
      )}
      {open && <p className="hint ask-hint">{t("msg.answerInAgent")}</p>}
    </>
  );

  return (
    <div
      className={`msg msg--${typeClass}${fromCoord ? " msg--out" : " msg--in"}${reply ? " msg--reply" : ""}${open || escalated ? " msg--open" : ""}${wide ? " msg--wide" : ""}`}
    >
      {wide ? (
        <>
          <div className="msg__lane msg__lane--from">
            <Party label={t("msg.from")} party={fromParty} onSelect={onSelect} />
          </div>
          <div className="msg__center">
            {head}
            {content}
          </div>
          <div className="msg__lane msg__lane--to">
            <Party label={t("msg.to")} party={toParty} onSelect={onSelect} />
          </div>
        </>
      ) : (
        <>
          {head}
          <div className="msg__route">
            <Party label={t("msg.from")} party={fromParty} onSelect={onSelect} />
            <Party label={t("msg.to")} party={toParty} onSelect={onSelect} />
          </div>
          {content}
        </>
      )}
    </div>
  );
}

export interface MessageFocusRequest {
  nonce: number;
  type?: string;
  taskId?: string;
  dir?: "all" | "out" | "in";
}

export function MessageThread({
  messages,
  all,
  snap,
  onSelect,
  playing = null,
  onPlay,
  filters = false,
  title,
  bare = false,
  focusRequest = null,
}: {
  messages: Message[];
  all: Message[];
  snap: RunSnapshot;
  onSelect?: (id: string) => void;
  playing?: string | null;
  onPlay?: (play: TalkPlay | null) => void;
  filters?: boolean;
  title?: string;
  bare?: boolean;
  focusRequest?: MessageFocusRequest | null;
}) {
  const { t } = useT();
  const [dirFilter, setDirFilter] = useState<"all" | "out" | "in">("all");
  const [typeFilter, setTypeFilter] = useState(ALL);
  const [taskFilter, setTaskFilter] = useState(ALL);

  useEffect(() => {
    if (!focusRequest) return;
    setDirFilter(focusRequest.dir ?? "all");
    setTypeFilter(focusRequest.type ?? ALL);
    setTaskFilter(focusRequest.taskId ?? ALL);
  }, [focusRequest?.nonce]);

  const wNums = useMemo(() => workerNumbers(authorWorkers(snap.workers)), [snap.workers]);
  const open = useMemo(() => new Set(openQuestions(all).map((m) => m.id)), [all]);
  const escalated = useMemo(
    () => new Set(all.filter((m) => m.needsAction && !m.handledAt && m.type !== "run_done" && !open.has(m.id)).map((m) => m.id)),
    [all, open],
  );
  const needs = (m: Message) => Number(open.has(m.id) || escalated.has(m.id));
  const taskLabel = (id?: string) => (id ? (snap.tasks.find((item) => item.id === id)?.title ?? id) : null);

  const outCount = useMemo(() => messages.filter((m) => m.from === "coordinator").length, [messages]);
  const inCount = messages.length - outCount;
  const types = [...new Set(messages.map((m) => m.type))];
  const tasks = [...new Set(messages.map((m) => m.taskId).filter((id): id is string => !!id))];
  const shown = messages.filter(
    (m) =>
      (dirFilter === "all" || (dirFilter === "out" ? m.from === "coordinator" : m.from !== "coordinator")) &&
      (typeFilter === ALL || (typeFilter === NEEDS ? needs(m) > 0 : m.type === typeFilter)) &&
      (taskFilter === ALL || m.taskId === taskFilter),
  );
  const answered = useMemo(() => answeredQuestionIds(all), [all]);
  const filtered = dirFilter !== "all" || typeFilter !== ALL || taskFilter !== ALL;
  const groups = useMemo(() => threadGroups(shown), [shown]);
  const play = (g: ThreadGroup) => {
    const seq =
      g.root.type === "dispatch" && g.root.attemptId
        ? [
            g.root,
            ...all
              .filter((m) => m.attemptId === g.root.attemptId && m.id !== g.root.id)
              .sort((a, b) => (a.createdAt < b.createdAt ? -1 : 1)),
          ]
        : [g.root, ...g.replies];
    onPlay?.({
      key: g.root.id,
      nonce: Date.now(),
      hops: seq.map((m) => messageHop(m, snap, wNums)),
    });
  };
  const card = (m: Message, reply: boolean) => (
    <MessageCard
      key={m.id}
      m={m}
      snap={snap}
      wNums={wNums}
      reply={reply}
      open={open.has(m.id)}
      escalated={escalated.has(m.id)}
      answered={m.type === "question" && answered.has(m.id)}
      taskLabel={taskLabel(m.taskId)}
      wide={bare}
      onSelect={onSelect}
    />
  );

  const body = (
    <>
      {messages.length > 0 && (
        <div className="msg-filters">
          <div className="seg seg--sm" role="group" aria-label={t("msg.dirLabel")}>
            <button
              type="button"
              className={dirFilter === "all" ? "active" : ""}
              aria-pressed={dirFilter === "all"}
              onClick={() => setDirFilter("all")}
            >
              {t("msg.dirAll")} ({messages.length})
            </button>
            <button
              type="button"
              className={dirFilter === "out" ? "active" : ""}
              aria-pressed={dirFilter === "out"}
              onClick={() => setDirFilter("out")}
            >
              {t("msg.dirOut")} ({outCount})
            </button>
            <button
              type="button"
              className={dirFilter === "in" ? "active" : ""}
              aria-pressed={dirFilter === "in"}
              onClick={() => setDirFilter("in")}
            >
              {t("msg.dirIn")} ({inCount})
            </button>
          </div>
          {filters && (
            <>
              <DoodleSelect
                size="sm"
                value={typeFilter}
                onChange={setTypeFilter}
                title={t("msg.filterType")}
                options={[
                  { value: ALL, label: t("msg.allTypes") },
                  { value: NEEDS, label: t("msg.needsYou") },
                  ...types.map((ty) => ({ value: ty, label: `${MSG_LABEL[ty] ?? ty} (${ty})` })),
                ]}
              />
              {tasks.length > 0 && (
                <DoodleSelect
                  size="sm"
                  value={taskFilter}
                  onChange={setTaskFilter}
                  title={t("msg.filterTask")}
                  options={[
                    { value: ALL, label: t("msg.allTasks") },
                    ...tasks.map((id) => ({ value: id, label: taskLabel(id) ?? id })),
                  ]}
                />
              )}
            </>
          )}
          {filtered && (
            <button
              type="button"
              className="btn btn--ghost btn--sm"
              onClick={() => {
                setDirFilter("all");
                setTypeFilter(ALL);
                setTaskFilter(ALL);
              }}
            >
              {t("msg.clearFilters")}
            </button>
          )}
        </div>
      )}
      {groups.length === 0 ? (
        <span className="hint">{filtered ? t("msg.noneFiltered") : t("msg.none")}</span>
      ) : (
        <>
          {onPlay && <p className="hint msg-play-hint">{t("msg.playHint")}</p>}
          <ol className="msg-list">
            {groups.map((g) => {
              const isPlaying = playing === g.root.id;
              return (
                <li
                  key={g.root.id}
                  className={`msg-thread${g.replies.length ? " msg-thread--has-replies" : ""}${isPlaying ? " msg-thread--playing" : ""}`}
                >
                  <div
                    className="msg-thread__hit"
                    role={onPlay ? "button" : undefined}
                    tabIndex={onPlay ? 0 : undefined}
                    aria-pressed={onPlay ? isPlaying : undefined}
                    title={onPlay ? t("msg.playTitle") : undefined}
                    onClick={onPlay ? () => play(g) : undefined}
                    onKeyDown={
                      onPlay
                        ? (e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              play(g);
                            }
                          }
                        : undefined
                    }
                  >
                    {card(g.root, false)}
                    {g.replies.length > 0 && (
                      <ol className="msg-replies" aria-label={t("msg.replies", { n: g.replies.length })}>
                        {g.replies.map((r) => (
                          <li key={r.id}>{card(r, true)}</li>
                        ))}
                      </ol>
                    )}
                  </div>
                  {isPlaying && <span className="msg-thread__playing">▶ {t("msg.playing")}</span>}
                </li>
              );
            })}
          </ol>
        </>
      )}
    </>
  );

  if (bare) {
    return (
      <div className="wb-messages">
        <div className="wb-messages__meta">
          <span>{`${title ?? t("msg.title")} · ${shown.length}/${messages.length}`}</span>
        </div>
        {body}
      </div>
    );
  }

  return (
    <section className="panel-group">
      <div className="panel-group__head">
        <div className="sp-group-title-wrap">
          <span className="panel-group__title">{title ?? t("msg.title")}</span>
          <span className="sp-count">{`${shown.length}/${messages.length}`}</span>
        </div>
      </div>
      {body}
    </section>
  );
}
