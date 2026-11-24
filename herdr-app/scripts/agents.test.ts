import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";
import { resolveSemanticLabel, resolveSubAgentLabel, buildHoverTooltip, buildLocalFallbackBriefing, mergeBriefingSnapshots } from "../defaults/mods/agents/briefing_model.js";

const source = (await readFile(new URL("../defaults/mods/agents/main.js", import.meta.url), "utf8"))
  .replace(/^import .*;\n/gm, "").replace("export default class Agents", "class Agents");

function harness(pins: string[], globals: Record<string, unknown> = {}) {
  const data = new Map([["pins", JSON.stringify(pins)]]);
  const writes: unknown[] = [];
  const storage = { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => data.set(k, v), removeItem: (k: string) => data.delete(k) };
  const mergedGlobals: Record<string, unknown> = {
    resolveSemanticLabel,
    resolveSubAgentLabel,
    buildHoverTooltip,
    buildLocalFallbackBriefing,
    mergeBriefingSnapshots,
    checking: () => true,
    ...globals,
  };
  const Agents = new Function("View", "localStorage", "props", "run", ...Object.keys(mergedGlobals), `${source}; return Agents;`)(
    class {}, storage, () => ({}), (...args: unknown[]) => writes.push(args),
    ...Object.values(mergedGlobals),
  );
  const agents = new Agents();
  const ticks: { ms: number; fn: Function }[] = [];
  agents.init({}, { timer: { every(ms: number, fn: Function) { ticks.push({ ms, fn }); } } });
  return { agents, data, writes, ticks };
}

test("legacy pins migrate once listed, retaining offline pins until their labels arrive", () => {
  const { agents, data, writes } = harness(["t1", "offline/t2"]);
  agents.migratePins([]);
  expect(JSON.parse(data.get("pins")!)).toEqual(["t1", "offline/t2"]);
  const local = { address: "t1", state: "live", labels: {} };
  agents.migratePins([local]);
  agents.migratePins([local]);
  expect(writes).toEqual([["terminal.pin", { terminal: "t1", pinned: true }]]);
  expect(JSON.parse(data.get("pins")!)).toEqual(["t1", "offline/t2"]);
  agents.migratePins([{ ...local, labels: { "herdr.pinned": "1" } }]);
  expect(JSON.parse(data.get("pins")!)).toEqual(["offline/t2"]);
  const remote = { address: "offline/t2", state: "live", labels: {} };
  agents.migratePins([remote]);
  expect(writes).toHaveLength(2);
  agents.migratePins([{ ...remote, labels: { "herdr.pinned": "1" } }]);
  expect(data.has("pins")).toBe(false);
});

test("already shared pins need no write and pending writes survive restart", () => {
  expect(harness([]).data.has("pins")).toBe(false);
  const { agents, data, writes } = harness(["t1"]);
  agents.migratePins([{ address: "t1", state: "live", labels: { "herdr.pinned": "1" } }]);
  expect(writes).toHaveLength(0);
  expect(data.has("pins")).toBe(false);
  const retry = harness(["t2"]);
  retry.agents.migratePins([{ address: "t2", state: "live", labels: {} }]);
  expect(JSON.parse(retry.data.get("pins")!)).toEqual(["t2"]);
  const restarted = harness(JSON.parse(retry.data.get("pins")!));
  restarted.agents.migratePins([{ address: "t2", state: "live", labels: {} }]);
  expect(restarted.writes).toHaveLength(1);
});

test("an unnamed tab's row reads its terminal's title, not the tab number", () => {
  const title = new Function("View", "localStorage", "props", "run", `${source}; return title;`)(class {}, {}, () => ({}), () => {});
  const claude = { program: { name: "claude", key: "claude" }, title: "Herdr session names" };
  expect(title({ name: "3", named: false }, claude)).toBe("Herdr session names");
  expect(title({ name: "claude", named: true }, claude)).toBe("Herdr session names");
  expect(title({ name: "fix login", named: true }, claude)).toBe("fix login");
  expect(title({ name: "3", named: false }, { ...claude, title: "claude" })).toBe("3");
});

test("an empty OSC title published as the terminal address keeps the tab name", () => {
  const title = new Function("View", `${source}; return title;`)(class {});
  for (const address of ["t2", "remote/t2"]) {
    for (const identity of [{ terminal: address }, { address }]) {
      const lead = { ...identity, program: { name: "Claude Code", key: "claude" }, title: address };
      expect(title({ name: "2", named: false }, lead)).toBe("2");
      expect(title({ name: "Claude Code", named: true }, lead)).toBe("Claude Code");
      expect(title({ name: "fix login", named: true }, lead)).toBe("fix login");
      expect(title({ name: "2", named: false }, { ...lead, title: "renamed-task" })).toBe("renamed-task");
    }
  }
});

test("a title that is a folder never replaces the tab's name", () => {
  const title = new Function("View", "localStorage", "props", "run", `${source}; return title;`)(class {}, {}, () => ({}), () => {});
  const claude = { program: { name: "claude", key: "claude" }, title: "Herdr session names" };
  expect(title({ name: "3", named: false }, { ...claude, title: "~/src/api" })).toBe("3");
  expect(title({ name: "3", named: false }, { ...claude, title: "/tmp" })).toBe("3");
  expect(title({ name: "3", named: false }, { ...claude, title: "\u2733 ~/src/api" })).toBe("3");
  expect(title({ name: "3", named: false }, { ...claude, title: "C:\\src\\api" })).toBe("3");
  expect(title({ name: "3", named: false }, { ...claude, title: "\u2733 Herdr session names" })).toBe("Herdr session names");
});

test("tab labels prefer the first agent, then focused terminal, then first terminal", () => {
  const leadPanes = new Function("View", `${source}; return leadPanes;`)(class {});
  const pane = (tab: string, terminal: string, agent = false, focused = false) => ({ tab, terminal, program: { agent }, focused });
  const all = [
    { tab: "agent", focused: true }, // A mod pane has no terminal.
    pane("agent", "shell", false, true), pane("agent", "first-agent", true), pane("agent", "second-agent", true, true),
    pane("focused", "first-shell"), pane("focused", "focused-shell", false, true),
    pane("first", "first"), pane("first", "second"),
  ];
  const leads = leadPanes(all);
  expect([...leads].map(([tab, p]: any) => [tab, p.terminal])).toEqual([
    ["agent", "first-agent"], ["focused", "focused-shell"], ["first", "first"],
  ]);
});

// Lightweight element descriptions: execute conditional builders and retain text,
// without running input callbacks or pretending to exercise native GPUI layout.
function element() {
  const node = { id: "", children: [] as any[] };
  const el: any = new Proxy(node, { get: (_, method) => {
    if (method === "node") return node;
    return (...args: any[]) => {
      if (method === "id") node.id = args[0];
      if (method === "on_click") (node as any).click = args[0];
      if (method === "on_mouse_down") (node as any).mouseDown = args;
      if (method === "on_hover") (node as any).hover = args[0];
      if (method === "bg") (node as any).bg = args[0];
      if (method === "h") (node as any).height = args[0];
      if (method === "size_full") (node as any).sizeFull = true;
      if (method === "child") node.children.push(args[0]?.node ?? args[0]);
      if (method === "children") node.children.push(...args[0].map((child: any) => child?.node ?? child));
      if (method === "when" && args[0]) args[1](el);
      return el;
    };
  } });
  return el;
}
function textOf(node: any): string {
  return typeof node === "string" ? node : (node?.children ?? []).map(textOf).join(" ");
}

test("a failed agent check keeps cached launchable agents and shows an error instead of missing agents", async () => {
  const menuSource = (await readFile(new URL("../defaults/mods/new-menu/main.js", import.meta.url), "utf8"))
    .replace(/^import .*;\n/gm, "").replace("export default class NewMenu", "class NewMenu");
  const cached = [
    { key: "claude", name: "Claude Code", run: ["claude"], installed: true },
    { key: "codex", name: "Codex", run: ["codex"], installed: false },
  ];
  for (const agents of [[], cached]) {
    const writes: unknown[] = [];
    const globals = {
      View: class {}, spaces: () => [], shared: () => ({ "": agents }), setShared: (...args: unknown[]) => writes.push(args),
      knownAgents: async () => { throw Error("shell failed"); },
      theme: () => ({}), props: () => ({}), keys: () => [], window: () => ({ platform: "macos" }), panes: () => [],
      div: element, svg: element, image: element, h_flex: element, v_flex: element,
    };
    const NewMenu = new Function(...Object.keys(globals), `${menuSource}; return NewMenu;`)(...Object.values(globals));
    const menu = new NewMenu();
    let task: Promise<void>;
    menu.init({}, { focus_handle: () => ({ focus() {} }), spawn: (run: (cx: unknown) => Promise<void>) => { task = run({ notify() {} }); } });
    await task!;
    const text = textOf(menu.render().node);
    expect(text).toContain("Could not check installed agents");
    expect(text).not.toContain("not installed");
    expect(text).not.toContain("Codex");
    if (agents.length) expect(text).toContain("Claude Code");
    expect(writes).toEqual([]);
  }
});

for (const sort of ["priority", "grouped"]) test(`${sort} render reads panes once and refreshes labels on the next render`, () => {
  const list = Array.from({ length: 64 }, (_, i) => ({
    address: `t${i}`, pane: `p${i}`, tab: `tab${i}`, space: "s1", space_name: "Project",
    program: { name: "Claude", key: "claude", agent: true }, title: "Fallback", status: "idle", seq: i,
  }));
  const snapshot = list.map((a, i) => ({ tab: a.tab, terminal: a.address, program: a.program, title: `Task ${i}` }));
  let reads = 0;
  const orders: string[][] = [];
  const { agents } = harness([], {
    agents: () => list, terminals: () => list.map(a => ({ ...a, state: "live" })),
    panes: () => { reads++; return structuredClone(snapshot); }, focused: () => null,
    spaces: () => [{ tabs: list.map(a => ({ id: a.tab, name: "Claude", named: true })) }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }), shared: () => sort,
    theme: () => ({}), setAgentOrder: (order: string[]) => orders.push(order),
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const first = textOf(agents.render({}).node);
  expect(reads).toBe(1);
  for (let i = 0; i < list.length; i++) expect(first).toContain(`Task ${i}`);
  expect(orders[0]).toHaveLength(list.length);
  snapshot[63].title = "Updated task";
  const second = textOf(agents.render({}).node);
  expect(reads).toBe(2);
  expect(second).toContain("Updated task");
  expect(second).not.toContain("Task 63");
  expect(orders[1]).toEqual(orders[0]);
});

for (const sort of ["priority", "grouped"]) for (const named of [false, true]) test(`${sort}: agents sharing a ${named ? "named" : "numbered"} tab keep their own titles`, () => {
  const program = { name: "Claude", key: "claude", agent: true };
  let list = ["Alpha task", "Beta task", "Solo task"].map((title, i) => ({
    address: `t${i}`, pane: `p${i}`, tab: i < 2 ? "shared" : "solo", space: "s1", space_name: "Project",
    program, title, status: "idle", seq: i,
  }));
  const { agents } = harness([], {
    agents: () => list, terminals: () => list.map(a => ({ ...a, state: "live" })),
    panes: () => list.map(a => ({ ...a, terminal: a.address })), focused: () => null,
    spaces: () => [{ tabs: [{ id: "shared", name: named ? "Named tab" : "1", named }, { id: "solo", name: "Solo tab", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }), shared: () => sort,
    theme: () => ({}), setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const rows = () => {
    const result = new Map<string, string>();
    const walk = (node: any) => {
      if (node?.id?.startsWith("ag-t")) result.set(node.id, textOf(node));
      (node?.children ?? []).forEach(walk);
    };
    walk(agents.render({}).node);
    return result;
  };
  const shared = rows();
  expect(shared.get("ag-t0")).toContain("Alpha task");
  expect(shared.get("ag-t1")).toContain("Beta task");
  expect(shared.get("ag-t1")).not.toContain("Alpha task");
  expect(shared.get("ag-t0")).not.toContain("Named tab");
  expect(shared.get("ag-t2")).toContain("Solo tab");
  expect(shared.get("ag-t2")).not.toContain("Solo task");
  list = list.filter(a => a.address !== "t1");
  expect(rows().get("ag-t0")).toContain(named ? "Named tab" : "Alpha task");
});

// A kept sub-agent: started by t1 (`hc run`), then given a pane (`terminal.keep`).
function keptRender(sort: string, { width = 268, kept = [{ address: "t3", parent: "t1" }], paneless = [] as { address: string; parent: string }[], pinned = [] as string[] } = {}) {
  const program = { name: "Claude", key: "claude", agent: true };
  const row = (address: string, seq: number) => ({ address, pane: `p-${address}`, tab: "tab1", space: "s1", space_name: "Project", program, title: `Task ${address}`, status: "working", seq, pinned: pinned.includes(address) });
  const list = [row("t1", 1), ...kept.map((k, i) => row(k.address, i + 2))];
  const live = (address: string, parent: string | null) => ({ address, parent, state: "live", status: "working", program, labels: {} });
  const orders: string[][] = [];
  const { agents, writes } = harness([], {
    agents: () => list,
    terminals: () => [live("t1", null), ...paneless.map(k => live(k.address, k.parent)), ...kept.map(k => live(k.address, k.parent))],
    panes: () => list.map(a => ({ tab: a.tab, terminal: a.address, program, title: a.title })), focused: () => null,
    spaces: () => [{ tabs: [{ id: "tab1", name: "Project", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: width } }), shared: () => sort,
    theme: () => ({}), setAgentOrder: (order: string[]) => orders.push(order),
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const ids: string[] = [];
  const clicks = new Map<string, () => void>();
  const texts = new Map<string, string>();
  const walk = (node: any) => {
    if (node?.id) { ids.push(node.id); texts.set(node.id, textOf(node)); if (node.click) clicks.set(node.id, node.click); }
    (node?.children ?? []).forEach(walk);
  };
  walk(agents.render({}).node);
  return { ids, clicks, writes, texts, order: orders[orders.length - 1] };
}

for (const sort of ["priority", "grouped"]) test(`${sort}: a kept sub-agent shows once, under its parent, and its row focuses its pane`, () => {
  const { ids, clicks, writes, texts, order } = keptRender(sort);
  expect(texts.get("ag-t1")).toContain("Project");
  expect(texts.get("ag-id-t1")).toBe("t1");
  expect(texts.get("sub-t3")).toContain("Claude");
  expect(texts.get("sub-t3")).not.toContain("Project");
  expect(ids).toContain("ag-t1");
  expect(ids).toContain("sub-t3");
  expect(ids).not.toContain("ag-t3");
  expect(order).toEqual(["t1"]);
  clicks.get("sub-t3")!();
  expect(writes).toContainEqual(["pane.focus", { terminal: "t3" }]);
});

for (const sort of ["priority", "grouped"]) test(`${sort}: a kept sub-agent under a pane-less one shows once in the tree`, () => {
  const { ids } = keptRender(sort, { paneless: [{ address: "t3", parent: "t1" }], kept: [{ address: "t4", parent: "t3" }] });
  expect(ids).toContain("sub-t3");
  expect(ids).toContain("sub-t4");
  expect(ids).not.toContain("ag-t4");
});

for (const sort of ["priority", "grouped"]) test(`${sort}: a kept sub-agent keeps its own row where no tree shows it`, () => {
  expect(keptRender(sort, { width: 44 }).ids).toContain("ag-t3"); // the rail draws no trees
  const pinned = keptRender(sort, { pinned: ["t3"] });
  expect(pinned.ids).toContain("ag-t3");
  expect(pinned.texts.get("ag-t1")).toContain("Task t1");
  expect(pinned.texts.get("ag-t3")).toContain("Task t3");
  expect(keptRender(sort, { kept: [{ address: "t3", parent: "t9" }] }).ids).toContain("ag-t3"); // its parent ended
});

for (const width of [268, 44]) test(`the focused agent's row is marked as selected ${width < 160 ? "in the rail" : "in the wide list"}`, () => {
  const program = { name: "Claude", key: "claude", agent: true };
  const list = ["t1", "t2"].map((address, i) => ({
    address, pane: `p-${address}`, tab: address, space: "s1", space_name: "Project",
    program, title: address, status: "idle", seq: i, focused: address === "t2",
  }));
  const { agents } = harness([], {
    agents: () => list, terminals: () => list.map(a => ({ ...a, state: "live", labels: {} })),
    panes: () => list.map(a => ({ ...a, terminal: a.address })), focused: () => ({ agent: "t2" }),
    spaces: () => [{ tabs: list.map(a => ({ id: a.tab, name: a.address, named: true })) }],
    layout: () => ({ id: "sidebar", size: { px: width } }), shared: () => "grouped",
    theme: () => ({ selected: "selected", hover: "hover" }), setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const fills = new Map<string, unknown>();
  const walk = (node: any) => {
    if (typeof node?.id === "string" && /^ag-t\d+$/.test(node.id)) fills.set(node.id, node.bg);
    (node?.children ?? []).forEach(walk);
  };
  walk(agents.render({}).node);
  expect([...fills]).toEqual([["ag-t1", undefined], ["ag-t2", "selected"]]);
});

test("a title that opens with a bracket keeps it; each agent's own prefix still goes", () => {
  const title = new Function("View", "localStorage", "props", "run", `${source}; return title;`)(class {}, {}, () => ({}), () => {});
  const clean = new Function("View", "localStorage", "props", "run", `${source}; return clean;`)(class {}, {}, () => ({}), () => {});
  const claude = { program: { name: "claude", key: "claude" }, title: "" };
  // The reported bug: a Claude Code session renamed "[herdr] config" showed as "herdr] config".
  expect(title({ name: "3", named: false }, { ...claude, title: "[herdr] config" })).toBe("[herdr] config");
  // Titles as each agent writes them (from its source).
  const titles: [string, string][] = [
    ["[herdr] config", "[herdr] config"], // Claude Code: the session name
    ["⠋ [herdr] config | herdr-app", "[herdr] config | herdr-app"], // Codex working
    ["[ ! ] Action Required | [herdr] config | herdr-app", "Action Required | [herdr] config | herdr-app"], // Codex needs you
    ["[ . ] Action Required | [herdr] config | herdr-app", "Action Required | [herdr] config | herdr-app"], // ... its other blink phase
    ["● [ ! ] Action Required | herdr-app", "Action Required | herdr-app"], // ... with the microphone on
    ["✦  [herdr] config (herdr-app)", "[herdr] config (herdr-app)"], // Gemini working
    ["◇  Ready (herdr-app)", "Ready (herdr-app)"], // Gemini idle
    ["OC | [herdr] config", "OC | [herdr] config"], // OpenCode
    ["π - [herdr] config - herdr-app", "π - [herdr] config - herdr-app"], // Pi
    ["⠋ - Thinking - [herdr] config - grok", "Thinking - [herdr] config - grok"], // Grok: the spinner's " - " goes too
  ];
  for (const [raw, shown] of titles) expect(clean(raw)).toBe(shown);
});

for (const sort of ["priority", "grouped"]) test(`${sort}: headless ACP sub-agents nest and roll up urgency, and clicking ACP rows opens or switches to their tab`, () => {
  const opencode = { name: "OpenCode", key: "opencode", agent: true };
  const claude = { name: "Claude Code", key: "claude", agent: true };
  const list = [
    { address: "t1", pane: "p1", tab: "tb1", space: "s1", space_name: "win-app", program: claude, title: "Parent shell", status: "idle", seq: 1 },
    { address: "t2", pane: "p2", tab: "tb2", space: "s1", space_name: "win-app", program: claude, title: "Peer working", status: "working", seq: 2 },
    { address: "wsl/a6", acp: true, pane: null, tab: null, tab_index: 0, space: "headless:wsl", space_name: "herdr2", program: opencode, title: "Remote reviewer", status: "done", seq: 3 },
  ];
  const liveTerms = [
    { address: "t1", parent: null, state: "live", status: "idle", program: claude, labels: {} },
    { address: "a7", acp: true, parent: "t1", state: "live", status: "blocked", name: "sub-reviewer", program: opencode, labels: {} },
    { address: "t2", parent: null, state: "live", status: "working", program: claude, labels: {} },
    { address: "wsl/a6", acp: true, parent: null, state: "live", status: "idle", name: "Remote reviewer", program: opencode, labels: {} },
  ];
  const orders: string[][] = [];
  const { agents, writes } = harness([], {
    agents: () => list,
    terminals: () => liveTerms,
    panes: () => list.filter(a => a.pane).map(a => ({ pane: a.pane, tab: a.tab, terminal: a.address, program: a.program, title: a.title })),
    focused: () => null,
    spaces: () => [{ tabs: [{ id: "tb1", name: "Project", named: true }, { id: "tb2", name: "Work", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: () => sort,
    theme: () => ({}),
    setAgentOrder: (order: string[]) => orders.push(order),
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const clicks = new Map<string, () => void>();
  const texts = new Map<string, string>();
  const walk = (node: any) => {
    if (node?.id) {
      texts.set(node.id, textOf(node));
      if (node.click) clicks.set(node.id, node.click);
    }
    (node?.children ?? []).forEach(walk);
  };
  walk(agents.render({}).node);

  expect(texts.get("sub-a7")).toContain("sub-reviewer");
  expect(texts.get("sub-a7")).toContain("ACP");
  expect(texts.get("ag-wsl/a6")).toContain("ACP");
  expect(texts.get("ag-wsl/a6")).toContain("wsl");
  if (sort === "priority") {
    // Blocked ACP child a7 promotes idle parent t1 ahead of done/working peers.
    expect(orders.at(-1)?.[0]).toBe("t1");
  }
  clicks.get("sub-a7")!();
  clicks.get("ag-wsl/a6")!();
  expect(writes).toEqual([
    ["terminal.keep", { terminal: "a7", place: "tab" }],
    ["terminal.keep", { terminal: "wsl/a6", place: "tab" }],
  ]);
});

test("hovering agent or sub-agent rows exposes a remove button that moves agents to the recycle bin", () => {
  const opencode = { name: "OpenCode", key: "opencode", agent: true };
  const claude = { name: "Claude Code", key: "claude", agent: true };
  const list = [
    { address: "t1", pane: "p1", tab: "tb1", space: "s1", space_name: "win-app", program: claude, title: "Parent shell", status: "idle", seq: 1 },
    { address: "wsl/a6", acp: true, pane: null, tab: null, tab_index: 0, space: "headless:wsl", space_name: "herdr2", program: opencode, title: "Remote reviewer", status: "idle", seq: 2 },
  ];
  const liveTerms = [
    { address: "t1", parent: null, state: "live", status: "idle", program: claude, labels: {} },
    { address: "a7", acp: true, parent: "t1", state: "live", status: "working", name: "sub-reviewer", program: opencode, labels: {} },
    { address: "wsl/a6", acp: true, parent: null, state: "live", status: "idle", name: "Remote reviewer", program: opencode, labels: {} },
  ];
  const { agents, writes } = harness([], {
    agents: () => list,
    terminals: () => liveTerms,
    panes: () => [{ pane: "p1", tab: "tb1", terminal: "t1", program: claude, title: "Parent shell" }],
    focused: () => null,
    spaces: () => [{ tabs: [{ id: "tb1", name: "Project", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: () => "grouped",
    theme: () => ({}),
    setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const scan = () => {
    const clicks = new Map<string, (e: unknown, cx: { stop_propagation(): void; notify(): void }) => void>();
    const hovers = new Map<string, (on: boolean, cx: { notify(): void }) => void>();
    const walk = (node: any) => {
      if (node?.id) {
        if (node.click) clicks.set(node.id, node.click);
        if (node.hover) hovers.set(node.id, node.hover);
      }
      (node?.children ?? []).forEach(walk);
    };
    walk(agents.render({}).node);
    return { clicks, hovers };
  };
  const cx = { stop_propagation() {}, notify() {} };

  let tree = scan();
  tree.hovers.get("ag-t1")!(true, cx);
  tree = scan();
  expect(tree.clicks.has("rm-t1")).toBe(true);
  tree.clicks.get("rm-t1")!({}, cx);

  tree.hovers.get("ag-wsl/a6")!(true, cx);
  tree = scan();
  expect(tree.clicks.has("rm-wsl/a6")).toBe(true);
  tree.clicks.get("rm-wsl/a6")!({}, cx);

  tree.hovers.get("sub-a7")!(true, cx);
  tree = scan();
  expect(tree.clicks.has("rm-sub-a7")).toBe(true);
  tree.clicks.get("rm-sub-a7")!({}, cx);

  expect(writes).toEqual([
    ["agent.trash", { terminal: "t1" }],
    ["agent.trash", { terminal: "wsl/a6" }],
    ["agent.trash", { terminal: "a7" }],
  ]);
});

test("clicking agent status dot opens manual status popover and chooses agent.status commands", () => {
  const claude = { name: "Claude Code", key: "claude", agent: true };
  const list = [
    { address: "t1", pane: "p1", tab: "tb1", space: "s1", space_name: "win-app", program: claude, title: "Claude", status: "working", seq: 1 },
  ];
  const liveTerms = [
    { address: "t1", parent: null, state: "live", status: "working", manual_status: null, program: claude, labels: {} },
    { address: "a7", acp: true, parent: "t1", state: "live", status: "idle", manual_status: "blocked", name: "sub-reviewer", program: claude, labels: {} },
  ];
  const overlays: any[] = [];
  const closedOverlays: any[] = [];
  let currentProps: any = { overlay: "ov-1" };
  const { agents, writes } = harness([], {
    agents: () => list,
    terminals: () => liveTerms,
    panes: () => [{ pane: "p1", tab: "tb1", terminal: "t1", program: claude, title: "Claude" }],
    focused: () => null,
    spaces: () => [{ tabs: [{ id: "tb1", name: "Project", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: () => "grouped",
    theme: () => ({}),
    props: () => currentProps,
    openOverlay: (spec: any) => overlays.push(spec),
    closeOverlay: (id: any) => closedOverlays.push(id),
    setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const cx = { stop_propagation() {}, notify() {}, close() {} };
  const textOf = (node: any): string => {
    if (typeof node === "string") return node;
    if (!node || typeof node !== "object") return "";
    return (node.children ?? []).map(textOf).join(" ");
  };
  const findClick = (node: any, pred: (n: any) => boolean): any => {
    if (!node || typeof node !== "object") return null;
    if (pred(node) && node.click) return node.click;
    for (const ch of node.children ?? []) {
      const hit = findClick(ch, pred);
      if (hit) return hit;
    }
    return null;
  };

  const panelNode = agents.render({}).node;
  expect(textOf(panelNode)).toContain("Needs you");
  const statusClick = findClick(panelNode, (n) => n.id === "status-t1");
  expect(statusClick).toBeTruthy();
  statusClick({}, cx);
  expect(overlays[0]?.mod).toBe("agents");
  expect(overlays[0]?.props?.statusMenu).toBe(true);
  expect(overlays[0]?.props?.terminal).toBe("t1");

  currentProps = { overlay: "ov-1", statusMenu: true, terminal: "t1" };
  const menuNode = agents.render({}).node;
  const doneClick = findClick(menuNode, (n) => n.id === "st-item-done");
  const autoClick = findClick(menuNode, (n) => n.id === "st-item-auto");
  expect(doneClick).toBeTruthy();
  expect(autoClick).toBeTruthy();
  doneClick({}, cx);
  autoClick({}, cx);
  expect(writes).toEqual([
    ["agent.status", { terminal: "t1", status: "done" }],
    ["agent.status", { terminal: "t1", status: "auto" }],
  ]);
  expect(closedOverlays).toEqual(["ov-1", "ov-1"]);
});

test("drove ACP sub-agent row renders drove.display / title in subRow", () => {
  const codex = { name: "Codex", key: "codex", agent: true };
  const list = [
    { address: "t1", pane: "p1", tab: "tb1", space: "s1", space_name: "win-app", program: codex, title: "Coordinator", status: "working", seq: 1 },
  ];
  const liveTerms = [
    { address: "t1", parent: null, state: "live", status: "working", manual_status: null, program: codex, labels: {} },
    {
      address: "a9",
      acp: true,
      parent: "t1",
      state: "live",
      status: "working",
      manual_status: null,
      name: null,
      title: "coder · api-auth",
      program: codex,
      labels: { "drove.display": "coder · api-auth", "drove.role": "coder" },
    },
  ];
  const { agents } = harness([], {
    agents: () => list,
    terminals: () => liveTerms,
    panes: () => [{ pane: "p1", tab: "tb1", terminal: "t1", program: codex, title: "Coordinator" }],
    focused: () => null,
    spaces: () => [{ tabs: [{ id: "tb1", name: "Project", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: () => "grouped",
    theme: () => ({}),
    props: () => ({}),
    openOverlay() {},
    closeOverlay() {},
    setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const textOf = (node: any): string => {
    if (typeof node === "string") return node;
    if (!node || typeof node !== "object") return "";
    return (node.children ?? []).map(textOf).join(" ");
  };
  const treeNode = agents.render({}).node;
  expect(textOf(treeNode)).toContain("coder · api-auth");
});

test("agents header trash button opens recycle bin popup and dispatches restore, purge, and empty_trash", () => {
  const claude = { name: "Claude Code", key: "claude", agent: true };
  const trash = [
    {
      address: "wsl/a16",
      acp: true,
      custom_name: "claude-llm",
      program: claude,
      space_name: "herdr2",
      live: true,
      trashed_at: Date.now() - 120_000,
    },
  ];
  const overlays: any[] = [];
  const closedOverlays: any[] = [];
  let currentProps: any = {};
  const { agents, writes } = harness([], {
    agents: () => [],
    terminals: () => [],
    panes: () => [],
    focused: () => null,
    spaces: () => [],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: (k: string) => (k === "agents.trash" ? trash : "grouped"),
    theme: () => ({}),
    props: () => currentProps,
    openOverlay: (spec: any) => overlays.push(spec),
    closeOverlay: (id: any) => {
      expect(writes.length).toBeGreaterThanOrEqual(1);
      closedOverlays.push(id);
    },
    setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const cx = { stop_propagation() {}, notify() {}, close() {} };
  const findNode = (node: any, pred: (n: any) => boolean): any => {
    if (!node || typeof node !== "object") return null;
    if (pred(node)) return node;
    for (const ch of node.children ?? []) {
      const hit = findNode(ch, pred);
      if (hit) return hit;
    }
    return null;
  };
  const findClick = (node: any, pred: (n: any) => boolean): any => {
    if (!node || typeof node !== "object") return null;
    if (pred(node) && node.click) return node.click;
    for (const ch of node.children ?? []) {
      const hit = findClick(ch, pred);
      if (hit) return hit;
    }
    return null;
  };
  const textOf = (node: any): string => {
    if (typeof node === "string") return node;
    if (!node || typeof node !== "object") return "";
    return (node.children ?? []).map(textOf).join(" ");
  };

  const mainNode = agents.render({}).node;
  expect(textOf(mainNode)).toContain("1");
  findClick(mainNode, (n) => n.id === "ag-recycle")({}, cx);
  expect(overlays[0]?.mod).toBe("agents");
  expect(overlays[0]?.props?.trashMenu).toBe(true);

  currentProps = { overlay: "ov-trash", trashMenu: true };
  const trashNode = agents.render({}).node;
  expect(trashNode.id).toBe("ag-trash-menu");
  expect(trashNode.sizeFull).toBeUndefined();
  expect(trashNode.height).toBeGreaterThanOrEqual(100);
  const restoreBtn = findNode(trashNode, (n) => n.id === "trash-restore-wsl/a16");
  expect(restoreBtn).not.toBeNull();
  expect(restoreBtn.mouseDown?.[0]).toBe("left");
  expect(textOf(trashNode)).toContain("claude-llm");
  expect(textOf(trashNode)).toContain("Live");
  restoreBtn.click({}, cx);
  findClick(trashNode, (n) => n.id === "trash-purge-wsl/a16")({}, cx);
  findClick(trashNode, (n) => n.id === "trash-empty")({}, cx);
  expect(writes).toEqual([
    ["agent.restore", { terminal: "wsl/a16" }],
    ["agent.purge", { terminal: "wsl/a16" }],
    ["agent.empty_trash", {}],
  ]);
  expect(closedOverlays).toEqual(["ov-trash"]);
});

test("selecting a sub-agent in a tab or split pane highlights its subRow in the agents list", () => {
  const claude = { name: "Claude", key: "claude", agent: true };
  const codex = { name: "Codex", key: "codex", agent: true };
  const list = [
    { address: "wsl/t2t", pane: "p1", tab: "tab1", space: "s1", space_name: "Project", program: claude, title: "Claude", status: "idle", seq: 1, focused: false },
    { address: "wsl/a48", acp: true, pane: "p2", tab: "tab2", space: "s1", space_name: "Project", program: codex, title: "Codex", status: "working", seq: 2, focused: false },
    { address: "wsl/a49", acp: false, pane: "p3", tab: "tab3", space: "s1", space_name: "Project", program: codex, title: "Fork", status: "idle", seq: 3, focused: false },
  ];
  const terms = [
    { address: "wsl/t2t", parent: null, state: "live", status: "idle", program: claude, labels: {} },
    { address: "wsl/a48", acp: true, parent: "wsl/t2t", lineage: { kind: "handoff", parent: "wsl/t2t" }, state: "live", status: "working", name: "handoff-codex", program: codex, labels: {} },
    { address: "wsl/a49", acp: false, parent: "wsl/t2t", lineage: { kind: "fork", parent: "wsl/t2t", worktree_path: "/tmp/wt" }, state: "live", status: "idle", name: "fork-codex", program: codex, labels: {} },
    { address: "wsl/a50", acp: true, parent: "wsl/t2t", lineage: { kind: "side-chat", parent: "wsl/t2t" }, state: "live", status: "idle", name: "side-claude", program: claude, labels: {} },
  ];
  const paneList = [
    { pane: "p1", tab: "tab1", terminal: "wsl/t2t", program: claude, title: "Claude", focused: false },
    { pane: "p2", tab: "tab2", terminal: "wsl/a48", acp: true, program: codex, title: "Codex", focused: false },
    { pane: "p3", tab: "tab3", terminal: "wsl/a49", acp: false, program: codex, title: "Fork", focused: false },
  ];
  const { agents } = harness([], {
    agents: () => list,
    terminals: () => terms,
    panes: () => paneList,
    focused: () => ({ space: "s1", tab: "tab1", pane: "p1", terminal: "wsl/a50", shown: null, agent: "wsl/t2t" }),
    spaces: () => [{ tabs: [{ id: "tab1", name: "Claude", named: true }, { id: "tab2", name: "Codex", named: true }, { id: "tab3", name: "Fork", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: () => "grouped",
    theme: () => ({ selected: "selected-bg", raised: "raised-bg", hover: "hover-bg", foreground: "fg", muted: "muted", accent: "accent" }),
    setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const fills = new Map<string, unknown>();
  const texts = new Map<string, string>();
  const textOfLocal = (n: any): string => (typeof n === "string" ? n : (n?.children ?? []).map(textOfLocal).join(" "));
  const walk = (node: any) => {
    if (node?.id) {
      fills.set(node.id, node.bg);
      texts.set(node.id, textOfLocal(node));
    }
    (node?.children ?? []).forEach(walk);
  };
  walk(agents.render({}).node);
  expect(fills.get("ag-wsl/t2t")).toBeUndefined();
  expect(texts.get("ag-id-wsl/t2t")).toBe("t2t");
  expect(texts.get("ag-rt-wsl/t2t")).toBe("CLI");
  // Both Handoff and Fork stay standalone top-level sessions with upstream lineage badges.
  expect(texts.has("ag-wsl/a48")).toBe(true);
  expect(texts.has("sub-wsl/a48")).toBe(false);
  expect(texts.get("ag-id-wsl/a48")).toBe("a48");
  expect(texts.get("ag-kind-wsl/a48")).toContain("Handoff ← t2t");
  expect(texts.get("ag-rt-wsl/a48")).toBe("ACP");
  expect(texts.has("ag-wsl/a49")).toBe(true);
  expect(texts.has("sub-wsl/a49")).toBe(false);
  expect(texts.get("ag-id-wsl/a49")).toBe("a49");
  expect(texts.get("ag-kind-wsl/a49")).toContain("Fork ← t2t");
  expect(texts.get("ag-wt-wsl/a49")).toContain("WT");
  expect(texts.get("ag-rt-wsl/a49")).toBe("CLI");
  // Side Chat stays nested as a sub-agent under t2t, with selection highlight when focused.
  expect(fills.get("sub-wsl/a50")).toBe("selected-bg");
  expect(texts.get("sub-id-wsl/a50")).toBe("a50");
  expect(texts.get("sub-rt-wsl/a50")).toBe("ACP");
  expect(texts.get("sub-kind-wsl/a50")).toContain("Side");
});

for (const sort of ["grouped", "priority"]) test(`${sort}: agents and sub-agents are ordered by appearance time (oldest first, newest last)`, () => {
  const claude = { name: "Claude", key: "claude", agent: true };
  // Intentionally out of chronological order and with higher seq on newer agent.
  const list = [
    { address: "a3", acp: true, pane: "p3", tab: "tab3", space: "s1", space_name: "Project", program: claude, title: "Third", status: "working", seq: 30, created_ms: 3000 },
    { address: "a1", acp: true, pane: "p1", tab: "tab1", space: "s1", space_name: "Project", program: claude, title: "First", status: "working", seq: 10, created_ms: 1000 },
    { address: "a2", acp: true, pane: "p2", tab: "tab2", space: "s1", space_name: "Project", program: claude, title: "Second", status: "working", seq: 20, created_ms: 2000 },
  ];
  const terms = [
    { address: "a3", parent: null, state: "live", status: "working", program: claude, created_ms: 3000, labels: {} },
    { address: "a1", parent: null, state: "live", status: "working", program: claude, created_ms: 1000, labels: {} },
    { address: "a2", parent: null, state: "live", status: "working", program: claude, created_ms: 2000, labels: {} },
    { address: "a1-sub2", parent: "a1", state: "live", status: "idle", program: claude, created_ms: 2500, labels: {} },
    { address: "a1-sub1", parent: "a1", state: "live", status: "idle", program: claude, created_ms: 1500, labels: {} },
  ];
  const orders: string[][] = [];
  const { agents } = harness([], {
    agents: () => list,
    terminals: () => terms,
    panes: () => list.map((a) => ({ pane: a.pane, tab: a.tab, terminal: a.address, program: claude, title: a.title })),
    focused: () => null,
    spaces: () => [{ tabs: list.map((a) => ({ id: a.tab, name: a.title, named: true })) }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: () => sort,
    theme: () => ({}),
    setAgentOrder: (order: string[]) => orders.push(order),
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const rowIds: string[] = [];
  const walk = (node: any) => {
    if (typeof node?.id === "string" && (node.id.startsWith("ag-") || node.id.startsWith("sub-")) && !node.id.includes("-id-") && !node.id.includes("-rt-") && !node.id.includes("-kind-") && !node.id.includes("-wt-") && !node.id.includes("-time-") && !node.id.startsWith("ag-list") && !node.id.startsWith("ag-recycle") && !node.id.startsWith("ag-sort") && !node.id.startsWith("ag-briefing")) {
      rowIds.push(node.id);
    }
    (node?.children ?? []).forEach(walk);
  };
  walk(agents.render({}).node);
  expect(orders.at(-1)).toEqual(["a1", "a2", "a3"]);
  expect(rowIds).toEqual(["ag-a1", "sub-a1-sub1", "sub-a1-sub2", "ag-a2", "ag-a3"]);
});

test("semantic naming strictly preserves user custom names and explicit tab names", () => {
  const prog = { name: "Claude Code", key: "claude", agent: true };
  // 1. User custom name always wins over semantic_title
  expect(
    resolveSemanticLabel(
      { address: "a1", custom_name: "My Custom Name", semantic_title: "Fix login", program: prog },
      { address: "a1", labels: {} },
      { name: "1", named: false },
      null,
    ),
  ).toBe("My Custom Name");
  // 2. Explicit user-renamed tab wins over semantic_title
  expect(
    resolveSemanticLabel(
      { address: "t1", semantic_title: "Fix login", program: prog },
      { address: "t1", labels: {} },
      { name: "Release Prep", named: true },
      null,
    ),
  ).toBe("Release Prep");
  // 3. Unnamed tab uses semantic_title instead of boring program name or tab number
  expect(
    resolveSemanticLabel(
      { address: "t1", semantic_title: "修复认证超时", title: "Claude Code", program: prog },
      { address: "t1", labels: {} },
      { name: "2", named: false },
      null,
    ),
  ).toBe("修复认证超时");
});

test("zero-click hover tooltip formats status, live micro_summary, and cwd/branch", () => {
  const tip = buildHoverTooltip(
    {
      address: "a3",
      status: "working",
      semantic_title: "迁移登录模块",
      micro_summary: "已完成 3/5 个文件迁移，正在修复 api_test 编译错误",
      program: { name: "Claude", key: "claude" },
      branch: "feat/auth",
    },
    { address: "a3", cwd: "/home/dev/repo", turn: 3 },
    { isZh: true, now: 181000, since: 1000 },
  );
  const lines = tip.split("\n");
  expect(lines).toHaveLength(3);
  expect(lines[0]).toContain("迁移登录模块");
  expect(lines[0]).toContain("运行中 3m");
  expect(lines[1]).toBe("▸ 已完成 3/5 个文件迁移，正在修复 api_test 编译错误");
  expect(lines[2]).toContain("/home/dev/repo (feat/auth)");
});

test("catch-up briefing clusters similar permissions and merges Drove downstream blockers", () => {
  const local = buildLocalFallbackBriefing({
    isZh: true,
    awayMinutes: 12,
    agents: [
      { address: "a1", acp: true, status: "blocked", semantic_title: "Auth", program: { name: "Claude" } },
      { address: "wsl/a2", acp: true, status: "blocked", semantic_title: "Storage", program: { name: "Codex" } },
      { address: "t1", acp: false, status: "done", semantic_title: "UI 优化", micro_summary: "通过单测", program: { name: "Claude" } },
    ],
    terminals: [
      { address: "a1", acp: true, state: "live", status: "blocked", pending_permission: { kind: "execute", title: "Run cargo test auth" }, labels: {} },
      { address: "wsl/a2", acp: true, state: "live", status: "blocked", pending_permission: { kind: "execute", title: "Run cargo test storage" }, labels: {} },
      { address: "t1", acp: false, state: "live", status: "done", labels: {} },
    ],
  });
  expect(local.permissionClusters).toHaveLength(1);
  expect(local.permissionClusters[0].addresses).toEqual(["a1", "wsl/a2"]);
  expect(local.readyToReview).toHaveLength(1);
  expect(local.headline).toContain("离开约 12 分钟");

  const merged = mergeBriefingSnapshots(local, {
    generatedMs: 2000,
    headline: "核心 schema 失败正阻塞 3 个下游任务；2 个测试审批可一键全批。",
    headlineFromAi: true,
    counts: { critical: 1, needsYou: 2, readyReview: 1, working: 0 },
    criticalBlockers: [
      { id: "drove_task:t_schema", kind: "drove_blocker", title: "schema: Update OpenRPC", summary: "Blocking 3 downstream", status: "error", blockedDownstreamCount: 3 },
    ],
    permissionClusters: local.permissionClusters,
    pendingDecisions: [],
    readyToReview: [],
    workingItems: [],
  });
  expect(merged.headlineFromAi).toBe(true);
  expect(merged.criticalBlockers[0].blockedDownstreamCount).toBe(3);
  expect(merged.readyToReview).toHaveLength(1);
});

test("auto-named tabs (shells, agent aliases, addresses) dynamically follow semantic_title and live title updates", () => {
  const claude = { name: "Claude Code", key: "claude", agent: true };
  const codex = { name: "Codex", key: "codex", agent: true };
  // 1. Shell-named tab (e.g. PowerShell / WSL / Terminal) running Claude follows semantic_title and live title
  expect(
    resolveSemanticLabel(
      { address: "t1", semantic_title: "修复侧边栏名称", title: "Claude Code", program: claude },
      { address: "t1", labels: {} },
      { name: "PowerShell", named: true },
      { terminal: "t1", program: claude, title: "Claude Code" },
    ),
  ).toBe("修复侧边栏名称");
  expect(
    resolveSemanticLabel(
      { address: "t1", title: "Debug websocket", program: claude },
      { address: "t1", labels: {} },
      { name: "WSL", named: true },
      { terminal: "t1", program: claude, title: "Debug websocket" },
    ),
  ).toBe("Debug websocket");
  // Before any semantic_title or telling title arrives, show the running agent name instead of stale shell name
  expect(
    resolveSemanticLabel(
      { address: "t1", title: "~/repo", program: claude },
      { address: "t1", labels: {} },
      { name: "PowerShell", named: true },
      { terminal: "t1", program: claude, title: "~/repo" },
    ),
  ).toBe("Claude Code");
  // 2. Agent-alias or address tab name (e.g. "Claude" vs "Claude Code", or "a12" / "wsl/a12") follows updates
  expect(
    resolveSemanticLabel(
      { address: "wsl/a12", acp: true, semantic_title: "重构渲染管线", program: claude },
      { address: "wsl/a12", labels: {} },
      { name: "Claude", named: true },
      null,
    ),
  ).toBe("重构渲染管线");
  expect(
    resolveSemanticLabel(
      { address: "wsl/a12", acp: true, semantic_title: "重构渲染管线", program: codex },
      { address: "wsl/a12", labels: {} },
      { name: "a12", named: true },
      null,
    ),
  ).toBe("重构渲染管线");
  // 3. Split tab (manyInTab=true) does NOT leak sibling lead.semantic_title onto another agent
  expect(
    resolveSemanticLabel(
      { address: "t2", title: "Second task", program: codex },
      { address: "t2", labels: {} },
      { name: "Split", named: true },
      { terminal: "t1", program: claude, semantic_title: "First task", title: "First task" },
      { manyInTab: true },
    ),
  ).toBe("Second task");
  // 4. Sub-agent auto names (e.g. side-claude, codex) update to semantic_title / live ACP title / custom name
  expect(
    resolveSubAgentLabel(
      { address: "wsl/a50", acp: true, name: "side-claude", semantic_title: "排查内存泄漏", program: claude, labels: {} },
      null,
    ),
  ).toBe("排查内存泄漏");
  expect(
    resolveSubAgentLabel(
      { address: "wsl/a50", acp: true, name: "side-claude", program: claude, labels: {} },
      { terminal: "wsl/a50", acp: true, title: "Live ACP Title", program: claude },
    ),
  ).toBe("Live ACP Title");
  expect(
    resolveSubAgentLabel(
      { address: "wsl/a50", acp: true, name: "side-claude", semantic_title: "排查内存泄漏", program: claude, labels: { "herdr.name": "手动改名" } },
      null,
    ),
  ).toBe("手动改名");
  // 5. PowerShell 7 and live work fallbacks (pending_permission / micro_summary) keep left list synced with top header
  expect(
    resolveSemanticLabel(
      { address: "t5", title: "Claude Code", micro_summary: "正在运行 cargo test", program: claude },
      { address: "t5", labels: {} },
      { name: "PowerShell 7", named: true },
      { terminal: "t5", program: claude, title: "Claude Code" },
    ),
  ).toBe("正在运行 cargo test");
  expect(
    resolveSemanticLabel(
      { address: "t6", title: "Claude Code", program: claude },
      { address: "t6", pending_permission: { title: "Run git status" }, labels: {} },
      { name: "PowerShell 7", named: true },
      { terminal: "t6", program: claude, title: "Claude Code" },
    ),
  ).toBe("Run git status");
  // 6. Stale auto/program herdr.name labels (e.g. "Antigravity" / "Claude Code") do not block real-time semantic_title updates
  const agy = { name: "Antigravity", key: "antigravity", agent: true };
  expect(
    resolveSemanticLabel(
      { address: "wsl/t4j", custom_name: "Antigravity", semantic_title: "修复热更新同步问题", program: agy },
      { address: "wsl/t4j", labels: { "herdr.name": "Antigravity" }, semantic_title: "修复热更新同步问题" },
      { name: "2", named: false },
      { terminal: "wsl/t4j", custom_name: "Antigravity", program: agy, semantic_title: "修复热更新同步问题" },
    ),
  ).toBe("修复热更新同步问题");
  expect(
    resolveSemanticLabel(
      { address: "wsl/t4n", custom_name: "Claude Code", semantic_title: "ZCode终端接入排查", program: claude },
      { address: "wsl/t4n", labels: { "herdr.name": "Claude Code" }, semantic_title: "ZCode终端接入排查" },
      { name: "Claude Code", named: true },
      { terminal: "wsl/t4n", custom_name: "Claude Code", program: claude, semantic_title: "ZCode终端接入排查" },
    ),
  ).toBe("ZCode终端接入排查");
  // Genuine user custom names still win over semantic_title
  expect(
    resolveSemanticLabel(
      { address: "wsl/t4j", custom_name: "主架构助手", semantic_title: "修复热更新同步问题", program: agy },
      { address: "wsl/t4j", labels: { "herdr.name": "主架构助手" }, semantic_title: "修复热更新同步问题" },
      { name: "2", named: false },
      { terminal: "wsl/t4j", custom_name: "主架构助手", program: agy, semantic_title: "修复热更新同步问题" },
    ),
  ).toBe("主架构助手");
});

test("handoff badge in agents list handles live, trashed (restore confirm), and permanently deleted parent", () => {
  const claude = { name: "Claude Code", key: "claude", agent: true };
  const findNode = (node: any, pred: (n: any) => boolean): any => {
    if (!node || typeof node !== "object") return null;
    if (pred(node)) return node;
    for (const c of node.children ?? []) {
      const hit = findNode(c, pred);
      if (hit) return hit;
    }
    return null;
  };
  const collectText = (node: any): string => {
    if (typeof node === "string") return node;
    if (!node || typeof node !== "object") return "";
    return (node.children ?? []).map(collectText).join(" ");
  };

  let curAgents: any[] = [];
  let curTerminals: any[] = [];
  let curPanes: any[] = [];
  let curTrash: any[] = [];
  let currentProps: any = {};
  const overlays: any[] = [];
  const closedOverlays: any[] = [];
  const notices: any[] = [];

  const { agents, writes } = harness([], {
    agents: () => curAgents,
    terminals: () => curTerminals,
    panes: () => curPanes,
    focused: () => null,
    spaces: () => [{ id: "s1", name: "herdr2", tabs: [{ id: "tb1", name: "t4y", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: (k: string) => (k === "agents.trash" ? curTrash : "grouped"),
    theme: () => ({}),
    settings: () => ({ language: "zh-CN" }),
    props: () => currentProps,
    openOverlay: (spec: any) => overlays.push(spec),
    closeOverlay: (id: any) => closedOverlays.push(id),
    notify: (n: any) => notices.push(n),
    setAgentOrder() {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });
  const cx = { stop_propagation() {}, notify() {}, close() {} };

  const childAgent = { address: "t4y", tab: "tb1", space: "s1", space_name: "herdr2", status: "idle", program: claude };
  const childTerm = { address: "t4y", state: "live", labels: {}, lineage: { kind: "handoff", parent: "t4n" } };

  // 1) Parent t4n is live -> normal focus jump
  curAgents = [
    { address: "t4n", tab: "tb0", space: "s1", space_name: "herdr2", status: "idle", program: claude },
    childAgent,
  ];
  curTerminals = [{ address: "t4n", state: "live", labels: {} }, childTerm];
  curPanes = [{ pane: "p1", tab: "tb1", terminal: "t4y", program: claude }];
  curTrash = [];
  currentProps = {};
  let badge = findNode(agents.render(cx).node, (n) => n.id === "ag-kind-t4y");
  expect(badge).toBeTruthy();
  expect(collectText(badge)).toContain("接力 ← t4n");
  badge.click({}, cx);
  expect(writes).toEqual([["agent.focus", { terminal: "t4n" }]]);
  expect(overlays).toHaveLength(0);
  expect(notices).toHaveLength(0);

  // 2) Parent t4n moved to recycle bin (even while still live in terminals with herdr.trashed) -> prompt & ask restore
  writes.length = 0;
  curAgents = [childAgent];
  curTerminals = [
    { address: "t4n", state: "live", labels: { "herdr.trashed": "1795420000000" } },
    childTerm,
  ];
  curTrash = [{ address: "t4n", live: true, program: claude, trashed_at: Date.now() - 5000 }];
  badge = findNode(agents.render(cx).node, (n) => n.id === "ag-kind-t4y");
  badge.click({}, cx);
  expect(writes).toEqual([]);
  expect(notices).toHaveLength(0);
  expect(overlays).toHaveLength(1);
  expect(overlays[0].kind).toBe("dialog");
  expect(overlays[0].props).toMatchObject({ parentRestoreDialog: true, parentAddr: "t4n", shortParent: "t4n", live: true });

  // Render the confirmation dialog: shows exact prompt & asks restore; cancel closes, confirm restores
  currentProps = { ...overlays[0].props, overlay: "ov-parent-restore" };
  const dialogNode = agents.render(cx).node;
  const dialogText = collectText(dialogNode);
  expect(dialogText).toContain("t4n 已移除到回收站");
  expect(dialogText).toContain("是否需要恢复");
  const cancelBtn = findNode(dialogNode, (n) => n.id === "parent-restore-cancel");
  const confirmBtn = findNode(dialogNode, (n) => n.id === "parent-restore-confirm");
  expect(cancelBtn).toBeTruthy();
  expect(confirmBtn).toBeTruthy();
  cancelBtn.click({}, cx);
  expect(writes).toEqual([]);
  expect(closedOverlays).toEqual(["ov-parent-restore"]);
  confirmBtn.click({}, cx);
  expect(writes).toEqual([["agent.restore", { terminal: "t4n" }]]);
  expect(closedOverlays).toEqual(["ov-parent-restore", "ov-parent-restore"]);

  // 3) Parent t4n permanently deleted -> notify "父 agent 已删除" and block jump
  writes.length = 0;
  overlays.length = 0;
  currentProps = {};
  curAgents = [childAgent];
  curTerminals = [childTerm];
  curTrash = [];
  badge = findNode(agents.render(cx).node, (n) => n.id === "ag-kind-t4y");
  badge.click({}, cx);
  expect(writes).toEqual([]);
  expect(overlays).toHaveLength(0);
  expect(notices).toHaveLength(1);
  expect(notices[0].title).toBe("父 agent 已删除");
  expect(notices[0].level).toBe("warning");
});

test("grouped mode orders space groups strictly by spaces() order without splitting spaces", () => {
  const orders: string[][] = [];
  const { agents } = harness([], {
    agents: () => [
      { address: "wsl/a2", space: "wsl/sp2", space_name: "herdr2", tab: "tb2", status: "idle", program: { name: "ZCode" } },
      { address: "wsl/a1", space: "wsl/sp1", space_name: "Game", tab: "tb1", status: "idle", program: { name: "Claude" } },
      { address: "wsl/a3", space: "wsl/sp2", space_name: "herdr2", tab: "tb3", status: "idle", program: { name: "Claude" } },
    ],
    terminals: () => [],
    panes: () => [],
    focused: () => ({}),
    spaces: () => [
      { id: "wsl/sp1", name: "Game", tabs: [] },
      { id: "wsl/sp2", name: "herdr2", tabs: [] },
    ],
    shared: (k: string) => (k === "agents.sort" ? "grouped" : undefined),
    setShared: () => {},
    setAgentOrder: (o: string[]) => orders.push(o),
    theme: () => ({}),
    window: () => ({ platform: "windows" }),
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    settings: () => ({ language: "zh-CN" }),
    keys: () => [],
    openOverlay: () => {},
    closeOverlay: () => {},
    notify: () => {},
    h2: () => Promise.resolve({ code: 0, stdout: "{}" }),
    div: () => element(),
    v_flex: () => element(),
    h_flex: () => element(),
    svg: () => element(),
    image: () => element(),
    Input: { new: () => element() },
    InputState: class {},
    Popup: { new: () => element() },
  });
  const root = agents.render({}).node;
  const headerIds: string[] = [];
  const walk = (n: any) => {
    if (typeof n?.id === "string" && n.id.startsWith("sp-")) headerIds.push(n.id);
    (n?.children ?? []).forEach(walk);
  };
  walk(root);
  expect(headerIds).toEqual(["sp-wsl/sp1", "sp-wsl/sp2"]);
  expect(orders.at(-1)).toEqual(["wsl/a1", "wsl/a2", "wsl/a3"]);
});

test("sub-agent rows display live and completed execution time and swap to remove on hover", () => {
  const now = Date.now();
  const claude = { key: "claude", name: "Claude", agent: true };
  const list = [
    { address: "t1", space: "s1", space_name: "Project", tab: "tab1", pane: "p1", status: "idle", title: "Parent", program: claude },
  ];
  const terms = [
    { address: "t1", parent: null, state: "live", status: "idle", program: claude, labels: {} },
    {
      address: "a-sub-run",
      acp: true,
      parent: "t1",
      state: "live",
      status: "working",
      name: "worker-running",
      program: claude,
      working_since: now - 65_000,
      total_work_ms: 65_000,
      labels: {},
    },
    {
      address: "a-sub-done",
      acp: true,
      parent: "t1",
      state: "live",
      status: "idle",
      name: "worker-done",
      program: claude,
      working_since: null,
      work_ms: 42_000,
      total_work_ms: 90_000,
      labels: {},
    },
  ];
  const { agents, ticks } = harness([], {
    agents: () => list,
    terminals: () => terms,
    panes: () => [{ pane: "p1", tab: "tab1", terminal: "t1", program: claude, title: "Parent" }],
    focused: () => null,
    spaces: () => [{ id: "s1", name: "Project", tabs: [{ id: "tab1", name: "Parent", named: true }] }],
    layout: () => ({ id: "sidebar", size: { px: 268 } }),
    shared: () => "grouped",
    theme: () => ({}),
    setAgentOrder: () => {},
    div: element, v_flex: element, h_flex: element, svg: element, image: element,
  });

  const collectTexts = (node: any, out = new Map<string, string>()): Map<string, string> => {
    if (typeof node?.id === "string") {
      const str = (n: any): string => (typeof n === "string" ? n : (n?.children ?? []).map(str).join(""));
      out.set(node.id, str(node));
    }
    for (const c of node?.children ?? []) collectTexts(c, out);
    return out;
  };

  const rendered = agents.render({});
  const texts = collectTexts(rendered.node);
  expect(texts.get("sub-time-a-sub-run")).toMatch(/^1:0[5-9]$/);
  expect(texts.get("sub-time-a-sub-done")).toBe("0:42");

  // Hovering a sub-agent replaces its elapsed time badge with the remove button cleanly.
  agents.hoveredSub = "a-sub-done";
  const hoveredTexts = collectTexts(agents.render({}).node);
  expect(hoveredTexts.has("sub-time-a-sub-done")).toBe(false);
  expect(hoveredTexts.has("rm-sub-a-sub-done")).toBe(true);

  // 1-second timer notifies when a headless sub-agent is working even if parent agents are idle.
  let notified = 0;
  const tick1s = ticks.find((t: any) => t.ms === 1000);
  expect(tick1s).toBeDefined();
  tick1s.fn({ notify: () => { notified += 1; } });
  expect(notified).toBe(1);
});
