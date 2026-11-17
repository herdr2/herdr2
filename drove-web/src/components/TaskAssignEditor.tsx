import { useEffect, useMemo, useState } from "react";
import { assignTask } from "../api";
import { agentModel } from "../format";
import { useT } from "../i18n";
import { buildRoleSelectOptions, resolveRoleMeta, roleDisplayName } from "../roles";
import { ASSIGNABLE, EFFORTS, type Attempt, type Effort, type Profile, type Task } from "../types";
import { DoodleSelect } from "./DoodleSelect";

const FALLBACK_AGENTS = ["antigravity", "codex", "claude", "cursor", "dsh"];

export function assignSourceLabel(source: string, t: (key: string) => string): string {
  return source === "user"
    ? t("panel.assignSourceUser")
    : source === "retry"
      ? t("panel.assignSourceRetry")
      : t("panel.assignSourcePlan");
}

export function TaskAssignEditor({
  task,
  attempts,
  profile,
  onRefresh,
  bare = false,
}: {
  task: Task;
  attempts: Attempt[];
  profile: Profile | null;
  onRefresh: () => void;
  bare?: boolean;
}) {
  const { t } = useT();
  const editable = ASSIGNABLE.has(task.status) && attempts.length === 0 && task.kind !== "exec";
  const agents = profile?.agents.map((a) => a.id) ?? FALLBACK_AGENTS;
  const [role, setRole] = useState(task.assign?.role ?? "");
  const [agent, setAgent] = useState(task.assign?.agent ?? "");
  const [model, setModel] = useState(task.assign?.model ?? "");
  const [effort, setEffort] = useState<Effort | "">(task.assign?.effort ?? "");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    setRole(task.assign?.role ?? "");
    setAgent(task.assign?.agent ?? "");
    setModel(task.assign?.model ?? "");
    setEffort(task.assign?.effort ?? "");
    setErr(null);
  }, [task.id, task.assign?.role, task.assign?.agent, task.assign?.model, task.assign?.effort, task.updatedAt]);

  const roleOptions = useMemo(() => buildRoleSelectOptions(profile, role), [profile, role]);
  const selectedRoleMeta = useMemo(() => resolveRoleMeta(role || undefined, { profile }), [profile, role]);

  const modelOptions = useMemo(() => {
    const fromProfile = profile?.agents.find((a) => a.id === agent)?.models.map((m) => m.id) ?? [];
    const extra = model && !fromProfile.includes(model) ? [model] : [];
    return ["", ...extra, ...fromProfile];
  }, [profile, agent, model]);

  async function save() {
    setBusy(true);
    setErr(null);
    try {
      const patch: { agent?: string; model?: string; effort?: Effort; role?: string; worker?: string; fork?: string } = {};
      if (role) patch.role = role;
      if (agent) patch.agent = agent;
      if (model) patch.model = model;
      if (effort) patch.effort = effort;
      if (task.assign?.worker) patch.worker = task.assign.worker;
      if (task.assign?.fork) patch.fork = task.assign.fork;
      await assignTask(task.id, patch);
      onRefresh();
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  if (!editable) {
    const readRoleMeta = resolveRoleMeta(task.assign?.role, { profile });
    return (
      <div className="panel-field">
        <span className="panel-field__key">{t("panel.assign")}</span>
        <div className="panel-field__val">
          {task.kind === "exec"
            ? t("wb.execNoAgent")
            : (
              <>
                {task.assign?.role && (
                  <>
                    <span className={`role-badge ${readRoleMeta.badgeClass}`} style={{ marginRight: 6 }}>
                      {roleDisplayName(readRoleMeta, t)}
                    </span>
                    <span className={`access-pill access-pill--${readRoleMeta.access}`} style={{ marginRight: 6 }}>
                      {t(readRoleMeta.accessKey)}
                    </span>
                  </>
                )}
                {agentModel(task.assign?.agent, task.assign?.model, t("task.unassigned"))}
                {task.assign?.effort ? ` · ${task.assign.effort}` : ""}
                {task.assign?.source ? ` · ${assignSourceLabel(task.assign.source, t)}` : ""}
              </>
            )}
        </div>
      </div>
    );
  }

  const form = (
    <>
      <div className="panel-field">
        <span className="panel-field__key">{t("assign.role")}</span>
        <div className="panel-field__val">
          <DoodleSelect
            value={role}
            onChange={(v) => setRole(v)}
            options={roleOptions.map((opt) => ({
              value: opt.value,
              label: opt.builtIn ? t(opt.labelKey) : opt.fallbackLabel,
            }))}
          />
          <div style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap", marginTop: 4 }}>
            <span className={`role-badge ${selectedRoleMeta.badgeClass}`}>
              {roleDisplayName(selectedRoleMeta, t)}
            </span>
            <span className={`access-pill access-pill--${selectedRoleMeta.access}`}>
              {t(selectedRoleMeta.accessKey)}
            </span>
            <span className="hint" style={{ fontSize: 11 }}>
              {t("assign.roleDefaultHint", {
                agents: selectedRoleMeta.defaultAgents.slice(0, 2).join(" → ") || "—",
                effort: selectedRoleMeta.defaultEffort,
              })}
            </span>
          </div>
        </div>
      </div>
      <div className="panel-field">
        <span className="panel-field__key">{t("panel.agent")}</span>
        <div className="panel-field__val">
          <DoodleSelect
            value={agent}
            onChange={(v) => {
              setAgent(v);
              const models = profile?.agents.find((a) => a.id === v)?.models ?? [];
              const def = models.find((m) => m.default)?.id ?? models[0]?.id ?? "";
              setModel(def);
            }}
            options={["", ...agents.filter((id, i, all) => all.indexOf(id) === i)].map((id) => ({
              value: id,
              label: id || t("panel.modelNone"),
            }))}
          />
        </div>
      </div>
      <div className="panel-field">
        <span className="panel-field__key">{t("panel.model")}</span>
        <div className="panel-field__val">
          <DoodleSelect
            value={model}
            onChange={setModel}
            options={modelOptions.map((id) => ({ value: id, label: id || t("panel.modelNone") }))}
          />
        </div>
      </div>
      <div className="panel-field">
        <span className="panel-field__key">{t("panel.effort")}</span>
        <div className="panel-field__val">
          <DoodleSelect
            value={effort}
            onChange={(v) => setEffort(v as Effort | "")}
            options={[{ value: "", label: t("panel.effortNone") }, ...EFFORTS.map((e) => ({ value: e, label: e }))]}
          />
        </div>
      </div>
      {task.assign?.reason && (
        <div className="panel-field">
          <span className="panel-field__key">{t("panel.reason")}</span>
          <div className="panel-field__val">{task.assign.reason}</div>
        </div>
      )}
      {task.assign?.source && <span className="hint">{assignSourceLabel(task.assign.source, t)}</span>}
      {err && <p className="hint" style={{ color: "var(--danger)" }}>{err}</p>}
      <button type="button" className="btn btn--primary" disabled={busy} onClick={save}>
        {busy ? t("panel.assignSaving") : t("panel.assignSave")}
      </button>
    </>
  );

  if (bare) {
    return (
      <div className="wb-assign">
        <p className="hint">{t("panel.assignHint")}</p>
        {form}
      </div>
    );
  }

  return (
    <section className="panel-group">
      <div className="panel-group__head">
        <span className="panel-group__title">{t("panel.assign")}</span>
      </div>
      <span className="hint">{t("panel.assignHint")}</span>
      {form}
    </section>
  );
}
