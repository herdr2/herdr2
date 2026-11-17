import type { CSSProperties, ReactNode } from "react";

export type WorkbenchRole = "coord" | "task" | "worker" | "reviewer";

export interface FilterOption<T extends string> {
  value: T;
  label: string;
  count?: number;
}

export function StatusBadge({
  token,
  pulse,
  children,
}: {
  token: string;
  pulse?: boolean;
  children: ReactNode;
}) {
  return (
    <span
      className="sbadge"
      style={{ "--st": `var(--st-${token})`, "--st-bg": `var(--st-${token}-bg)` } as CSSProperties}
    >
      <span className={`sdot${pulse ? " sdot--pulse" : ""}`} />
      {children}
    </span>
  );
}

export function Workbench({
  role,
  header,
  master,
  detail,
}: {
  role: WorkbenchRole;
  header: ReactNode;
  master: ReactNode;
  detail: ReactNode;
}) {
  return (
    <section className="wb" data-wb-role={role}>
      {header}
      <div className="wb__grid">
        <aside className="wb__master">{master}</aside>
        <div className="wb__detail">{detail}</div>
      </div>
    </section>
  );
}

export function WorkbenchHeader({
  badge,
  title,
  subtitle,
  actions,
}: {
  badge?: ReactNode;
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="wb__head">
      <div className="wb__head-main">
        <div className="wb__title-row">
          {badge}
          <h2 className="wb__title">{title}</h2>
        </div>
        {subtitle && <div className="wb__subtitle">{subtitle}</div>}
      </div>
      {actions && <div className="wb__actions">{actions}</div>}
    </header>
  );
}

export function WorkbenchFilterBar<T extends string>({
  value,
  options,
  onChange,
  ariaLabel,
}: {
  value: T;
  options: FilterOption<T>[];
  onChange: (next: T) => void;
  ariaLabel: string;
}) {
  return (
    <div className="wb-filters" role="group" aria-label={ariaLabel}>
      {options.map((opt) => (
        <button
          key={opt.value}
          type="button"
          className={`wb-filter${value === opt.value ? " wb-filter--active" : ""}`}
          aria-pressed={value === opt.value}
          onClick={() => onChange(opt.value)}
        >
          <span>{opt.label}</span>
          {opt.count != null && <span className="wb-filter__count">{opt.count}</span>}
        </button>
      ))}
    </div>
  );
}

export function WorkbenchSubtabs<T extends string>({
  value,
  options,
  onChange,
  ariaLabel,
}: {
  value: T;
  options: FilterOption<T>[];
  onChange: (next: T) => void;
  ariaLabel: string;
}) {
  return (
    <div className="wb-subtabs" role="tablist" aria-label={ariaLabel}>
      {options.map((opt) => (
        <button
          key={opt.value}
          type="button"
          role="tab"
          aria-selected={value === opt.value}
          className={`wb-subtab${value === opt.value ? " wb-subtab--active" : ""}`}
          onClick={() => onChange(opt.value)}
        >
          <span>{opt.label}</span>
          {opt.count != null && <span className="wb-subtab__count">{opt.count}</span>}
        </button>
      ))}
    </div>
  );
}

export function WorkbenchEmpty({
  title,
  body,
  action,
}: {
  title: ReactNode;
  body?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="wb-empty">
      <p className="wb-empty__title">{title}</p>
      {body && <p className="wb-empty__body">{body}</p>}
      {action && <div className="wb-empty__action">{action}</div>}
    </div>
  );
}
