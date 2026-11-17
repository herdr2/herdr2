import { useT } from "../i18n";

/** Review policy stays visible independently of execution status and review verdict. */
export function ReviewRequirement({ required, count }: { required: boolean; count?: number }) {
  const { t } = useT();
  return (
    <span
      className={`review-requirement review-requirement--${required ? "required" : "none"}`}
      title={t(required ? "review.requiredHint" : "review.notRequiredHint")}
    >
      {t(required ? "review.required" : "review.notRequired")}
      {count != null && <strong className="review-requirement__count">{count}</strong>}
    </span>
  );
}
