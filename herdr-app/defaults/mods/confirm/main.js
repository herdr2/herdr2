// Asks before something destructive, e.g. "Close space?". Opened as a
// dialog with props { title, detail, command: { name, args }, confirmLabel?,
// confirmDanger?, cancelLabel?, secondary?: { label, accent?, command },
// nextStep?: { title, detail, confirmLabel?, confirmDanger?, cancelLabel? } }.
// Enter confirms (runs the command or advances to nextStep), Esc cancels.

import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { run, props, closeOverlay, theme, settings } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

export default class Confirm extends View {
  init(_props, cx) {
    this.handle = cx.focus_handle();
    this.step = 0;
    this.handle.focus();
  }

  yes(cx) {
    const p = props() ?? {};
    if (this.step === 0 && p.nextStep) {
      this.step = 1;
      cx?.notify?.();
      return;
    }
    closeOverlay(p.overlay);
    if (p.command?.name) {
      run(p.command.name, p.command.args ?? {});
    }
  }

  runSecondary() {
    const p = props() ?? {};
    const sec = p.secondary;
    if (!sec?.command?.name) return;
    closeOverlay(p.overlay);
    run(sec.command.name, sec.command.args ?? {});
  }

  cancel(cx) {
    const p = props() ?? {};
    if (this.step > 0) {
      this.step = 0;
      cx?.notify?.();
      return;
    }
    closeOverlay(p.overlay);
  }

  render(cx) {
    const t = theme();
    const p = props() ?? {};
    const cur = this.step > 0 && p.nextStep ? p.nextStep : p;
    const dangerBg = t.danger ?? "#dc2626";
    const dangerFg = "#ffffff";
    const accentBg = t.accent ?? t.solid ?? "#2563eb";
    const accentFg = t.solid_foreground ?? "#ffffff";
    const confirmBg = cur.confirmDanger ? dangerBg : (t.solid ?? accentBg);
    const confirmFg = cur.confirmDanger ? dangerFg : (t.solid_foreground ?? "#ffffff");
    const cancelLabel = cur.cancelLabel ?? (this.step > 0 ? tr("Back", "返回") : tr("Cancel", "取消"));
    const confirmLabel = cur.confirmLabel ?? tr("Confirm", "确认");
    const sec = this.step === 0 ? p.secondary : null;

    const button = (id, label, bg, fg, borderColor, onClick) =>
      h_flex()
        .id(id)
        .h(32)
        .px(14)
        .items_center()
        .rounded(8)
        .border_1()
        .border_color(borderColor)
        .bg(bg)
        .text_color(fg)
        .cursor_pointer()
        .hover((s) => s.opacity(0.88))
        .on_click(onClick)
        .child(label);

    return v_flex()
      .id("confirm")
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "enter") this.yes(cx);
        else if (e.key === "escape") this.cancel(cx);
        else return;
        cx.stop_propagation();
      })
      .size_full()
      .min_h(0)
      .p(18)
      .gap(12)
      .rounded(8)
      .bg(t.popover)
      .text_size(13)
      .child(
        h_flex()
          .items_center()
          .gap(8)
          .child(div().flex_1().min_w(0).text_size(15).font_semibold().text_color(t.foreground).child(cur.title ?? tr("Are you sure?", "确认执行？")))
          .child(div().flex_none().text_size(11).text_color(t.dim).child("Esc")),
      )
      .child(
        div()
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .whitespace_normal()
          .line_height("20px")
          .text_color(t.muted)
          .child(cur.detail ?? ""),
      )
      .child(
        h_flex()
          .justify_end()
          .flex_wrap()
          .gap(8)
          .text_size(12)
          .child(button("no", cancelLabel, t.raised, t.foreground, t.input ?? t.border, (_e, cx) => this.cancel(cx)))
          .when(Boolean(sec?.label), (el) =>
            el.child(
              button(
                "secondary",
                sec.label,
                sec.accent ? accentBg : t.raised,
                sec.accent ? accentFg : t.foreground,
                sec.accent ? accentBg : (t.accent ?? t.input ?? t.border),
                () => this.runSecondary(),
              ),
            ),
          )
          .child(button("yes", confirmLabel, confirmBg, confirmFg, confirmBg, (_e, cx) => this.yes(cx))),
      );
  }
}
