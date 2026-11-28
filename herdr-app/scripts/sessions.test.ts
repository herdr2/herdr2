import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";

class Element {
  nodes: any[] = [];
  calls: Record<string, unknown[]> = {};
  constructor() {
    const proxy: Element = new Proxy(this, {
      get: (target, key: string) =>
        key in target
          ? Reflect.get(target, key)
          : (...args: unknown[]) => {
              if (key === "child") {
                if (args[0] !== null && args[0] !== undefined) target.nodes.push(args[0]);
              } else if (key === "children") {
                for (const c of (args[0] as any[]) ?? []) {
                  if (c !== null && c !== undefined) target.nodes.push(c);
                }
              } else if (key === "when") {
                if (args[0]) (args[1] as (el: Element) => void)(proxy);
              } else {
                target.calls[key] = args;
                if (typeof args[0] === "string") target.calls[`${key}:${args[0]}`] = args;
              }
              return proxy;
            },
    });
    return proxy;
  }
}

const element = () => new Element();

function makeInputState(opts?: { placeholder?: string }) {
  let val = "";
  const handlers: Record<string, Array<(e: any, cx: any) => void>> = {};
  return {
    placeholder: opts?.placeholder ?? "",
    value: () => val,
    set_value(next: string) {
      val = String(next ?? "");
      for (const fn of handlers.change ?? []) fn({ value: val }, { notify() {} });
    },
    on(ev: string, fn: (e: any, cx: any) => void) {
      (handlers[ev] ??= []).push(fn);
    },
    emit(ev: string, e: any, cx: any) {
      for (const fn of handlers[ev] ?? []) fn(e, cx);
    },
  };
}

function findById(node: any, id: string): any {
  if (!node || typeof node !== "object") return null;
  if (node.calls?.id?.[0] === id) return node;
  for (const ch of node.nodes ?? []) {
    const hit = findById(ch, id);
    if (hit) return hit;
  }
  return null;
}

function collectTexts(node: any): string[] {
  if (!node) return [];
  if (typeof node === "string") return [node];
  const out: string[] = [];
  for (const ch of node.nodes ?? []) {
    if (typeof ch === "string") out.push(ch);
    else out.push(...collectTexts(ch));
  }
  return out;
}

const sessionsSource = await readFile(new URL("../defaults/mods/sessions/main.js", import.meta.url), "utf8");
const menuSource = await readFile(new URL("../defaults/mods/menu/main.js", import.meta.url), "utf8");

function loadSessionsMod(hostValues: Record<string, unknown>) {
  const transformed = sessionsSource
    .replace(/^import .*;\n/gm, "")
    .replace(/^export function /gm, "function ")
    .replace("export default class Sessions", "return class Sessions");
  return new Function(...Object.keys(hostValues), transformed)(...Object.values(hostValues));
}

test("sessions panel supports resizable vertical split, folded-by-default thoughts & tool calls, and non-overlapping hover card vs right-click menu", async () => {
  const h2Calls: string[][] = [];
  const overlays: any[] = [];
  const localStore: Record<string, string> = {};
  let lang = "zh-CN";

  const mockListResponse = {
    agent: "claude",
    agent_name: "Claude Code",
    cwd: "/home/dev/project-alpha",
    sessions: [
      {
        session_id: "sess-cur-1",
        agent: "claude",
        title: "重构认证中间件并补充单测",
        preview: "重构认证中间件并补充单测",
        turns: 3,
        created_ms: 1700000000000,
        updated_ms: 1700000050000,
        model: "claude-opus-4-7",
        git_branch: "feat/auth",
        current: true,
        resume_command: "claude --dangerously-skip-permissions --resume sess-cur-1",
      },
      {
        session_id: "sess-prev-2",
        agent: "claude",
        title: "排查数据库连接池泄漏问题",
        preview: "排查数据库连接池泄漏问题",
        turns: 5,
        created_ms: 1699990000000,
        updated_ms: 1699995000000,
        model: "claude-sonnet-4-6",
        git_branch: "fix/db-pool",
        current: false,
        resume_command: "claude --dangerously-skip-permissions --resume sess-prev-2",
      },
    ],
  };

  const mockTranscripts: Record<string, any> = {
    "sess-cur-1": {
      session_id: "sess-cur-1",
      agent: "claude",
      agent_name: "Claude Code",
      title: "重构认证中间件并补充单测",
      model: "claude-opus-4-7",
      updated_ms: 1700000050000,
      resume_command: "claude --dangerously-skip-permissions --resume sess-cur-1",
      entries: [
        { kind: "user", turn: 1, text: "请重构 auth.rs 中的 token 校验逻辑", ts_ms: 1700000010000 },
        { kind: "thought", turn: 1, text: "先阅读 auth.rs 了解现有签名校验流程", ts_ms: 1700000011000 },
        { kind: "tool_call", turn: 1, title: "Read src/auth.rs", tool_kind: "read", status: "completed", ts_ms: 1700000012000 },
        { kind: "assistant", turn: 1, text: "已完成 token 校验重构并通过单元测试。", ts_ms: 1700000020000 },
      ],
    },
    "sess-prev-2": {
      session_id: "sess-prev-2",
      agent: "claude",
      agent_name: "Claude Code",
      title: "排查数据库连接池泄漏问题",
      model: "claude-sonnet-4-6",
      updated_ms: 1699995000000,
      resume_command: "claude --dangerously-skip-permissions --resume sess-prev-2",
      entries: [
        { kind: "user", turn: 1, text: "定位连接池为什么在超时后没有释放连接", ts_ms: 1699990010000 },
        { kind: "assistant", turn: 1, text: "发现是在超时分支提前 return 跳过了 guard drop，已修复。", ts_ms: 1699990030000 },
      ],
    },
  };

  const hostValues = {
    View: class {},
    div: element,
    svg: element,
    v_flex: element,
    h_flex: element,
    v_resizable: (id: string) => (element() as any).id(id),
    resizable_panel: () => element(),
    InputState: { new: makeInputState },
    Input: { new: () => element() },
    Scrollbar: { vertical: () => element() },
    localStorage: {
      getItem: (k: string) => localStore[k] ?? null,
      setItem: (k: string, v: string) => { localStore[k] = v; },
    },
    spaces: () => [{ id: "s1", active: true, folder: "/home/dev/project-alpha", machine: null, tabs: [{ id: "tab1", active: true }] }],
    panes: () => [
      {
        pane: "p1",
        tab: "tab1",
        terminal: "t1",
        cwd: "/home/dev/project-alpha",
        focused: true,
        program: { key: "claude", name: "Claude Code", agent: true, shell: false },
      },
    ],
    focused: () => ({ space: "s1", tab: "tab1", pane: "p1", terminal: "t1", cwd: "/home/dev/project-alpha" }),
    terminals: () => [
      {
        address: "t1",
        cwd: "/home/dev/project-alpha",
        turn: 3,
        saved_cli: { kind: "claude", session_id: "sess-cur-1" },
        program: { key: "claude", name: "Claude Code", agent: true, shell: false },
      },
    ],
    theme: () => ({
      background: "#111114",
      raised: "#18181c",
      popover: "#1c1c22",
      surface: "#151519",
      message: "#1f2430",
      selected: "#272730",
      hover: "#202028",
      border: "#2e2e38",
      foreground: "#f4f4f5",
      muted: "#a1a1aa",
      dim: "#71717a",
      accent: "#3b82f6",
      ansi2: "#22c55e",
    }),
    settings: () => ({ language: lang, zoom: 1 }),
    checking: () => false,
    openOverlay: (spec: any) => {
      overlays.push(spec);
      return "ov-session";
    },
    h2: async (args: string[]) => {
      h2Calls.push(args);
      if (args[0] === "session" && args[1] === "ls") {
        return { code: 0, stdout: JSON.stringify(mockListResponse), stderr: "" };
      }
      if (args[0] === "session" && args[1] === "read") {
        const sid = args[3];
        return { code: 0, stdout: JSON.stringify(mockTranscripts[sid] ?? { entries: [] }), stderr: "" };
      }
      return { code: 1, stdout: "", stderr: "unexpected" };
    },
  };

  const Sessions = loadSessionsMod(hostValues);

  const pendingTasks: Promise<unknown>[] = [];
  const cx = {
    focus_handle: () => ({ focus() {} }),
    timer: { every() {} },
    notify() {},
    stop_propagation() {},
    spawn(fn: (acx: any) => Promise<unknown>) {
      const p = fn(cx);
      pendingTasks.push(p);
      return { cancel() {} };
    },
  };

  const view = new Sessions();
  view.init({}, cx);
  await Promise.all(pendingTasks);
  pendingTasks.length = 0;

  expect(h2Calls[0]).toEqual(["session", "ls", "--json", "t1", "--cwd", "/home/dev/project-alpha", "--agent", "claude"]);
  expect(h2Calls[1]).toEqual([
    "session",
    "read",
    "--json",
    "sess-cur-1",
    "--agent",
    "claude",
    "--target",
    "t1",
    "--cwd",
    "/home/dev/project-alpha",
  ]);

  const tree1 = view.render(cx);

  // 1) Resizable vertical divider wraps upper session list and lower detail pane.
  const split = findById(tree1, "sessions-split");
  expect(split).not.toBeNull();
  expect(split.nodes).toHaveLength(2);
  expect(split.nodes[0].calls.size?.[0]).toBe(208);
  expect(split.nodes[0].calls.size_range).toEqual([84, 680]);
  expect(split.nodes[1].calls.size_range).toEqual([120]);
  split.calls.on_resize[0]([296, 340], cx);
  expect(localStore["sessions.topHeight"]).toBe("296");
  expect(view.splitTopHeight).toBe(296);

  // 2) Conversation detail defaults to showing ONLY dialogue; thoughts & tool calls are collapsed by default.
  const transcript1Texts = collectTexts(findById(tree1, "sessions-transcript")).join(" ");
  expect(transcript1Texts).toContain("请重构 auth.rs 中的 token 校验逻辑");
  expect(transcript1Texts).toContain("已完成 token 校验重构并通过单元测试。");
  expect(transcript1Texts).toContain("思考过程 · 1 次工具调用");
  expect(transcript1Texts).not.toContain("先阅读 auth.rs 了解现有签名校验流程");
  expect(transcript1Texts).not.toContain("Read src/auth.rs");

  // Clicking the collapsed activity bar unfolds thoughts and tool calls on demand.
  const actToggle = findById(tree1, "sess-act-toggle-act-1");
  expect(actToggle).not.toBeNull();
  actToggle.calls.on_click[0]({}, cx);
  const expandedTree = view.render(cx);
  const expandedTexts = collectTexts(findById(expandedTree, "sessions-transcript")).join(" ");
  expect(expandedTexts).toContain("先阅读 auth.rs 了解现有签名校验流程");
  expect(expandedTexts).toContain("Read src/auth.rs");

  // Toolbar "折叠过程" collapses all steps back with one click.
  findById(expandedTree, "sessions-toggle-steps").calls.on_click[0]({}, cx);
  const collapsedAgainTexts = collectTexts(findById(view.render(cx), "sessions-transcript")).join(" ");
  expect(collapsedAgainTexts).not.toContain("Read src/auth.rs");

  // 3) Session rows carry NO cursor-anchored native tooltip; hover renders opposite-edge card and right-click dismisses it immediately.
  const row0 = findById(tree1, "session-row-0");
  const row1 = findById(tree1, "session-row-1");
  expect(row0.calls.tooltip).toBeUndefined();
  expect(row1.calls.tooltip).toBeUndefined();
  expect(findById(tree1, "sessions-hover-tip")).toBeNull();

  // Simulate hovering row1 when pointer is in bottom half -> hover tip docks at top(6), away from pointer.
  const listPane = findById(tree1, "sessions-list-pane");
  listPane.calls.on_size_change[0]({ width: 500, height: 220 });
  listPane.calls.on_mouse_move[0]({ local_position: { x: 80, y: 180 } }, cx);
  row1.calls.on_hover[0](true, cx);
  const hoverTree = view.render(cx);
  const hoverCard = findById(hoverTree, "sessions-hover-tip");
  expect(hoverCard).not.toBeNull();
  expect(hoverCard.calls.top?.[0]).toBe(6);
  expect(hoverCard.calls.bottom).toBeUndefined();
  expect(collectTexts(hoverCard).join(" ")).toContain("排查数据库连接池泄漏问题");

  // Right-clicking row1 immediately unmounts hover card AND opens context menu with full-access resume command.
  findById(hoverTree, "session-row-1").calls["on_mouse_down:right"][1]({}, cx);
  await Promise.all(pendingTasks);
  pendingTasks.length = 0;

  const afterRightClickTree = view.render(cx);
  expect(findById(afterRightClickTree, "sessions-hover-tip")).toBeNull();
  expect(overlays).toHaveLength(1);
  expect(overlays[0].mod).toBe("menu");
  expect(overlays[0].kind).toBe("popup");
  expect(overlays[0].props.tab).toBe("session");
  expect(overlays[0].props.target.sessionId).toBe("sess-prev-2");
  expect(overlays[0].props.target.resumeCommand).toBe(
    "claude --dangerously-skip-permissions --resume sess-prev-2",
  );

  // Verify the menu mod copies the full-access command to clipboard.
  const copied: string[] = [];
  const notices: any[] = [];
  const closedOverlays: string[] = [];
  const menuValues = {
    View: class {},
    div: element,
    svg: element,
    v_flex: element,
    h_flex: element,
    run() {},
    props: () => ({ ...overlays[0].props, overlay: "ov-session" }),
    closeOverlay: (id: string) => closedOverlays.push(id),
    openOverlay() {},
    theme: () => ({ popover: "#18181c", border: "#2e2e38", foreground: "#f4f4f5", muted: "#a1a1aa", dim: "#71717a", selected: "#272730", hover: "#202028" }),
    keys: () => [],
    terminals: () => [],
    spaces: () => [],
    panes: () => [],
    exec: async () => ({ code: 0, stdout: "", stderr: "" }),
    settings: () => ({ language: lang }),
    h2: async () => ({ code: 0, stdout: "", stderr: "" }),
    notify: (n: any) => notices.push(n),
  };
  const Menu = new Function(
    ...Object.keys(menuValues),
    menuSource.replace(/^import .*;\n/gm, "").replace("export default class Menu", "return class Menu"),
  )(...Object.values(menuValues));

  const menuView = new Menu();
  menuView.init({}, { focus_handle: () => ({ focus() {} }), spawn() {} });
  const menuTree = menuView.render({});
  const item0 = findById(menuTree, "mi-0");
  expect(collectTexts(item0)).toContain("复制恢复对话命令（Full Access）");

  item0.calls.on_click[0]({}, { write_to_clipboard: (txt: string) => copied.push(txt) });
  expect(closedOverlays).toEqual(["ov-session"]);
  expect(copied).toEqual(["claude --dangerously-skip-permissions --resume sess-prev-2"]);
  expect(notices).toHaveLength(1);
});

test("sessions mod skips background h2 calls during herdr2 check", () => {
  let spawned = false;
  const hostValues = {
    View: class {},
    div: element,
    svg: element,
    v_flex: element,
    h_flex: element,
    v_resizable: (id: string) => (element() as any).id(id),
    resizable_panel: () => element(),
    InputState: { new: makeInputState },
    Input: { new: () => element() },
    Scrollbar: { vertical: () => element() },
    localStorage: { getItem: () => null, setItem() {} },
    spaces: () => [],
    panes: () => [],
    focused: () => null,
    terminals: () => [],
    theme: () => ({}),
    settings: () => ({ language: "en", zoom: 1 }),
    checking: () => true,
    openOverlay: () => "ov",
    h2: async () => {
      spawned = true;
      return { code: 0, stdout: "{}", stderr: "" };
    },
  };
  const Sessions = loadSessionsMod(hostValues);

  const view = new Sessions();
  view.init({}, { focus_handle: () => ({ focus() {} }), timer: { every() { spawned = true; } }, spawn() { spawned = true; } });
  expect(spawned).toBe(false);
  expect(findById(view.render({}), "sessions")).not.toBeNull();
});
