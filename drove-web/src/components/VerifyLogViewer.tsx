import { useEffect, useState } from "react";
import { fetchVerifyLog } from "../api";
import { useT } from "../i18n";

const LOG_PAGE_LIMIT = 65_536;

export function VerifyLogViewer({
  runId,
  artifactId,
  title,
  defaultOpen = false,
}: {
  runId: string;
  artifactId: string;
  title?: string;
  defaultOpen?: boolean;
}) {
  const { t } = useT();
  const [open, setOpen] = useState(defaultOpen);
  const [text, setText] = useState("");
  const [nextOffset, setNextOffset] = useState(0);
  const [totalBytes, setTotalBytes] = useState<number | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setOpen(defaultOpen);
    setText("");
    setNextOffset(0);
    setTotalBytes(null);
    setTruncated(false);
    setError(null);
  }, [runId, artifactId, defaultOpen]);

  async function loadChunk(offset: number, replace: boolean) {
    setLoading(true);
    setError(null);
    try {
      const slice = await fetchVerifyLog(runId, artifactId, { offset, limit: LOG_PAGE_LIMIT });
      setText((prev) => (replace ? slice.content : prev + slice.content));
      setTotalBytes(slice.totalBytes);
      // Backend slices via JS string.slice(offset, offset + limit), so advance by UTF-16 code units.
      setNextOffset(slice.offset + slice.content.length);
      setTruncated(slice.truncated && slice.content.length > 0);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (!open || text !== "" || totalBytes !== null || loading) return;
    void loadChunk(0, true);
  }, [open, text, totalBytes, loading]);

  return (
    <div className="wb-log">
      <div className="wb-log__head">
        <div className="wb-log__meta">
          <span className="wb-log__title">{title ?? t("wb.verifyLog")}</span>
          <span className="mono wb-log__artifact">{artifactId}</span>
          {totalBytes != null && (
            <span className="hint">{t("wb.verifyLogBytes", { loaded: nextOffset, bytes: totalBytes })}</span>
          )}
        </div>
        <div className="wb-log__actions">
          {open && (
            <button
              type="button"
              className="btn btn--ghost btn--sm"
              disabled={loading}
              onClick={() => void loadChunk(0, true)}
            >
              {t("wb.refresh")}
            </button>
          )}
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => setOpen((v) => !v)}
          >
            {open ? t("wb.verifyLogHide") : t("wb.verifyLogShow")}
          </button>
        </div>
      </div>
      {open && (
        <div className="wb-log__body">
          {error && <p className="hint" style={{ color: "var(--danger)" }}>{error}</p>}
          {loading && !text ? (
            <p className="hint">{t("panel.loading")}</p>
          ) : text ? (
            <pre className="wb-log__pre">{text}</pre>
          ) : (
            <p className="hint">{t("wb.verifyLogEmpty")}</p>
          )}
          {truncated && (
            <button
              type="button"
              className="btn btn--ghost btn--sm"
              disabled={loading}
              onClick={() => void loadChunk(nextOffset, false)}
            >
              {loading ? t("panel.loading") : t("wb.loadMore")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
