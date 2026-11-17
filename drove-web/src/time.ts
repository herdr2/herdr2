/** ISO timestamps, shown in Chinese relative or wall-clock form. */

export function parseTime(s: string | null | undefined): number {
  if (!s) return NaN;
  const iso = /^\d{4}-\d\d-\d\d \d/.test(s) ? `${s.replace(" ", "T")}Z` : s;
  return Date.parse(iso);
}

export function relTime(s: string | null | undefined, now = Date.now()): string {
  const ts = parseTime(s);
  if (!Number.isFinite(ts)) return s ?? "—";
  const sec = Math.round((ts - now) / 1000);
  const fmt = new Intl.RelativeTimeFormat("zh-CN", { numeric: "auto" });
  const a = Math.abs(sec);
  if (a < 60) return fmt.format(sec, "second");
  if (a < 3600) return fmt.format(Math.round(sec / 60), "minute");
  if (a < 86400) return fmt.format(Math.round(sec / 3600), "hour");
  return fmt.format(Math.round(sec / 86400), "day");
}

export function clockTime(s: string | null | undefined): string {
  const ts = parseTime(s);
  if (!Number.isFinite(ts)) return s ?? "—";
  return new Date(ts).toLocaleString("zh-CN", { hour12: false });
}
