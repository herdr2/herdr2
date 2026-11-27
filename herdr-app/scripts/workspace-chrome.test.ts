import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";

class Element {
  nodes: Element[] = [];
  calls: Record<string, any[]> = {};
  constructor() {
    const proxy: Element = new Proxy(this, {
      get: (target, key: string) => key in target ? (target as any)[key] : (...args: any[]) => {
        if (key === "child") target.nodes.push(args[0]);
        else if (key === "children") target.nodes.push(...args[0]);
        else if (key === "when") { if (args[0]) args[1](proxy); }
        else target.calls[key] = args;
        return proxy;
      },
    });
    return proxy;
  }
}

const source = await readFile(new URL("../defaults/mods/workspace-chrome/main.js", import.meta.url), "utf8");
const element = () => new Element();

function findById(node: any, id: string): any {
  if (!node) return null;
  if (node.calls?.id?.[0] === id) return node;
  for (const ch of node.nodes ?? []) {
    const hit = findById(ch, id);
    if (hit) return hit;
  }
  return null;
}

function makeChrome(opts: {
  width?: number;
  hidden?: boolean;
  platform?: string;
  language?: string;
  savedWidth?: number;
} = {}) {
  const runCalls: unknown[][] = [];
  const sharedStore: Record<string, unknown> = {};
  if (opts.savedWidth !== undefined) sharedStore["sidebar.width"] = opts.savedWidth;
  const currentLayout = {
    id: "root",
    children: [{ id: "sidebar", hidden: opts.hidden ?? false, size: { px: opts.width ?? 268 } }],
  };
  const values = {
    View: class {},
    div: element,
    image: element,
    svg: element,
    h_flex: element,
    v_flex: element,
    theme: () => ({}),
    run: (cmd: string, args: unknown) => runCalls.push([cmd, args]),
    props: () => ({}),
    window: () => ({ platform: opts.platform ?? "linux", fullscreen: false }),
    shared: (k: string) => sharedStore[k],
    setShared: (k: string, v: unknown) => { sharedStore[k] = v; },
    layout: () => currentLayout,
    settings: () => ({ language: opts.language ?? "zh-CN" }),
    keys: () => [{
      section: "global",
      action: "layout.toggle:sidebar",
      keys: (opts.platform ?? "linux") === "macos" ? "cmd-b" : "ctrl-shift-b",
    }],
  };
  const WorkspaceChrome = new Function(
    ...Object.keys(values),
    source.replace(/^import .*;\n/gm, "").replace("export default class WorkspaceChrome", "return class WorkspaceChrome"),
  )(...Object.values(values));
  return { view: new WorkspaceChrome(), runCalls, sharedStore };
}

test("top-left sidebar button hides the sidebar completely, matching layout.toggle:sidebar", () => {
  const { view, runCalls, sharedStore } = makeChrome({ width: 280, platform: "linux", language: "zh-CN" });
  const btn = findById(view.render(), "sidebar-toggle");
  expect(btn).not.toBeNull();
  expect(btn.calls.tooltip?.[0]).toBe("收起侧边栏 (Ctrl+Shift+B)");

  btn.calls.on_click[0]({}, { stop_propagation() {} });
  expect(sharedStore["sidebar.width"]).toBe(280);
  expect(sharedStore["sidebar.collapsed"]).toBe(false);
  expect(runCalls).toEqual([["layout.toggle", { id: "sidebar" }]]);
});

test("legacy narrow sidebar button expands back to full width instead of shrinking further", () => {
  const { view, runCalls, sharedStore } = makeChrome({ width: 44, savedWidth: 310, platform: "linux", language: "en" });
  const btn = findById(view.render(), "sidebar-toggle");
  expect(btn).not.toBeNull();
  expect(btn.calls.tooltip?.[0]).toBe("Expand sidebar (Ctrl+Shift+B)");

  btn.calls.on_click[0]({}, { stop_propagation() {} });
  expect(sharedStore["sidebar.collapsed"]).toBe(false);
  expect(runCalls).toEqual([["layout.resize", { id: "sidebar", px: 310 }]]);
});
