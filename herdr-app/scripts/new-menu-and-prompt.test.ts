import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";

const newMenuSource = (await readFile(new URL("../defaults/mods/new-menu/main.js", import.meta.url), "utf8"))
  .replace(/^import .*;\n/gm, "")
  .replace("export default class NewMenu", "class NewMenu");

const promptSource = (await readFile(new URL("../defaults/mods/prompt/main.js", import.meta.url), "utf8"))
  .replace(/^import .*;\n/gm, "")
  .replace("export default class Prompt", "class Prompt");

function makeElementProxy() {
  const el: any = new Proxy(
    {},
    {
      get: (_, _method) => (..._args: any[]) => el,
    },
  );
  return el;
}

function makeNewMenuHarness(mode = "acp") {
  const runs: { name: string; args?: any }[] = [];
  const closed: any[] = [];
  const sharedStore: Record<string, any> = { "new-menu.agent-mode": mode };
  const el = makeElementProxy();
  const theme = () => new Proxy({}, { get: () => "#111111" });
  const NewMenu = new Function(
    "View",
    "div",
    "svg",
    "image",
    "h_flex",
    "v_flex",
    "theme",
    "spaces",
    "panes",
    "knownAgents",
    "run",
    "h2",
    "openOverlay",
    "closeOverlay",
    "props",
    "window",
    "keys",
    "shared",
    "setShared",
    "settings",
    `${newMenuSource}; return NewMenu;`,
  )(
    class {},
    () => el,
    () => el,
    () => el,
    () => el,
    () => el,
    theme,
    () => [{ id: "sp1", active: true, folder: "/repo/app", tabs: [] }],
    () => [],
    async () => [],
    (name: string, args?: any) => runs.push({ name, args }),
    async () => ({ code: 0, stdout: "{}", stderr: "" }),
    () => {},
    (id: any) => closed.push(id),
    () => ({ overlay: "ov-1" }),
    () => ({ platform: "linux" }),
    () => [],
    (k: string) => sharedStore[k],
    (k: string, v: any) => { sharedStore[k] = v; },
    () => ({ language: "en" }),
  );
  return { NewMenu, runs, closed };
}

function makePromptHarness(lang = "en") {
  const el = makeElementProxy();
  const theme = () => new Proxy({}, { get: () => "#111111" });
  const mod = new Function(
    "View",
    "div",
    "svg",
    "v_flex",
    "h_flex",
    "InputState",
    "Input",
    "TextareaState",
    "Textarea",
    "run",
    "h2",
    "props",
    "closeOverlay",
    "theme",
    "settings",
    "checking",
    "nextCommand",
    "keys",
    "appWindow",
    `${promptSource}; return { Prompt, polishFallbackMessage };`,
  )(
    class {},
    () => el,
    () => el,
    () => el,
    () => el,
    { new: () => ({ value: () => "", set_selected_range: () => {}, on: () => {}, focus: () => {} }) },
    { new: () => el },
    { new: () => ({ value: () => "", set_auto_grow: () => {}, on: () => {}, focus: () => {} }) },
    { new: () => el },
    () => {},
    async () => ({ code: 0, stdout: "{}", stderr: "" }),
    () => ({ overlay: "ov-prompt" }),
    () => {},
    theme,
    () => ({ language: lang }),
    () => false,
    async () => new Promise(() => {}),
    () => [],
    () => ({ platform: "linux" }),
  );
  return mod;
}

test("new-menu supports built-in TRAE CLI in both ACP and CLI modes", () => {
  const h = makeNewMenuHarness("acp");
  const view = new h.NewMenu();
  const trae = { key: "trae", name: "TRAE CLI", icon: "icons/trae.svg", tint: true, run: ["traex"], installed: true };

  view.launchAgent(trae, { id: "sp1", folder: "/repo/app", tabs: [] }, true);
  view.launchAgent(trae, { id: "sp1", folder: "/repo/app", tabs: [] }, false);

  expect(h.runs).toEqual([
    { name: "tab.new", args: { space: "sp1", acp: true, agent: "trae", folder: "/repo/app" } },
    { name: "tab.new", args: { name: "TRAE CLI", space: "sp1", argv: ["traex"] } },
  ]);
});

test("prompt polishFallbackMessage surfaces invalid response and context drift clearly", () => {
  const hEn = makePromptHarness("en");
  expect(hEn.polishFallbackMessage("ai_invalid_response")).toContain("non-JSON");
  expect(hEn.polishFallbackMessage("ai_context_drift")).toContain("drifted");

  const hZh = makePromptHarness("zh-CN");
  expect(hZh.polishFallbackMessage("ai_invalid_response")).toContain("已保留原草稿");
  expect(hZh.polishFallbackMessage("ai_context_drift")).toContain("偏离");
});

test("prompt startPolish spawns an async task so notify succeeds after await", async () => {
  let textVal = "rough prompt";
  let resolveH2: ((v: any) => void) | null = null;
  const el = makeElementProxy();
  const theme = () => new Proxy({}, { get: () => "#111111" });
  const mod = new Function(
    "View",
    "div",
    "svg",
    "v_flex",
    "h_flex",
    "InputState",
    "Input",
    "TextareaState",
    "Textarea",
    "run",
    "h2",
    "props",
    "closeOverlay",
    "theme",
    "settings",
    "checking",
    "nextCommand",
    "keys",
    "appWindow",
    `${promptSource}; return { Prompt };`,
  )(
    class {},
    () => el,
    () => el,
    () => el,
    () => el,
    { new: () => ({ value: () => "", set_selected_range: () => {}, on: () => {}, focus: () => {} }) },
    { new: () => el },
    {
      new: () => ({
        value: () => textVal,
        set_value: (v: string) => { textVal = v; },
        set_auto_grow: () => {},
        on: () => {},
        focus: () => {},
      }),
    },
    { new: () => el },
    () => {},
    () => new Promise((r) => { resolveH2 = r; }),
    () => ({ mode: "polish", value: "rough prompt", terminal: "t1", overlay: "ov-prompt" }),
    () => {},
    theme,
    () => ({ language: "zh-CN" }),
    () => false,
    async () => new Promise(() => {}),
    () => [],
    () => ({ platform: "linux" }),
  );

  const view = new mod.Prompt();
  view.init({}, { focus_handle: () => ({}), timer: { after: () => {} }, spawn: () => {} });

  // Simulate gpui-kit's call-scoped event cx: valid only during the synchronous click handler,
  // while cx.spawn provides a task-scoped cx that remains valid across await.
  let callAlive = true;
  let taskNotifies = 0;
  let spawnedDone: Promise<any> | null = null;
  const clickCx = {
    notify() {
      if (!callAlive) throw new TypeError("cx is no longer valid");
    },
    spawn(fn: (taskCx: any) => any) {
      const taskCx = { notify: () => { taskNotifies += 1; } };
      spawnedDone = Promise.resolve(fn(taskCx));
    },
  };

  view.startPolish(clickCx);
  callAlive = false; // on_click returned; CallScope popped
  expect(view.polishing).toBe(true);
  expect(taskNotifies).toBe(1);

  resolveH2!({
    code: 0,
    stdout: JSON.stringify({ usedAi: true, polished: "Refined prompt", summary: "Clearer" }),
    stderr: "",
  });
  await spawnedDone;

  expect(textVal).toBe("Refined prompt");
  expect(view.polishing).toBe(false);
  expect(taskNotifies).toBe(2);
});
