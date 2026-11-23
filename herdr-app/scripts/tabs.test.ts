import { afterAll, beforeEach, expect, setSystemTime, test } from "bun:test";
import { readFile } from "node:fs/promises";

// Exercise the shipped tab bar's wheel handler without a window or a core.
type Handler = (event: unknown) => void;
class Element {
  nodes: Element[] = [];
  calls: Record<string, unknown[]> = {};
  constructor() {
    const proxy: Element = new Proxy(this, {
      get: (target, key: string) => key in target ? Reflect.get(target, key) : (...args: unknown[]) => {
        if (key === "child") target.nodes.push(args[0] as Element);
        else if (key === "children") target.nodes.push(...(args[0] as Element[]));
        else if (key === "when") { if (args[0]) (args[1] as (el: Element) => void)(proxy); }
        else {
          target.calls[key] = args;
          if (typeof args[0] === "string") target.calls[`${key}:${args[0]}`] = args;
        }
        return proxy;
      },
    });
    return proxy;
  }
}
const source = await readFile(new URL("../defaults/mods/tabs/main.js", import.meta.url), "utf8");
const element = () => new Element();
const commands: string[] = [];
const store: Record<string, string> = {};
const values: Record<string, unknown> = {
  View: class {}, div: element, svg: element, image: element, h_flex: element,
  ScrollHandle: { new: () => ({ scroll_to_item() {} }) },
  spaces: () => [{ id: "s1", active: true, tabs: [{ id: "t1", name: "a" }, { id: "t2", name: "b", active: true }, { id: "t3", name: "c" }] }],
  panes: () => [], run(command: string) { commands.push(command); }, openOverlay() {}, theme: () => ({}),
  window: () => null, shared: () => null, layout: () => null, settings: () => ({ zoom: 1 }),
  localStorage: { getItem: (key: string) => store[key] ?? null, setItem(key: string, value: string) { store[key] = value; } },
};
const Tabs = new Function(...Object.keys(values), source.replace(/^import .*;\n/gm, "").replace("export default class Tabs", "return class Tabs"))(...Object.values(values));

let now = 1_000_000;
function wheel() {
  const view = new Tabs();
  view.init();
  // The mod is plain JS: its handler's shape is the documented ScrollWheelEvent listener.
  const handler = (view.render() as Element).calls.on_scroll_wheel[0] as Handler;
  return (y: number, touch_phase = "moved", lines?: number, ms = 8) => {
    now += ms;
    setSystemTime(new Date(now));
    handler({ delta: { x: 0, y }, delta_lines: lines === undefined ? undefined : { x: 0, y: lines }, touch_phase });
  };
}
beforeEach(() => { commands.length = 0; });
// Bun runs every test file in one process: give the later ones the real clock.
afterAll(() => setSystemTime());

test("a short touchpad swipe switches one tab", () => {
  const send = wheel();
  for (let i = 0; i < 12; i++) send(-6);
  expect(commands).toEqual(["tab.next"]);
});

test("a nudge below the threshold switches nothing", () => {
  const send = wheel();
  for (let i = 0; i < 3; i++) send(-5);
  expect(commands).toEqual([]);
});

test("each notch of a smooth-scrolling mouse switches one tab", () => {
  const send = wheel();
  // macOS smooths one notch into about 28 events of 1 px.
  for (let i = 0; i < 28; i++) send(-1, "moved", undefined, 10);
  send(-1, "moved", undefined, 700);
  for (let i = 0; i < 27; i++) send(-1, "moved", undefined, 10);
  expect(commands).toEqual(["tab.next", "tab.next"]);
});

test("a long swipe keeps switching, a tab per 60 px", () => {
  const send = wheel();
  // 20 px for the first tab, then 60 px each: 400 px is seven tabs.
  for (let i = 0; i < 40; i++) send(10, "moved", undefined, 40);
  expect(commands).toEqual(Array(7).fill("tab.prev"));
});

test("a fast flick shows each tab it passes for at least 100 ms", () => {
  const send = wheel();
  // 1200 px within 240 ms: enough for twenty tabs, time for three.
  for (let i = 0; i < 30; i++) send(-40);
  expect(commands).toEqual(["tab.next", "tab.next", "tab.next"]);
});

test("a pause or a turn starts a new gesture", () => {
  const send = wheel();
  for (let i = 0; i < 12; i++) send(-6);
  send(-30, "moved", undefined, 400);
  for (let i = 0; i < 4; i++) send(-5);
  for (let i = 0; i < 12; i++) send(6, "moved", undefined, 20);
  expect(commands).toEqual(["tab.next", "tab.next", "tab.prev"]);
});

test("each wheel notch switches one tab", () => {
  const send = wheel();
  send(-20, "moved", -1);
  send(-20, "moved", -1);
  send(2, "moved", 0.1);
  expect(commands).toEqual(["tab.next", "tab.next", "tab.prev"]);
});

const label = new Function("View", `${source.replace(/^import .*;\n/gm, "").replace("export default class Tabs", "class Tabs")}; return label;`)(class {});

const claude = { program: { name: "Claude Code", key: "claude", agent: true }, title: "my-task" };

test("a tab Herdr named after its agent follows the agent's session title", () => {
  expect(label({ name: "Claude Code", named: true }, claude)).toBe("my-task");
  expect(label({ name: "", named: false }, claude)).toBe("my-task");
  expect(label({ name: "", named: false }, { ...claude, title: "✦ [web] login" })).toBe("[web] login");
  expect(label({ name: "Claude Code", named: true }, { ...claude, title: "renamed-task" })).toBe("renamed-task");
});

test("an empty OSC title published as the terminal address keeps the program label", () => {
  for (const terminal of ["t2", "remote/t2"]) {
    const lead = { ...claude, terminal, title: terminal };
    expect(label({ name: "2", named: false }, lead)).toBe("Claude Code");
    expect(label({ name: "Claude Code", named: true }, lead)).toBe("Claude Code");
    expect(label({ name: "fix login", named: true }, lead)).toBe("fix login");
    expect(label({ name: "2", named: false }, { ...lead, title: "renamed-task" })).toBe("renamed-task");
  }
});

test("a name the person gave stays", () => {
  expect(label({ name: "fix login", named: true }, claude)).toBe("fix login");
});

test("a title that says nothing new keeps the tab's own name", () => {
  expect(label({ name: "Claude Code", named: true }, { ...claude, title: "Claude Code" })).toBe("Claude Code");
  expect(label({ name: "Claude Code", named: true }, { ...claude, title: "me@build-box" })).toBe("Claude Code");
  expect(label({ name: "", named: false }, { ...claude, title: "" })).toBe("Claude Code");
  expect(label({ name: "", named: false }, { ...claude, title: "~/src/api" })).toBe("Claude Code");
  expect(label({ name: "", named: false }, { ...claude, title: "✦ /tmp" })).toBe("Claude Code");
});

test("a shell's title never replaces its tab's name", () => {
  const shell = { program: { name: "zsh", key: "zsh" }, title: "vim notes.md" };
  expect(label({ name: "", named: false }, shell)).toBe("zsh");
  expect(label({ name: "notes", named: true }, shell)).toBe("notes");
  expect(label({ name: "", named: false }, undefined)).toBe("Terminal");
});

test("clicking vs dragging tabs floats active tab with zoom-aware 1:1 tracking, spring settle, and type badges", async () => {
  const runCalls: unknown[][] = [];
  let currentZoom = 1.2;
  let currentLang = "en";
  let currentTabs = [{ id: "t1", name: "a" }, { id: "t2", name: "b", active: true }, { id: "t3", name: "c" }];
  const currentPanes = [
    { tab: "t1", acp: true, program: { name: "Claude Code", key: "claude", agent: true } },
    { tab: "t2", program: { name: "Claude Code", key: "claude", agent: true } },
    { tab: "t3", program: { name: "zsh", key: "zsh" } },
  ];
  const view = new (new Function(...Object.keys(values), source.replace(/^import .*;\n/gm, "").replace("export default class Tabs", "return class Tabs"))(
    ...Object.values({
      ...values,
      spaces: () => [{ id: "s1", active: true, tabs: currentTabs }],
      panes: () => currentPanes,
      settings: () => ({ zoom: currentZoom, language: currentLang }),
      run: (cmd: string, args: any) => {
        runCalls.push([cmd, args]);
        if (cmd === "tab.move" && args?.id === "t1" && args?.index === 2) {
          currentTabs = [currentTabs[1], currentTabs[0], currentTabs[2]];
        }
      },
    }),
  ))();
  view.init();
  const collectTexts = (node: any): string[] => {
    if (!node) return [];
    const out: string[] = [];
    for (const ch of node.nodes ?? []) {
      if (typeof ch === "string") out.push(ch);
      else out.push(...collectTexts(ch));
    }
    return out;
  };
  const findById = (node: any, id: string): any => {
    if (!node) return null;
    if (node.calls?.id?.[0] === id) return node;
    for (const ch of node.nodes ?? []) {
      const hit = findById(ch, id);
      if (hit) return hit;
    }
    return null;
  };

  let releaseSleep: (() => void) | null = null;
  const tasks: Promise<unknown>[] = [];
  const asyncCx = {
    stop_propagation() {},
    notify() {},
    spawn(fn: (acx: any) => Promise<unknown>) {
      const p = fn({
        sleep: () => new Promise<void>((res) => { releaseSleep = res; }),
        notify() {},
      });
      tasks.push(p);
      return { cancel() {} };
    },
  };

  // Tab pills distinguish ACP vs Terminal Agent (CLI in English, 终端 in Simplified Chinese) vs plain shell.
  const root1 = view.render();
  expect(collectTexts(findById(root1, "tab-t1"))).toContain("ACP");
  expect(collectTexts(findById(root1, "tab-t2"))).toContain("CLI");
  expect(collectTexts(findById(root1, "tab-t3"))).not.toContain("ACP");
  expect(collectTexts(findById(root1, "tab-t3"))).not.toContain("CLI");
  currentLang = "zh-CN";
  expect(collectTexts(findById(view.render(), "tab-t2"))).toContain("终端");
  currentLang = "en";

  // Click without drag selects the tab.
  const tab1 = findById(root1, "tab-t1");
  tab1.calls["on_mouse_down:left"][1]({ click_count: 1, position: { x: 24 }, bounds: { width: 120 } }, asyncCx);
  tab1.calls["on_mouse_up:left"][1]({}, asyncCx);
  expect(runCalls).toEqual([["tab.select", { id: "t1" }]]);

  runCalls.length = 0;
  // Measure raw zoomed widths (120 raw px / 1.2 zoom = 100 logical px) and drag t1 past t2.
  findById(root1, "tab-t2").calls.on_size_change[0]({ width: 120 }, asyncCx);
  findById(root1, "tab-t3").calls.on_size_change[0]({ width: 120 }, asyncCx);
  tab1.calls["on_mouse_down:left"][1]({ click_count: 1, position: { x: 24 }, bounds: { width: 120 } }, asyncCx);
  // Move +180 raw px -> 150 logical px (past t2's merge zone into the reorder slot).
  root1.calls.on_mouse_move[0]({ position: { x: 204 }, pressed_button: "left" }, asyncCx);

  const draggingRoot = view.render();
  const placeholderTab1 = findById(draggingRoot, "tab-t1");
  const floatingTab1 = findById(draggingRoot, "tab-float-t1");
  const shiftedTab2 = findById(draggingRoot, "tab-t2");
  const shield = findById(draggingRoot, "tab-drag-shield");
  expect(placeholderTab1.calls.opacity?.[0]).toBe(0);
  expect(floatingTab1.calls.left[0]).toBe(150);
  expect(floatingTab1.calls.spring?.[1]?.response).toBe(0);
  expect(shiftedTab2.calls.left[0]).toBe(-103);
  expect(shiftedTab2.calls.spring?.[0]).toBe("left");
  expect(shiftedTab2.calls.spring?.[1]?.response).toBe(165);
  expect(shield).not.toBeNull();

  // Releasing enters the smooth settle spring (160ms response to destination X 103) before committing tab.move.
  shield.calls["on_mouse_up:left"][1]({}, asyncCx);
  expect(runCalls).toEqual([]);
  const settlingRoot = view.render();
  const settlingFloat = findById(settlingRoot, "tab-float-t1");
  expect(settlingFloat.calls.left[0]).toBe(103);
  expect(settlingFloat.calls.spring?.[1]?.response).toBe(160);
  expect(findById(settlingRoot, "tab-t2").calls.left[0]).toBe(-103);

  releaseSleep?.();
  await Promise.all(tasks);
  expect(runCalls).toEqual([
    ["tab.move", { id: "t1", index: 2 }],
    ["tab.select", { id: "t1" }],
  ]);

  // First render after array reorder snaps neighbors at response: 0 so there is zero double-slide seam.
  const settledRoot = view.render();
  expect(findById(settledRoot, "tab-t2").calls.left[0]).toBe(0);
  expect(findById(settledRoot, "tab-t2").calls.spring?.[1]?.response).toBe(0);

  // Dragging onto another tab's center highlights the target without shifting it and merges on release.
  runCalls.length = 0;
  const beforeMergeRoot = view.render();
  const tab2Now = findById(beforeMergeRoot, "tab-t2");
  tab2Now.calls["on_mouse_down:left"][1]({ click_count: 1, position: { x: 24 }, bounds: { width: 120 } }, asyncCx);
  // Drag t2 (+103 logical px -> +124 raw px, center = 153) squarely over t1's center [103..203].
  beforeMergeRoot.calls.on_mouse_move[0]({ position: { x: 148 }, pressed_button: "left" }, asyncCx);
  currentLang = "zh-CN";
  const mergePreviewRoot = view.render();
  expect(findById(mergePreviewRoot, "tab-t1").calls.left[0]).toBe(0);
  expect(collectTexts(findById(mergePreviewRoot, "tab-merge-badge"))).toContain("◫ 分栏");
  currentLang = "en";
  expect(collectTexts(findById(view.render(), "tab-merge-badge"))).toContain("◫ Split");
  findById(mergePreviewRoot, "tab-drag-shield").calls["on_mouse_up:left"][1]({}, asyncCx);
  expect(runCalls).toEqual([["tab.merge", { source: "t2", target: "t1", direction: "right" }]]);

  // Two-tab strip: holding over neighbor merges; pushing past neighbor still reorders.
  runCalls.length = 0;
  currentTabs = [{ id: "t1", name: "a", active: true }, { id: "t2", name: "b" }];
  const twoTabRoot = view.render();
  findById(twoTabRoot, "tab-t1").calls["on_mouse_down:left"][1]({ click_count: 1, position: { x: 24 }, bounds: { width: 120 } }, asyncCx);
  // Push +192 raw px -> +160 logical px (clamped at 103+58 = 161, center = 210 > 183).
  twoTabRoot.calls.on_mouse_move[0]({ position: { x: 216 }, pressed_button: "left" }, asyncCx);
  const twoReorderRoot = view.render();
  expect(findById(twoReorderRoot, "tab-merge-badge")).toBeNull();
  expect(findById(twoReorderRoot, "tab-t2").calls.left[0]).toBe(-103);
  findById(twoReorderRoot, "tab-drag-shield").calls["on_mouse_up:left"][1]({}, asyncCx);
  releaseSleep?.();
  await Promise.all(tasks);
  expect(runCalls).toEqual([["tab.move", { id: "t1", index: 2 }]]);
});

test("pane header maximize button breaks out split pane into a standalone tab via pane.detach", async () => {
  const paneSource = await readFile(new URL("../defaults/mods/pane/main.js", import.meta.url), "utf8");
  let currentPanes: Array<Record<string, unknown>> = [
    { pane: "p1", tab: "tb1", terminal: "t1", title: "zsh", status: "idle", zoomed: false, focused: true },
  ];
  let currentProps = { pane: "p1", terminal: "t1" };
  const paneRunCalls: Array<[string, Record<string, unknown>]> = [];
  const paneValues: Record<string, unknown> = {
    View: class {},
    div: element,
    svg: element,
    image: element,
    h_flex: element,
    InputState: { new: () => ({ on() {}, value: () => "", set_value() {}, set_selected_range() {}, focus() {} }) },
    Input: { new: () => element() },
    panes: () => currentPanes,
    terminals: () => [{ address: "t1", name: "zsh", labels: {} }, { address: "t2", acp: true, name: "codex-news", labels: {} }],
    props: () => currentProps,
    run(command: string, args: Record<string, unknown>) { paneRunCalls.push([command, args]); },
    h2() {},
    openOverlay() {},
    theme: () => ({}),
    home: () => "/home/me",
    layout: () => null,
    settings: () => ({ language: "zh-CN" }),
  };
  const Pane = new Function(
    ...Object.keys(paneValues),
    paneSource.replace(/^import .*;\n/gm, "").replace("export default class Pane", "return class Pane"),
  )(...Object.values(paneValues));

  const findNodeById = (node: Element | null | undefined, id: string): Element | null => {
    if (!node) return null;
    if (node.calls.id?.[0] === id) return node;
    for (const child of node.nodes) {
      if (child && typeof child === "object" && Array.isArray(child.nodes)) {
        const hit = findNodeById(child, id);
        if (hit) return hit;
      }
    }
    return null;
  };

  const view = new Pane();
  view.init();
  // Hover a single-pane tab header: no #ph-z button (cannot detach/zoom a lone pane), but shows #ph-view toggle.
  const singleRoot = view.render({});
  singleRoot.calls.on_hover[0](true, { notify() {} });
  const hoveredSingle = view.render({});
  expect(findNodeById(hoveredSingle, "ph-z-p1")).toBeNull();
  const viewBtn = findNodeById(hoveredSingle, "ph-view-p1");
  expect(viewBtn).not.toBeNull();
  viewBtn.calls.on_click[0]({}, { stop_propagation() {} });
  expect(paneRunCalls).toEqual([["terminal.view", { terminal: "t1", view: "toggle" }]]);
  paneRunCalls.length = 0;

  // Merged / split tab with two panes: shows #ph-z-p2 and clicking it runs pane.detach.
  currentPanes = [
    { pane: "p1", tab: "tb1", terminal: "t1", title: "zsh", status: "idle", zoomed: false, focused: false },
    { pane: "p2", tab: "tb1", terminal: "a1", acp: true, title: "codex-news", status: "idle", zoomed: false, focused: true },
  ];
  currentProps = { pane: "p2", terminal: "a1" };
  const mergedRoot = view.render({});
  const zoomBtn = findNodeById(mergedRoot, "ph-z-p2");
  expect(zoomBtn).not.toBeNull();
  zoomBtn.calls.on_click[0]({}, { stop_propagation() {} });
  expect(paneRunCalls).toEqual([["pane.detach", { pane: "p2" }]]);
});

test("tabs and pane headers show agent type (ACP / Terminal) and live/paused execution duration", async () => {
  const paneSource = await readFile(new URL("../defaults/mods/pane/main.js", import.meta.url), "utf8");
  const collectTexts = (node: any): string[] => {
    if (!node) return [];
    const out: string[] = [];
    for (const ch of node.nodes ?? []) {
      if (typeof ch === "string") out.push(ch);
      else out.push(...collectTexts(ch));
    }
    return out;
  };
  const findById = (node: any, id: string): any => {
    if (!node) return null;
    if (node.calls?.id?.[0] === id) return node;
    for (const ch of node.nodes ?? []) {
      const hit = findById(ch, id);
      if (hit) return hit;
    }
    return null;
  };

  let currentLang = "en";
  let tNow = 10_000_000;
  setSystemTime(new Date(tNow));

  let currentTabs = [
    { id: "tb-acp", name: "Codex", active: true, status: "working", working_since: tNow - 65_000 },
    { id: "tb-cli", name: "Claude", status: "done", work_ms: 12_000, total_work_ms: 45_000 },
    { id: "tb-mix", name: "Mix", status: "working", working_since: tNow - 5_000 },
    { id: "tb-sh", name: "Shell" },
  ];
  let currentPanes: Array<Record<string, unknown>> = [
    { pane: "p-acp", tab: "tb-acp", terminal: "a1", acp: true, title: "Codex", status: "working", working_since: tNow - 65_000, program: { name: "Codex", key: "codex", agent: true } },
    { pane: "p-cli", tab: "tb-cli", terminal: "t1", title: "Claude", status: "done", work_ms: 12_000, total_work_ms: 45_000, program: { name: "Claude Code", key: "claude", agent: true } },
    { pane: "p-mix-1", tab: "tb-mix", terminal: "a2", acp: true, title: "A2", status: "idle", work_ms: 3_000, program: { name: "Codex", key: "codex", agent: true } },
    { pane: "p-mix-2", tab: "tb-mix", terminal: "t2", structured: true, title: "T2", status: "working", working_since: tNow - 5_000, program: { name: "Claude Code", key: "claude", agent: true } },
    { pane: "p-sh", tab: "tb-sh", terminal: "t3", title: "zsh", status: null, program: { name: "zsh", key: "zsh", shell: true, agent: false } },
  ];

  const tabsCtor = new Function(...Object.keys(values), source.replace(/^import .*;\n/gm, "").replace("export default class Tabs", "return class Tabs"))(
    ...Object.values({
      ...values,
      spaces: () => [{ id: "s1", active: true, tabs: currentTabs }],
      panes: () => currentPanes,
      settings: () => ({ zoom: 1, language: currentLang }),
    }),
  );
  let timerCb: ((cx: any) => void) | null = null;
  const tabsView = new tabsCtor();
  tabsView.init({}, { timer: { every: (_ms: number, fn: (cx: any) => void) => { timerCb = fn; } } });

  const tree1 = tabsView.render();
  expect(collectTexts(findById(tree1, "tab-tb-acp"))).toContain("ACP");
  expect(collectTexts(findById(tree1, "tab-time-tb-acp"))).toContain("1:05");
  expect(collectTexts(findById(tree1, "tab-tb-cli"))).toContain("CLI");
  expect(collectTexts(findById(tree1, "tab-time-tb-cli"))).toContain("0:12");
  // Merged tab with both ACP and Terminal CLI agents shows both badges and active elapsed time.
  const mixTexts = collectTexts(findById(tree1, "tab-tb-mix"));
  expect(mixTexts).toContain("ACP");
  expect(mixTexts).toContain("CLI");
  expect(collectTexts(findById(tree1, "tab-time-tb-mix"))).toContain("0:05");
  // Plain shell tab shows neither agent badge nor execution timer.
  const shTexts = collectTexts(findById(tree1, "tab-tb-sh"));
  expect(shTexts).not.toContain("ACP");
  expect(shTexts).not.toContain("CLI");
  expect(findById(tree1, "tab-time-tb-sh")).toBeNull();

  // Advance clock by 4 seconds and verify working timer ticks live and zh-CN switches CLI -> 终端.
  tNow += 4_000;
  setSystemTime(new Date(tNow));
  let timerNotified = 0;
  timerCb?.({ notify: () => { timerNotified++; } });
  expect(timerNotified).toBe(1);
  currentLang = "zh-CN";
  const tree2 = tabsView.render();
  expect(collectTexts(findById(tree2, "tab-time-tb-acp"))).toContain("1:09");
  expect(collectTexts(findById(tree2, "tab-tb-cli"))).toContain("终端");
  expect(collectTexts(findById(tree2, "tab-time-tb-cli"))).toContain("0:12");
  expect(collectTexts(findById(tree2, "tab-tb-mix"))).toContain("终端");
  expect(collectTexts(findById(tree2, "tab-time-tb-mix"))).toContain("0:09");

  // Pane header tests across ACP lone tab, Terminal agent pane, and plain shell pane.
  let activePaneProp = { pane: "p-acp", terminal: "a1" };
  const paneValues: Record<string, unknown> = {
    View: class {},
    div: element,
    svg: element,
    image: element,
    h_flex: element,
    InputState: { new: () => ({ on() {}, value: () => "", set_value() {}, set_selected_range() {}, focus() {} }) },
    Input: { new: () => element() },
    panes: () => currentPanes,
    terminals: () => [],
    props: () => activePaneProp,
    run() {},
    h2() {},
    openOverlay() {},
    theme: () => ({}),
    home: () => "/home/me",
    layout: () => null,
    settings: () => ({ language: currentLang }),
  };
  const PaneCtor = new Function(
    ...Object.keys(paneValues),
    paneSource.replace(/^import .*;\n/gm, "").replace("export default class Pane", "return class Pane"),
  )(...Object.values(paneValues));
  const paneView = new PaneCtor();
  let paneTimerCb: ((cx: any) => void) | null = null;
  paneView.init({}, { timer: { every: (_ms: number, fn: (cx: any) => void) => { paneTimerCb = fn; } } });

  // Lone ACP pane shows ACP badge and live timer (zh-CN).
  const acpPaneTree = paneView.render({});
  expect(collectTexts(findById(acpPaneTree, "ph-type-p-acp"))).toContain("ACP");
  expect(collectTexts(findById(acpPaneTree, "ph-time-p-acp"))).toContain("1:09");

  let paneNotified = 0;
  paneTimerCb?.({ notify: () => { paneNotified++; } });
  expect(paneNotified).toBe(1);

  // Terminal agent pane shows 终端 (zh-CN) / CLI (en) and preserved duration 0:12.
  activePaneProp = { pane: "p-cli", terminal: "t1" };
  const cliPaneZh = paneView.render({});
  expect(collectTexts(findById(cliPaneZh, "ph-type-p-cli"))).toContain("终端");
  expect(collectTexts(findById(cliPaneZh, "ph-time-p-cli"))).toContain("0:12");

  currentLang = "en";
  const cliPaneEn = paneView.render({});
  expect(collectTexts(findById(cliPaneEn, "ph-type-p-cli"))).toContain("CLI");
  expect(collectTexts(findById(cliPaneEn, "ph-time-p-cli"))).toContain("0:12");

  // Plain shell pane omits both ph-type and ph-time.
  activePaneProp = { pane: "p-sh", terminal: "t3" };
  const shPaneTree = paneView.render({});
  expect(findById(shPaneTree, "ph-type-p-sh")).toBeNull();
  expect(findById(shPaneTree, "ph-time-p-sh")).toBeNull();
});

test("panel buttons render files, changes, and checkpoints side-by-side including legacy saved prefs", () => {
  const sharedMap: Record<string, unknown> = {};
  const runCalls: unknown[][] = [];
  let mockPanes: any[] = [{ pane: "p1", tab: "tab-1", terminal: "wsl/t2t", focused: true }];
  let mockTerms: any[] = [
    { address: "wsl/t2t", state: "live", labels: {} },
    { address: "wsl/a4h", state: "live", parent: "wsl/t2t", lineage: { kind: "side-chat", parent: "wsl/t2t" }, labels: {} },
  ];
  const makeTabs = (lang = "zh-CN") => {
    const env = {
      ...values,
      panes: () => mockPanes,
      terminals: () => mockTerms,
      agents: () => [],
      focused: () => ({ terminal: mockPanes[0]?.terminal ?? null }),
      shared: (k: string) => (k in sharedMap ? sharedMap[k] : null),
      settings: () => ({ zoom: 1, language: lang }),
      layout: () => ({
        id: "root",
        children: [
          { id: "panel", hidden: false },
          { id: "panel-body", active: "checkpoints" },
        ],
      }),
      run: (cmd: string, args: unknown) => {
        runCalls.push([cmd, args]);
      },
    };
    const view = new (new Function(...Object.keys(env), source.replace(/^import .*;\n/gm, "").replace("export default class Tabs", "return class Tabs"))(
      ...Object.values(env),
    ))();
    view.init();
    return view;
  };
  const findById = (node: any, id: string): any => {
    if (!node) return null;
    if (node.calls?.id?.[0] === id) return node;
    for (const ch of node.nodes ?? []) {
      const hit = findById(ch, id);
      if (hit) return hit;
    }
    return null;
  };
  const collectTexts = (node: any): string[] => {
    if (!node) return [];
    const out: string[] = [];
    for (const ch of node.nodes ?? []) {
      if (typeof ch === "string") out.push(ch);
      else out.push(...collectTexts(ch));
    }
    return out;
  };

  // Focused agent wsl/t2t has a live side-chat: all 4 panel tabs show side-by-side.
  const defaultTree = makeTabs("zh-CN").render();
  expect(collectTexts(findById(defaultTree, "panel-files"))).toContain("文件");
  expect(collectTexts(findById(defaultTree, "panel-changes"))).toContain("变更");
  expect(collectTexts(findById(defaultTree, "panel-checkpoints"))).toContain("快照");
  expect(collectTexts(findById(defaultTree, "panel-side-chat"))).toContain("旁路");

  // Switching focus to an agent without side-chats hides the side-chat panel tab automatically.
  mockPanes = [{ pane: "p2", tab: "tab-2", terminal: "wsl/t99", focused: true }];
  const noSideTree = makeTabs("zh-CN").render();
  expect(findById(noSideTree, "panel-side-chat")).toBeNull();
  mockPanes = [{ pane: "p1", tab: "tab-1", terminal: "wsl/t2t", focused: true }];

  // Legacy saved ["files", "changes"] without workspace.panels_has_checkpoints still reveals Checkpoints.
  sharedMap["workspace.panels"] = ["files", "changes"];
  const legacyTree = makeTabs("en").render();
  expect(collectTexts(findById(legacyTree, "panel-checkpoints"))).toContain("Checkpoints");

  // Explicitly disabled checkpoints after migration flag is respected.
  sharedMap["workspace.panels_has_checkpoints"] = true;
  sharedMap["workspace.panels"] = ["files", "changes"];
  const disabledTree = makeTabs("zh-CN").render();
  expect(findById(disabledTree, "panel-files")).not.toBeNull();
  expect(findById(disabledTree, "panel-changes")).not.toBeNull();
  expect(findById(disabledTree, "panel-checkpoints")).toBeNull();
});

test("pane header shows agent ID for manual terminal agents and clickable upstream link for handoff agents", async () => {
  const paneSource = await readFile("defaults/mods/pane/main.js", "utf8");
  const claude = { key: "claude", name: "Claude", agent: true, shell: false, icon: null, tint: true };
  const codex = { key: "codex", name: "Codex", agent: true, shell: false, icon: null, tint: true };
  const shell = { key: "shell", name: "zsh", agent: false, shell: true, icon: null, tint: true };
  const runCalls: unknown[][] = [];
  const paneList = [
    { pane: "p-cli", tab: "tab-1", terminal: "wsl/t2t", title: "Claude", cwd: "/repo", status: "idle", program: claude, structured: false, focused: true },
    { pane: "p-ho", tab: "tab-2", acp: true, terminal: "wsl/a49", title: "Codex", cwd: "/repo", status: "working", program: codex, focused: false },
    { pane: "p-sh", tab: "tab-3", terminal: "wsl/t99", title: "zsh", cwd: "/repo", status: null, program: shell, focused: false },
  ];
  const termList = [
    { address: "wsl/t2t", parent: null, state: "live", status: "idle", program: claude, labels: {} },
    { address: "wsl/a49", acp: true, parent: "wsl/t2t", lineage: { kind: "fork", parent: "wsl/t2t", worktree_path: "/tmp/wt-1" }, state: "live", status: "working", program: codex, labels: {} },
    { address: "wsl/t99", parent: null, state: "live", status: null, program: shell, labels: {} },
  ];
  let activeProp = { pane: "p-cli", terminal: "wsl/t2t" };
  const paneValues = {
    View: class {},
    div: element,
    svg: element,
    image: element,
    h_flex: element,
    InputState: { new: () => ({ on() {}, value: () => "", set_value() {}, set_selected_range() {}, focus() {} }) },
    Input: { new: () => element() },
    panes: () => paneList,
    terminals: () => termList,
    props: () => activeProp,
    run: (cmd: string, args: unknown) => { runCalls.push([cmd, args]); },
    h2() {},
    openOverlay() {},
    theme: () => ({ accent: "#3b82f6", raised: "#222", border: "#333", hover: "#444", foreground: "#fff", muted: "#aaa", dim: "#777", background: "#111" }),
    home: () => "/home/me",
    layout: () => null,
    settings: () => ({ language: "zh-CN" }),
  };
  const PaneCtor = new Function(
    ...Object.keys(paneValues),
    paneSource.replace(/^import .*;\n/gm, "").replace("export default class Pane", "return class Pane"),
  )(...Object.values(paneValues));
  const paneView = new PaneCtor();
  paneView.init({}, { timer: { every() {} } });

  const findById = (node: any, id: string): any => {
    if (!node) return null;
    if (node.calls?.id?.[0] === id) return node;
    for (const ch of node.nodes ?? []) {
      const hit = findById(ch, id);
      if (hit) return hit;
    }
    return null;
  };
  const collectTexts = (node: any): string[] => {
    if (!node) return [];
    const out: string[] = [];
    for (const ch of node.nodes ?? []) {
      if (typeof ch === "string") out.push(ch);
      else out.push(...collectTexts(ch));
    }
    return out;
  };

  // Manual raw-terminal CLI agent displays its own ID badge wsl/t2t.
  const cliTree = paneView.render({});
  const cliAddr = findById(cliTree, "ph-addr-p-cli");
  expect(cliAddr).not.toBeNull();
  expect(collectTexts(cliAddr)).toContain("wsl/t2t");
  expect(findById(cliTree, "ph-lin-p-cli")).toBeNull();

  // Handoff/fork ACP agent displays own ID, lineage kind badge, Worktree badge, standalone Side button, and clickable upstream badge.
  activeProp = { pane: "p-ho", terminal: "wsl/a49" };
  const hoTree = paneView.render({});
  expect(collectTexts(findById(hoTree, "ph-addr-p-ho"))).toContain("wsl/a49");
  expect(collectTexts(findById(hoTree, "ph-kind-p-ho"))).toContain("会话分叉");
  expect(collectTexts(findById(hoTree, "ph-wt-p-ho"))).toContain("Worktree");
  const sideBtn = findById(hoTree, "ph-side-p-ho");
  expect(sideBtn).not.toBeNull();
  expect(collectTexts(sideBtn)).toContain("旁路");
  const forkBtn = findById(hoTree, "ph-fork-p-ho");
  expect(forkBtn).not.toBeNull();
  expect(collectTexts(forkBtn)).toContain("分叉");
  const hoBtn = findById(hoTree, "ph-ho-p-ho");
  expect(hoBtn).not.toBeNull();
  expect(collectTexts(hoBtn)).toContain("接力");
  const upstreamBtn = findById(hoTree, "ph-lin-p-ho");
  expect(upstreamBtn).not.toBeNull();
  expect(collectTexts(upstreamBtn).join(" ")).toContain("wsl/t2t");
  upstreamBtn.calls.on_click[0]({}, { stop_propagation() {} });
  expect(runCalls).toEqual([["agent.focus", { terminal: "wsl/t2t" }]]);

  // Plain shell pane omits ph-addr.
  activeProp = { pane: "p-sh", terminal: "wsl/t99" };
  const shTree = paneView.render({});
  expect(findById(shTree, "ph-addr-p-sh")).toBeNull();
});

test("tab strip splits available width equally across all tabs when tabs overflow and restores natural widths when they fit", () => {
  const mkTabs = (n: number) =>
    Array.from({ length: n }, (_, i) => ({
      id: `t${i + 1}`,
      name: `Tab ${i + 1}`,
      named: true,
      active: i === 0,
    }));
  let currentTabs = mkTabs(2);
  const currentPanes = Array.from({ length: 8 }, (_, i) => ({
    pane: `p${i + 1}`,
    tab: `t${i + 1}`,
    terminal: `a${i + 1}`,
    acp: true,
    status: "idle",
    program: { name: "Claude Code", key: "claude", agent: true, icon: "icons/claude.svg" },
  }));
  const view = new (new Function(
    ...Object.keys(values),
    source.replace(/^import .*;\n/gm, "").replace("export default class Tabs", "return class Tabs"),
  )(
    ...Object.values({
      ...values,
      spaces: () => [{ id: "s1", active: true, tabs: currentTabs }],
      panes: () => currentPanes,
      settings: () => ({ zoom: 1, language: "zh-CN" }),
    }),
  ))();
  view.init();
  const findById = (node: any, id: string): any => {
    if (!node) return null;
    if (node.calls?.id?.[0] === id) return node;
    for (const ch of node.nodes ?? []) {
      const hit = findById(ch, id);
      if (hit) return hit;
    }
    return null;
  };
  const cx = { stop_propagation() {}, notify() {} };

  // 1) 2 tabs of 150px fit comfortably in 600px strip -> natural width (no forced .w())
  let root = view.render();
  findById(root, "tab-scroll").calls.on_size_change[0]({ width: 600 }, cx);
  findById(root, "tab-t1").calls.on_size_change[0]({ width: 150 }, cx);
  findById(root, "tab-t2").calls.on_size_change[0]({ width: 150 }, cx);
  root = view.render();
  expect(findById(root, "tab-t1").calls.w).toBeUndefined();
  expect(findById(root, "tab-t2").calls.w).toBeUndefined();

  // 2) 6 tabs overflow 600px strip (total natural 900 > 585) -> every tab gets floor((600 - 3*5) / 6) = 97px equally
  currentTabs = mkTabs(6);
  root = view.render();
  for (let i = 1; i <= 6; i++) {
    const node = findById(root, `tab-t${i}`);
    expect(node.calls.w?.[0]).toBe(97);
    expect(node.calls.overflow_hidden).toBeDefined();
    // Simulate GPUI reporting compressed 97px back: must NOT corrupt naturalWidths or cause oscillation
    node.calls.on_size_change[0]({ width: 97 }, cx);
  }
  const stableRoot = view.render();
  expect(findById(stableRoot, "tab-t1").calls.w?.[0]).toBe(97);
  expect(findById(stableRoot, "tab-t6").calls.w?.[0]).toBe(97);

  // 3) Closing back down to 3 tabs (450 <= 594) exits equal-width mode back to natural widths
  currentTabs = mkTabs(3);
  root = view.render();
  expect(findById(root, "tab-t1").calls.w).toBeUndefined();
  expect(findById(root, "tab-t3").calls.w).toBeUndefined();

  // 4) Widening strip to 1000px with 6 tabs (900 <= 985) also restores natural widths
  currentTabs = mkTabs(6);
  root = view.render();
  expect(findById(root, "tab-t1").calls.w?.[0]).toBe(97);
  findById(root, "tab-scroll").calls.on_size_change[0]({ width: 1000 }, cx);
  root = view.render();
  expect(findById(root, "tab-t1").calls.w).toBeUndefined();
});

test("newly added tabs are moved to the tail and Chrome-style vertical hairlines separate inactive tabs", () => {
  const runCalls: Array<[string, any]> = [];
  let activeSpaceId = "s1";
  let currentTabs: any[] = [
    { id: "t1", name: "First", named: true, active: true },
    { id: "t2", name: "Second", named: true, active: false },
    { id: "t3", name: "Third", named: true, active: false },
  ];
  const view = new (new Function(
    ...Object.keys(values),
    source.replace(/^import .*;\n/gm, "").replace("export default class Tabs", "return class Tabs"),
  )(
    ...Object.values({
      ...values,
      theme: () => ({
        chrome: "#000000",
        raised: "#141417",
        hover: "#1e1e24",
        selected: "#22222a",
        border: "#ffffff14",
        foreground: "#f4f4f5",
        muted: "#a3a3a4",
        dim: "#71717a",
        accent: "#3b82f6",
      }),
      spaces: () => [{ id: activeSpaceId, active: true, tabs: currentTabs }],
      panes: () => [],
      settings: () => ({ zoom: 1, language: "zh-CN" }),
      run: (cmd: string, args: any) => runCalls.push([cmd, args]),
    }),
  ))();
  view.init();
  const findById = (node: any, id: string): any => {
    if (!node) return null;
    if (node.calls?.id?.[0] === id) return node;
    for (const ch of node.nodes ?? []) {
      const hit = findById(ch, id);
      if (hit) return hit;
    }
    return null;
  };
  const cx = { stop_propagation() {}, notify() {} };

  // Initial render: inactive tabs stay borderless/flat like Chrome; hairline divider shows between inactive tabs only.
  const root1 = view.render();
  expect(runCalls).toEqual([]);
  const activeTab = findById(root1, "tab-t1");
  const inactiveTab2 = findById(root1, "tab-t2");
  expect(inactiveTab2.calls.border_color?.[0]).toBe("#000000");
  expect(inactiveTab2.calls.bg?.[0]).toBe("#000000");
  expect(activeTab.calls.border_color?.[0]).toBe("#ffffff14");
  expect(activeTab.calls.bg?.[0]).toBe("#141417");
  expect(findById(root1, "tab-sep-t1")).toBeNull();
  const sep2 = findById(root1, "tab-sep-t2");
  expect(sep2).not.toBeNull();
  expect(sep2.calls.w?.[0]).toBe(1);
  expect(sep2.calls.h?.[0]).toBe(14);
  expect(sep2.calls.bg?.[0]).toBe("#f4f4f526");
  expect(findById(root1, "tab-sep-t3")).not.toBeNull();

  // Hovering t3 hides both its left neighbor's divider (tab-sep-t2) and its own divider (tab-sep-t3), just like Chrome.
  findById(root1, "tab-t3").calls.on_hover[0](true, cx);
  const hoverRoot = view.render();
  expect(findById(hoverRoot, "tab-sep-t2")).toBeNull();
  expect(findById(hoverRoot, "tab-sep-t3")).toBeNull();
  findById(hoverRoot, "tab-t3").calls.on_hover[0](false, cx);

  // If a new tab t4 arrives in the middle (after active t1), tabs mod immediately moves it to the tail (index 4).
  currentTabs = [
    { id: "t1", name: "First", named: true, active: false },
    { id: "t4", name: "Fourth", named: true, active: true },
    { id: "t2", name: "Second", named: true, active: false },
    { id: "t3", name: "Third", named: true, active: false },
  ];
  view.render();
  expect(runCalls).toEqual([["tab.move", { id: "t4", index: 4 }]]);

  // If a new tab t5 already arrives at the tail, no redundant tab.move is issued.
  runCalls.length = 0;
  currentTabs = [
    { id: "t1", name: "First", named: true, active: false },
    { id: "t2", name: "Second", named: true, active: false },
    { id: "t3", name: "Third", named: true, active: false },
    { id: "t4", name: "Fourth", named: true, active: false },
    { id: "t5", name: "Fifth", named: true, active: true },
  ];
  view.render();
  expect(runCalls).toEqual([]);

  // Switching to another space initializes its seen set without moving existing tabs.
  runCalls.length = 0;
  activeSpaceId = "s2";
  currentTabs = [
    { id: "x1", name: "Other 1", named: true, active: true },
    { id: "x2", name: "Other 2", named: true, active: false },
  ];
  view.render();
  expect(runCalls).toEqual([]);
});
