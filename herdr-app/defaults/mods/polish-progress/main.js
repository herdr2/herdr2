// Lightweight floating progress card for in-place CLI prompt polishing.
// Driven by shared("polish.state") from AppWindow; hot-reloads live with zero Rust recompile.

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { shared, run, theme, settings } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

const CYCLE_MS = 2600;
const ENTER_MS = 240;
const APPLY_MS = 220;
const CARD_W = 368;
const TRACK_W = CARD_W - 28;
const PURPLE = "#a855f7";
const PURPLE_TRACK = "#a855f724";

export default class PolishProgress extends View {
  init(_props, cx) {
    this.mountedAt = Date.now();
    this.lastId = null;
    this.stageAt = this.mountedAt;
    this.lastStage = null;
    cx?.timer?.every?.(24, (tcx) => {
      if (shared("polish.state")) tcx?.notify?.();
    });
  }

  render() {
    const state = shared("polish.state");
    if (!state || typeof state !== "object") return div();

    const now = Date.now();
    if (this.lastId !== state.id) {
      this.lastId = state.id;
      this.mountedAt = typeof state.startedAtMs === "number" ? state.startedAtMs : now;
      this.lastStage = state.stage ?? "polishing";
      this.stageAt = typeof state.stageChangedAtMs === "number" ? state.stageChangedAtMs : this.mountedAt;
    } else if (this.lastStage !== state.stage) {
      this.lastStage = state.stage;
      this.stageAt = typeof state.stageChangedAtMs === "number" ? state.stageChangedAtMs : now;
    }

    const t = theme();
    const applying = state.stage === "applying";
    const elapsedMs = Math.max(0, now - this.mountedAt);
    const elapsedStr = `${(elapsedMs / 1000).toFixed(1)}s`;
    const title = applying
      ? tr("Writing to input…", "正在写入输入框…")
      : tr("Polishing prompt…", "正在润色输入…");

    // Smooth 240ms cubic ease-out entrance (fade + subtle 10px upward glide).
    const enterT = Math.min(1, Math.max(0, elapsedMs / ENTER_MS));
    const enterEase = 1 - Math.pow(1 - enterT, 3);
    const slidePx = 10 * (1 - enterEase);

    // Calm 2.6s cosine ease-in-out glide during polishing; smooth fill during applying.
    let barLeft = 0;
    let barW = TRACK_W * 0.3;
    if (applying) {
      const applyT = Math.min(1, Math.max(0, (now - this.stageAt) / APPLY_MS));
      const applyEase = 1 - Math.pow(1 - applyT, 2);
      barLeft = 0;
      barW = TRACK_W * (0.68 + 0.32 * applyEase);
    } else {
      const phase = (elapsedMs % CYCLE_MS) / CYCLE_MS;
      const u = phase < 0.5 ? phase * 2 : (1 - phase) * 2;
      const eased = 0.5 - 0.5 * Math.cos(Math.PI * u);
      const midBoost = Math.sin(Math.PI * u);
      barW = TRACK_W * (0.26 + 0.09 * midBoost);
      barLeft = (TRACK_W - barW) * eased;
    }

    const dotOpacity = applying
      ? 1
      : 0.48 + 0.52 * (0.5 - 0.5 * Math.cos(Math.PI * 2 * ((elapsedMs % CYCLE_MS) / CYCLE_MS)));

    const lineCount = Number(state.lineCount) || 1;
    const lineBadge = lineCount > 1 ? tr(`${lineCount} lines`, `共 ${lineCount} 行`) : "";
    const preview = String(state.preview ?? "");

    return div()
      .id("polish-progress-host")
      .w(CARD_W + 20)
      .px(10)
      .pt(Math.round(slidePx * 10) / 10 + 6)
      .pb(Math.round((10 - slidePx) * 10) / 10 + 8)
      .child(
        v_flex()
          .id("polish-progress-card")
          .w(CARD_W)
          .px(14)
          .py(11)
          .gap(8)
          .rounded(12)
          .bg(t.popover)
          .border_1()
          .border_color(t.border)
          .shadow_lg()
          .opacity(enterEase)
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .gap(8)
              .child(
                h_flex()
                  .items_center()
                  .gap(7)
                  .min_w(0)
                  .child(
                    div()
                      .w(7)
                      .h(7)
                      .rounded(99)
                      .bg(PURPLE)
                      .opacity(dotOpacity)
                      .flex_none(),
                  )
                  .child(
                    div()
                      .text_size(12.5)
                      .font_medium()
                      .text_color(t.foreground)
                      .child(title),
                  )
                  .when(Boolean(lineBadge), (el) =>
                    el.child(
                      div()
                        .px(5)
                        .py(1)
                        .rounded(4)
                        .bg(t.surface ?? t.raised)
                        .text_size(10.5)
                        .text_color(t.muted)
                        .child(lineBadge),
                    ),
                  ),
              )
              .child(
                h_flex()
                  .items_center()
                  .gap(8)
                  .flex_none()
                  .child(
                    div()
                      .text_size(11)
                      .text_color(t.dim)
                      .child(elapsedStr),
                  )
                  .when(!applying, (el) =>
                    el.child(
                      div()
                        .id("polish-progress-cancel")
                        .px(7)
                        .py(2)
                        .rounded(5)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.surface ?? t.raised)
                        .text_size(11)
                        .text_color(t.muted)
                        .cursor_pointer()
                        .hover((s) => s.bg(t.hover).text_color(t.foreground))
                        .tooltip(tr("Cancel polish (Esc)", "取消润色 (Esc)"))
                        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                        .on_click((_e, cx) => {
                          cx.stop_propagation();
                          run("prompt.polish_cancel");
                        })
                        .child(tr("Cancel", "取消")),
                    ),
                  ),
              ),
          )
          .child(
            div()
              .w(TRACK_W)
              .h(3)
              .rounded(2)
              .bg(PURPLE_TRACK)
              .overflow_hidden()
              .relative()
              .child(
                div()
                  .absolute()
                  .top(0)
                  .bottom(0)
                  .left(Math.round(barLeft * 10) / 10)
                  .w(Math.round(barW * 10) / 10)
                  .rounded(2)
                  .bg(PURPLE),
              ),
          )
          .when(Boolean(preview), (el) =>
            el.child(
              div()
                .text_size(11.5)
                .text_color(t.muted)
                .truncate()
                .child(preview),
            ),
          ),
      );
  }
}
