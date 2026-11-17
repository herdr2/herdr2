import { fmtCost, fmtMs, fmtTokens, statsSuccess } from "../format";
import { useT } from "../i18n";
import { BUILT_IN_ROLE_IDS, resolveRoleMeta, roleDisplayName } from "../roles";
import type { ModelAvailability, Profile, QuotaCard, QuotaWindow, StatsTable } from "../types";
import { Estimate } from "./Estimate";

function formatPercent(remaining: number): string {
  const rounded = Math.round(remaining * 10) / 10;
  return Number.isInteger(rounded) ? `${rounded}%` : `${rounded.toFixed(1)}%`;
}

function formatResetTime(iso?: string): string {
  if (!iso) return "";
  const ts = Date.parse(iso);
  if (!Number.isFinite(ts)) return iso;
  const d = new Date(ts);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function windowTone(remaining: number, unlimited?: boolean): "ok" | "warn" | "bad" {
  if (unlimited) return "ok";
  if (remaining <= 0) return "bad";
  if (remaining <= 20) return "warn";
  return "ok";
}

function resolveCardStatus(card: QuotaCard, windows: QuotaWindow[]): "ok" | "exhausted" | "error" {
  if (card.error) return "error";
  if (windows.length > 0 && windows.every((w) => !w.unlimited && w.remaining <= 0)) return "exhausted";
  return "ok";
}

function QuotaOverview({
  quotas,
  quotaError,
  magpieEnabled = false,
}: {
  quotas: QuotaCard[];
  quotaError?: string | null;
  magpieEnabled?: boolean;
}) {
  const { t } = useT();
  if (!magpieEnabled && quotas.length === 0 && !quotaError) return null;

  return (
    <section className="agents-quotas">
      <div className="agents-quotas__head">
        <h2>{t("agents.quotas")}</h2>
      </div>
      {quotaError && (
        <div className="agents-quotas__warn">
          {t("agents.quotasError", { error: quotaError })}
        </div>
      )}
      {magpieEnabled && quotas.length === 0 && !quotaError && (
        <p className="hint">{t("agents.quotasEmpty")}</p>
      )}
      {quotas.length > 0 && (
        <div className="agents-quotas__grid">
          {quotas.map((card, idx) => {
            const numericWindows = (card.windows ?? []).filter((w) => !w.text);
            const status = resolveCardStatus(card, numericWindows);
            const statusClass = status === "ok" ? "agents-status-pill--ok" : "agents-status-pill--bad";
            const statusLabel =
              status === "ok"
                ? t("agents.quotaStatusOk")
                : status === "error"
                  ? t("agents.quotaStatusError")
                  : t("agents.quotaStatusExhausted");
            return (
              <div key={`${card.provider}-${card.user ?? idx}`} className={`agents-quota-card agents-quota-card--${status}`}>
                <div className="agents-quota-card__top">
                  <div>
                    <div className="agents-quota-card__title">
                      <span className="agents-quota-card__provider">{card.name || card.provider}</span>
                      {card.plan && <span className="agents-quota-card__plan">{card.plan}</span>}
                    </div>
                    {card.user && <div className="agents-quota-card__user mono">{card.user}</div>}
                  </div>
                  <span className={`agents-status-pill ${statusClass}`}>{statusLabel}</span>
                </div>
                {card.error && (
                  <div className="agents-model-avail__detail--bad" style={{ fontSize: 11 }}>
                    {card.error}
                  </div>
                )}
                {card.balance && (
                  <div className="agents-quota-card__balance">
                    <span className="agents-quota-card__balance-label">
                      {card.kind === "balance" ? t("agents.quotaBalance") : t("agents.quotaCredits")}
                    </span>
                    <span className="agents-quota-card__balance-val mono">{card.balance}</span>
                  </div>
                )}
                {numericWindows.length > 0 && (
                  <div className="agents-quota-card__windows">
                    {numericWindows.map((span, wIdx) => {
                      const tone = windowTone(span.remaining, span.unlimited);
                      const pct = Math.max(0, Math.min(100, span.unlimited ? 100 : span.remaining));
                      const resetText = formatResetTime(span.resetsAt);
                      return (
                        <div key={`${span.pool ?? ""}-${span.name}-${wIdx}`} className="agents-quota-win">
                          <div className="agents-quota-win__row">
                            <span className="agents-quota-win__name">
                              <span>{span.name}</span>
                              {span.pool && <span className="agents-quota-win__pool">{span.pool}</span>}
                            </span>
                            <span className={`agents-quota-win__pct agents-quota-win__pct--${tone}`}>
                              {span.unlimited ? t("agents.quotaStatusUnlimited") : formatPercent(span.remaining)}
                            </span>
                          </div>
                          <div className="agents-quota-win__bar">
                            <div className={`agents-quota-win__fill agents-quota-win__fill--${tone}`} style={{ width: `${pct}%` }} />
                          </div>
                          {resetText && (
                            <div className="agents-quota-win__reset mono">
                              {t("agents.quotaResets", { time: resetText })}
                            </div>
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

function RoleRoutingMatrix({ profile }: { profile: Profile | null }) {
  const { t } = useT();
  const roleIds = [...new Set<string>([...BUILT_IN_ROLE_IDS, ...Object.keys(profile?.roles ?? {})])];
  return (
    <section className="agents-roles" style={{ marginBottom: 24 }}>
      <h2 style={{ marginBottom: 6 }}>{t("agents.rolesTitle")}</h2>
      <p className="hint" style={{ marginBottom: 10 }}>{t("agents.rolesHint")}</p>
      <table>
        <thead>
          <tr>
            <th>{t("agents.roleCol.role")}</th>
            <th>{t("agents.roleCol.access")}</th>
            <th>{t("agents.roleCol.effort")}</th>
            <th>{t("agents.roleCol.agents")}</th>
            <th>{t("agents.roleCol.tags")}</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>
              <span className="role-badge role-badge--coord">{t("role.coordinator")}</span>
              <span className="mono hint" style={{ marginLeft: 8 }}>architect</span>
            </td>
            <td><span className="access-pill">Host Singleton</span></td>
            <td className="mono">—</td>
            <td>{t("agents.architectRow")}</td>
            <td className="hint">planning · user Q&A · arbitration</td>
          </tr>
          {roleIds.map((id) => {
            const meta = resolveRoleMeta(id, { profile });
            return (
              <tr key={id}>
                <td>
                  <span className={`role-badge ${meta.badgeClass}`}>{roleDisplayName(meta, t)}</span>
                  <span className="mono hint" style={{ marginLeft: 8 }}>{meta.id}</span>
                </td>
                <td>
                  <span className={`access-pill access-pill--${meta.access}`}>{t(meta.accessKey)}</span>
                </td>
                <td className="mono">{meta.defaultEffort}</td>
                <td className="mono" style={{ fontSize: 12 }}>
                  {meta.defaultAgents.length > 0 ? meta.defaultAgents.join(" → ") : "—"}
                </td>
                <td style={{ fontSize: 12, color: "var(--text-2)" }}>
                  {meta.tags.join("、") || "—"}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </section>
  );
}

export function AgentsView({
  profile,
  stats,
  magpieEnabled = false,
  quotas = [],
  quotaError,
  error,
  onRefresh,
}: {
  profile: Profile | null;
  stats: StatsTable;
  availability?: Record<string, ModelAvailability>;
  magpieEnabled?: boolean;
  quotas?: QuotaCard[];
  quotaError?: string | null;
  error?: string | null;
  onRefresh?: () => void;
}) {
  const { t, lang } = useT();
  const rows = Object.entries(stats).sort(([a], [b]) => a.localeCompare(b));

  return (
    <div className="agents">
      {error && (
        <p className="hint" style={{ color: "var(--danger)", marginBottom: 10 }}>
          {error}
        </p>
      )}

      <div className="agents__header">
        <div className="agents__title-row">
          <h2>{t("agents.profile")}</h2>
          {profile?.updatedAt && (
            <span className="agents__updated mono">
              {t("agents.updatedAt", { time: profile.updatedAt.replace("T", " ").replace(/\.\d+Z$/, "Z") })}
            </span>
          )}
        </div>
        <div className="agents__actions">
          <span className="hint" style={{ marginRight: 10 }}>
            {lang === "zh"
              ? "角色编排、轻量 AI 助手与 Magpie 额度开关请在 Herdr2 App → 设置 → 编队 (Drove) 中配置。"
              : "Configure Roles, AI Assistant & Magpie Quota in Herdr2 App → Settings → Drove."}
          </span>
          {onRefresh && (
            <button className="btn btn--ghost" onClick={onRefresh}>
              {t("agents.refresh")}
            </button>
          )}
        </div>
      </div>

      <QuotaOverview quotas={quotas} quotaError={quotaError} magpieEnabled={magpieEnabled} />
      <RoleRoutingMatrix profile={profile} />

      <h2>{t("agents.stats")}</h2>
      {rows.length === 0 ? (
        <p className="hint">{t("agents.none")}</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>{t("agents.key")}</th>
              <th>{t("agents.n")}</th>
              <th>{t("agents.success")}</th>
              <th>{t("agents.avgTokens")}</th>
              <th>{t("agents.avgCost")}</th>
              <th>{t("agents.avgDuration")}</th>
              <th>{t("agents.retry")}</th>
              <th>{lang === "zh" ? "无报告" : "No Done"}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map(([key, row]) => {
              const success = statsSuccess(row);
              const count = Number.isInteger(success.count) ? String(success.count) : success.count.toFixed(1);
              return (
                <tr key={key}>
                  <td className="mono">{key}</td>
                  <td>{row.n}</td>
                  <td>{count}/{row.n}{row.n ? ` (${success.pct}%)` : ""}</td>
                  <td><Estimate text={fmtTokens(row.avgTokens)} estimated={row.estimated?.tokens} /></td>
                  <td><Estimate text={fmtCost(row.avgCost)} estimated={row.estimated?.cost} /></td>
                  <td>{fmtMs(row.avgDurationMs)}</td>
                  <td>{Math.round(row.retryRate * 100)}%</td>
                  <td>{Math.round((row.noDoneRate ?? 0) * 100)}%</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}
