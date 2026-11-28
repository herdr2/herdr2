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
              if (key === "child" || key === "content") {
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

test("sessions panel renders identical UI across CLI agent and terminal tabs, shows all CLI session types by default, and supports filtering & full-access resume", async () => {
  const h2Calls: string[][] = [];
  const overlays: any[] = [];
  const notices: any[] = [];
  const localStore: Record<string, string> = {};
  const sharedStore: Record<string, any> = {};
  let lang = "zh-CN";

  // Start on a live Claude Code CLI agent tab — UI must still show the unified Agent Bar & ALL session types!
  let currentPanes: any[] = [
    {
      pane: "p1",
      tab: "tab1",
      terminal: "t1",
      cwd: "/home/dev/project-alpha",
      focused: true,
      program: { key: "claude", name: "Claude Code", agent: true, shell: false },
    },
  ];
  let currentTerminals: any[] = [
    {
      address: "t1",
      cwd: "/home/dev/project-alpha",
      turn: 3,
      saved_cli: { kind: "claude", session_id: "sess-cur-1" },
      program: { key: "claude", name: "Claude Code", agent: true, shell: false },
    },
  ];

  const claudeSess = {
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
  };
  const codexSess = {
    session_id: "codex-sess-9",
    agent: "codex",
    title: "优化 SQL 查询索引",
    preview: "优化 SQL 查询索引",
    turns: 2,
    created_ms: 1700001000000,
    updated_ms: 1700002000000,
    model: "gpt-5.4",
    git_branch: "perf/sql",
    current: false,
    resume_command: "codex resume --dangerously-bypass-approvals-and-sandbox codex-sess-9",
  };
  const agySess = {
    session_id: "agy-sess-3",
    agent: "agy",
    title: "新增 CLI 会话历史面板",
    preview: "新增 CLI 会话历史面板",
    turns: 4,
    created_ms: 1699998000000,
    updated_ms: 1699999000000,
    model: "gemini-4-carbon-high",
    git_branch: "main",
    current: false,
    resume_command: "agy --dangerously-skip-permissions --conversation agy-sess-3",
  };

  const listByAgent: Record<string, any> = {
    all: {
      agent: "all",
      agent_name: "All CLI Agents",
      cwd: "/home/dev/project-alpha",
      sessions: [claudeSess, codexSess, agySess],
    },
    claude: {
      agent: "claude",
      agent_name: "Claude Code",
      cwd: "/home/dev/project-alpha",
      sessions: [claudeSess],
    },
    codex: {
      agent: "codex",
      agent_name: "Codex",
      cwd: "/home/dev/project-alpha",
      sessions: [codexSess],
    },
    agy: {
      agent: "agy",
      agent_name: "Antigravity",
      cwd: "/home/dev/project-alpha",
      sessions: [agySess],
    },
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
        { kind: "assistant", turn: 1, text: "已完成 token 校验重构并通过单元测试：\n\n```rust\nfn verify_token() -> bool { true }\n```", ts_ms: 1700000020000 },
      ],
    },
    "codex-sess-9": {
      session_id: "codex-sess-9",
      agent: "codex",
      agent_name: "Codex",
      title: "优化 SQL 查询索引",
      model: "gpt-5.4",
      updated_ms: 1700002000000,
      resume_command: "codex resume --dangerously-bypass-approvals-and-sandbox codex-sess-9",
      entries: [
        { kind: "user", turn: 1, text: "为 sessions 表添加复合索引", ts_ms: 1700001500000 },
        { kind: "assistant", turn: 1, text: "已添加 `(project_id, updated_at)` 复合索引。", ts_ms: 1700001800000 },
      ],
    },
  };

  const hostValues = {
    View: class {},
    div: element,
    svg: element,
    image: element,
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
    panes: () => currentPanes,
    focused: () => ({ space: "s1", tab: "tab1", pane: "p1", terminal: "t1", cwd: "/home/dev/project-alpha" }),
    terminals: () => currentTerminals,
    knownAgents: async () => [
      { key: "claude", name: "Claude Code", icon: "icons/claude.svg", tint: false, installed: true, run: ["claude"] },
      { key: "codex", name: "Codex", icon: "icons/codex.svg", tint: false, installed: true, run: ["codex"] },
      { key: "antigravity", name: "Antigravity", icon: "icons/antigravity.svg", tint: false, installed: true, run: ["agy"] },
    ],
    shared: (k: string) => sharedStore[k] ?? null,
    setShared: (k: string, v: any) => { sharedStore[k] = v; },
    theme: () => ({
      background: "#111114",
      raised: "#18181c",
      popover: "#1c1c22",
      surface: "#151519",
      code_bg: "#141418",
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
    notify: (n: any) => notices.push(n),
    h2: async (args: string[]) => {
      h2Calls.push(args);
      if (args[0] === "session" && args[1] === "ls") {
        const agIdx = args.indexOf("--agent");
        const ag = agIdx >= 0 ? args[agIdx + 1] : "all";
        return { code: 0, stdout: JSON.stringify(listByAgent[ag] ?? { agent: ag, sessions: [] }), stderr: "" };
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
  const copiedDirect: string[] = [];
  const cx = {
    focus_handle: () => ({ focus() {} }),
    timer: { every() {} },
    notify() {},
    stop_propagation() {},
    write_to_clipboard: (txt: string) => copiedDirect.push(txt),
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

  // 1) Even on a CLI agent tab (`claude`), requests `--agent all` by default and renders the unified Agent Filter Bar!
  expect(h2Calls[0]).toEqual(["session", "ls", "--json", "t1", "--cwd", "/home/dev/project-alpha", "--agent", "all", "--limit", "120"]);
  const tree1 = view.render(cx);
  const agentBar = findById(tree1, "sessions-agent-bar");
  expect(agentBar).not.toBeNull();
  expect(findById(tree1, "sessions-agent-pill-all")).not.toBeNull();
  expect(findById(tree1, "sessions-agent-pill-claude")).not.toBeNull();
  expect(findById(tree1, "sessions-agent-pill-codex")).not.toBeNull();
  expect(findById(tree1, "sessions-agent-pill-agy")).not.toBeNull();

  // All 3 session types (Claude Code, Codex, Antigravity) render together in the list!
  const listTexts = collectTexts(findById(tree1, "sessions-list")).join(" ");
  expect(listTexts).toContain("重构认证中间件并补充单测");
  expect(listTexts).toContain("优化 SQL 查询索引");
  expect(listTexts).toContain("新增 CLI 会话历史面板");
  expect(listTexts).toContain("Claude Code");
  expect(listTexts).toContain("Codex");
  expect(listTexts).toContain("Antigravity");

  // 2) Switching to a plain zsh terminal tab keeps the EXACT same UI (`sessions-agent-bar` + all sessions)!
  currentPanes = [
    {
      pane: "p1",
      tab: "tab1",
      terminal: "t1",
      cwd: "/home/dev/project-alpha",
      focused: true,
      program: { key: "zsh", name: "zsh", agent: false, shell: true },
    },
  ];
  currentTerminals = [
    {
      address: "t1",
      cwd: "/home/dev/project-alpha",
      turn: 0,
      program: { key: "zsh", name: "zsh", agent: false, shell: true },
    },
  ];
  const shellTree = view.render(cx);
  await Promise.all(pendingTasks);
  pendingTasks.length = 0;
  expect(findById(shellTree, "sessions-agent-bar")).not.toBeNull();
  expect(findById(shellTree, "sessions-agent-pill-all")).not.toBeNull();

  // 3) Conversation detail defaults to showing ONLY dialogue; thoughts & tool calls are collapsed.
  const transcript1Texts = collectTexts(findById(tree1, "sessions-transcript")).join(" ");
  expect(transcript1Texts).toContain("请重构 auth.rs 中的 token 校验逻辑");
  expect(transcript1Texts).toContain("verify_token");
  expect(transcript1Texts).toContain("思考过程 · 1 次工具调用");
  expect(transcript1Texts).not.toContain("Read src/auth.rs");

  // 4) Clicking the Codex filter pill filters to Codex sessions and loads Codex's conversation & full-access resume command.
  findById(shellTree, "sessions-agent-pill-codex").calls.on_click[0]({}, cx);
  await Promise.all(pendingTasks);
  pendingTasks.length = 0;

  const codexTree = view.render(cx);
  const codexRow0 = findById(codexTree, "session-row-0");
  expect(codexRow0.calls.tooltip).toBeUndefined();
  expect(collectTexts(codexRow0).join(" ")).toContain("优化 SQL 查询索引");
  expect(findById(codexTree, "session-row-1")).toBeNull();

  // Right-click opens menu with Codex's full-access resume command.
  codexRow0.calls["on_mouse_down:right"][1]({}, cx);
  expect(overlays).toHaveLength(1);
  expect(overlays[0].props.tab).toBe("session");
  expect(overlays[0].props.target.resumeCommand).toBe(
    "codex resume --dangerously-bypass-approvals-and-sandbox codex-sess-9",
  );

  // Clicking "全部" pill restores all session types.
  findById(codexTree, "sessions-agent-pill-all").calls.on_click[0]({}, cx);
  await Promise.all(pendingTasks);
  pendingTasks.length = 0;
  const restoredAllTree = view.render(cx);
  expect(findById(restoredAllTree, "session-row-2")).not.toBeNull();
});

test("sessions mod skips background h2 calls during herdr2 check", () => {
  let spawned = false;
  const hostValues = {
    View: class {},
    div: element,
    svg: element,
    image: element,
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
    knownAgents: async () => [],
    shared: () => null,
    setShared() {},
    theme: () => ({}),
    settings: () => ({ language: "en", zoom: 1 }),
    checking: () => true,
    openOverlay: () => "ov",
    notify() {},
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
