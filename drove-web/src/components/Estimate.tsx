import { withEstimate } from "../format";
import { useT } from "../i18n";

/** Show ≈ and a tooltip when a token or cost figure was estimated. */
export function Estimate({ text, estimated }: { text: string; estimated?: boolean }) {
  const { t } = useT();
  if (!estimated) return text;
  return <span title={t("usage.estimated")}>{withEstimate(text, true)}</span>;
}
