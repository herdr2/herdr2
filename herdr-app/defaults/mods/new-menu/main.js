import { View, div, svg, image } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { theme, spaces, panes, knownAgents, run, h2, openOverlay, closeOverlay, props, window, keys, shared, setShared, settings } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

// The last agents each machine reported, shown at once while knownAgents() checks again.
const KNOWN = "new-menu.agents";
const MODE_KEY = "new-menu.agent-mode";
const ACP_PRESETS = new Set(["claude", "codex", "antigravity", "agy", "gemini", "opencode", "copilot", "cursor", "kimi", "qwen", "pi", "hermes", "glm", "zcode", "trae", "dsh-tui", "dsh"]);
const ACP_AGENT_KEY = { antigravity: "agy", "dsh-tui": "dsh" };
const WIN_SHELLS = () => [
  { id: "powershell", label: "PowerShell" },
  { id: "pwsh", label: "PowerShell 7" },
  { id: "git-bash", label: "Git Bash" },
  { id: "cmd", label: "CMD" },
];

function activeFolder(space) {
  if (!space) return null;
  const tabIds = new Set((space.tabs ?? []).map((t) => t.id));
  return panes().find((p) => tabIds.has(p.tab) && p.cwd)?.cwd || space.folder || null;
}

function shortcut(name) {
  const k = keys().find(b => b.section === "global" && b.action === name);
  if (!k?.keys.trim()) return "";
  const mac = window()?.platform === "macos";
  const names = mac ? { ctrl: "⌃", alt: "⌥", shift: "⇧", cmd: "⌘", fn: "fn" } : { ctrl: "Ctrl", alt: "Alt", shift: "Shift", cmd: "Super", fn: "Fn" };
  const alias = { secondary: mac ? "cmd" : "ctrl", super: "cmd", win: "cmd" };
  return k.keys.trim().split(/\s+/).map(stroke => {
    const parts = stroke.split("-");
    let key = parts.pop();
    if (key === "") { key = "-"; parts.pop(); }
    const held = new Set(parts.map(m => alias[m.toLowerCase()] ?? m.toLowerCase()));
    if (/^[A-Z]$/.test(key)) held.add("shift");
    const label = key.length === 1 ? key.toUpperCase() : key[0].toUpperCase() + key.slice(1).toLowerCase();
    return [...Object.keys(names).filter(m => held.has(m)).map(m => names[m]), label].join(mac ? "" : "+");
  }).join(" ");
}

export default class NewMenu extends View {
  init(_props, cx) {
    const machine = spaces().find(s => s.active)?.machine;
    const where = machine ?? "";
    this.agents = shared(KNOWN)?.[where] ?? [];
    this.handle = cx.focus_handle();
    this.handle.focus();
    cx.spawn(async cx => {
      try {
        this.agents = await knownAgents({ on: machine });
        setShared(KNOWN, { ...shared(KNOWN), [where]: this.agents });
      }
      catch { this.failed = true; }
      cx.notify();
    });
  }
  row(id, icon, name, action, hint = "", enabled = true) {
    const t = theme();
    return h_flex().id(id).h(31).px(9).gap(10).rounded(7).items_center().flex_none()
      .text_color(enabled ? t.foreground : t.dim).when(enabled, el => el.hover(s => s.bg(t.selected)))
      .on_click(() => { if (enabled) { closeOverlay(props().overlay); action(); } })
      .child(icon).child(div().flex_1().child(name)).child(div().text_size(11).text_color(t.dim).child(hint));
  }
  launchAgent(a, space, acpMode) {
    closeOverlay(props().overlay);
    if (acpMode && ACP_PRESETS.has(a.key)) {
      const cwd = activeFolder(space);
      const preset = ACP_AGENT_KEY[a.key] ?? a.key;
      const args = { space: space?.id, acp: true, agent: preset };
      if (cwd) args.folder = cwd;
      run("tab.new", args);
      return;
    }
    const runArgv = a.key === "cursor" && Array.isArray(a.run) && a.run[0] === "agent"
      ? ["cursor-agent", ...a.run.slice(1)]
      : a.run;
    run("tab.new", { name: a.name, space: space?.id, argv: runArgv });
  }
  render() {
    const t = theme();
    const space = spaces().find(s => s.active);
    const acpMode = (shared(MODE_KEY) ?? "terminal") === "acp";
    const winInfo = window();
    const isWinLocal = winInfo?.platform === "windows" && !space?.machine;
    const installedWinShells = Array.isArray(winInfo?.shells)
      ? new Set(winInfo.shells.map((s) => String(s).trim().toLowerCase()))
      : null;
    const winShells = isWinLocal
      ? (installedWinShells ? WIN_SHELLS().filter((sh) => installedWinShells.has(sh.id)) : WIN_SHELLS())
      : [];
    const showWinShells = winShells.length > 0;
    const rawShell = String(this.selectedShell ?? (typeof settings === "function" ? settings()?.shell : undefined) ?? "auto").trim().toLowerCase();
    const activeShell = rawShell === "windows-powershell"
      ? "powershell"
      : rawShell === "powershell7"
        ? "pwsh"
        : (rawShell === "gitbash" || rawShell === "bash")
          ? "git-bash"
          : rawShell;
    const activeShellLabel = winShells.find((sh) => sh.id === activeShell)?.label ?? "";
    const installed = this.agents.filter(a => a.installed);
    const choices = this.failed ? installed : [...installed, ...this.agents.filter(a => !a.installed).slice(0, 2)];
    const modeChip = (id, label, active) => div()
      .id(`nm-mode-${id}`)
      .px(6)
      .py(2)
      .rounded(5)
      .text_size(10.5)
      .font_medium()
      .bg(active ? t.selected : t.popover)
      .border_1()
      .border_color(active ? (t.accent ?? t.border) : t.border)
      .text_color(active ? t.foreground : t.dim)
      .cursor_pointer()
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        cx.stop_propagation();
        setShared(MODE_KEY, id);
        cx.notify();
      })
      .child(label);
    return v_flex().min_h(0).w_full().h(Math.min(520, 175 + (showWinShells ? 28 : 0) + Math.max(1, choices.length + (this.failed ? 1 : 0)) * 31)).bg(t.popover).p(5).text_size(12.8)
      .id("new-menu").track_focus(this.handle)
      .on_key_down((e) => {
        if (e.modifiers.control || e.modifiers.platform || e.modifiers.alt) return;
        const agent = /^[1-9]$/.test(e.key) ? installed[Number(e.key) - 1] : null;
        if (!agent) return;
        this.launchAgent(agent, space, acpMode);
      })
      .child(this.row(
        "terminal",
        svg("icons/shell.svg").size(14),
        activeShellLabel ? `${tr("Terminal", "终端")} · ${activeShellLabel}` : tr("Terminal", "终端"),
        () => run("tab.new", { space: space?.id }),
        shortcut("tab.new"),
      ))
      .when(showWinShells, (el) => el.child(
        h_flex()
          .id("nm-win-shells")
          .px(9)
          .pt(2)
          .pb(4)
          .gap(4)
          .flex_wrap()
          .items_center()
          .children(winShells.map((sh) => {
            const active = activeShell === sh.id;
            return div()
              .id(`nm-shell-${sh.id}`)
              .px(6)
              .py(2)
              .rounded(5)
              .bg(active ? t.selected : t.raised)
              .border_1()
              .border_color(active ? (t.accent ?? t.border) : t.border)
              .text_size(10.5)
              .when(active, (el) => el.font_medium())
              .text_color(active ? t.foreground : t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.selected).text_color(t.foreground))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click((_e, cx) => {
                cx.stop_propagation();
                this.selectedShell = sh.id;
                run("app.shell", { shell: sh.id });
                cx.notify();
              })
              .child(sh.label);
          })),
      ))
      .child(
        h_flex()
          .px(9)
          .pt(8)
          .pb(4)
          .items_center()
          .justify_between()
          .child(div().text_size(11).text_color(t.dim).child(tr("Start an agent", "启动智能体")))
          .child(h_flex().gap(4).items_center().child(modeChip("terminal", tr("CLI", "终端"), !acpMode)).child(modeChip("acp", "ACP", acpMode))),
      )
      .child(v_flex().flex_1().min_h(0).overflow_y_scrollbar()
        .children(choices.map((a, i) => {
          const canAcp = ACP_PRESETS.has(a.key);
          const enabled = a.installed && (!acpMode || canAcp);
          const hint = !a.installed ? tr("not installed", "未安装") : (acpMode && !canAcp) ? tr("CLI only", "仅终端") : (i < 9 ? String(i + 1) : "");
          return this.row(
            a.key,
            a.tint ? svg(a.icon).size(14).text_color(t.foreground) : image(a.icon).size(14),
            a.name,
            () => this.launchAgent(a, space, acpMode),
            hint,
            enabled,
          );
        }))
        .when(this.failed || this.agents.length === 0, el => el.child(div().px(9).py(6).text_color(t.dim).child(this.failed ? tr("Could not check installed agents", "无法检查已安装的智能体") : tr("Checking installed agents…", "正在检查已安装的智能体…")))))
      .child(div().h(1).mx(4).my(5).bg(t.border))
      .child(this.row("space", div().w(14).child("+"), tr("New space", "新建工作区"), () => run("space.new"), shortcut("space.new")))
      .child(this.row("worktree", svg("mods/workspace-chrome/branch.svg").size(14), tr("New worktree…", "新建工作树…"), () => openOverlay({ mod: "worktree", kind: "dialog", width: 460, height: 280, props: { space: space?.id, folder: panes().find(p => p.focused)?.cwd, action: "new" } }), "", Boolean(space)))
      .child(this.row("worktree-open", svg("mods/workspace-chrome/branch.svg").size(14), tr("Open worktree\u2026", "打开工作树…"), () => openOverlay({ mod: "worktree", kind: "dialog", width: 560, height: 480, props: { space: space?.id, folder: panes().find(p => p.focused)?.cwd, action: "open" } }), "", Boolean(space)));
  }
}
