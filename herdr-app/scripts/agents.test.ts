import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";

const source = (await readFile(new URL("../defaults/mods/agents/main.js", import.meta.url), "utf8"))
  .replace(/^import .*;\n/gm, "").replace("export default class Agents", "class Agents");

function harness(pins: string[], globals: Record<string, unknown> = {}) {
  const data = new Map([["pins", JSON.stringify(pins)]]);
  const writes: unknown[] = [];
  const storage = { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => data.set(k, v), removeItem: (k: string) => data.delete(k) };
  const Agents = new Function("View", "localStorage", "props", "run", ...Object.keys(globals), `${source}; return Agents;`)(
    class {}, storage, () => ({}), (...args: unknown[]) => writes.push(args),
    ...Object.values(globals),
  );
  const agents = new Agents();
  agents.init({}, { timer: { every() {} } });
  return { agents, data, writes };
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
  expect(texts.get("ag-t1")).not.toContain("t1");
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
    if (node?.id?.startsWith("ag-t")) fills.set(node.id, node.bg);
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

for (const sort of ["priority", "grouped"]) test(`${sort}: headless ACP sub-agents nest and roll up urgency, and standalone ACP rows open terminal.peek`, () => {
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
    ["terminal.peek", { terminal: "a7" }],
    ["terminal.peek", { terminal: "wsl/a6" }],
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
