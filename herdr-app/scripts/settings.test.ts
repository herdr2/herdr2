import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";

// What a click in the Settings window writes to settings.json, without a window or a core.
const source = await readFile(new URL("../defaults/frame/settings/main.js", import.meta.url), "utf8");
// settings.json as the mod sees it through readTextFile / writeTextFile.
const disk: { raw: string | null; writes: number; fail?: string; readFail?: string; toasts: any[] } = { raw: null, writes: 0, toasts: [] };
const values: Record<string, any> = {
  View: class {},
  appHome: () => "/home",
  notify: (n: any) => { disk.toasts.push(n); },
  readTextFile: async () => {
    if (disk.readFail) throw new Error(disk.readFail);
    return disk.raw;
  },
  writeTextFile: async (_path: string, text: string, expected: string | null) => {
    disk.writes++;
    if (disk.fail) throw new Error(disk.fail);
    if (expected !== disk.raw) throw new Error("changed since read");
    disk.raw = text;
  },
};
const names = [...source.matchAll(/^import\s*\{([\s\S]*?)\}\s*from\s*["'][^"']+["'];?/gm)]
  .flatMap(m => m[1].split(",").map(n => n.trim()).filter(Boolean));
const args = names.map(name => values[name] ?? (() => undefined));
const Settings = new Function(
  ...names,
  source
    .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
    .replace("export default class Settings", "return class Settings"),
)(...args);

/** Runs `edit` through the Settings window's change() on `raw`; the toasts it posts. */
async function change(raw: string | null, edit: (q: Record<string, any>) => void, fail?: string, readFail?: string) {
  Object.assign(disk, { raw, writes: 0, fail, readFail, toasts: [] });
  const view = new Settings();
  let done: () => void;
  const finished = new Promise<void>((resolve) => (done = resolve));
  view.change(edit, { spawn: (f: (cx: any) => Promise<void>) => f({ notify: () => done() }) });
  await finished;
  return { view, toasts: disk.toasts };
}

test("a change writes settings.json, keeping what else it holds", async () => {
  const { view, toasts } = await change('{"font_size": 15}', (q) => (q.dock_bounce = true));
  expect(JSON.parse(disk.raw!)).toEqual({ font_size: 15, dock_bounce: true });
  expect(view.person).toEqual({ font_size: 15, dock_bounce: true });
  expect(toasts).toEqual([]);
});

test("a broken settings.json is never written over, and says so", async () => {
  const broken = '{"font_size": 15, "theme": "x",}';
  const { toasts } = await change(broken, (q) => (q.dock_bounce = true));
  expect(disk.raw).toBe(broken);
  expect(disk.writes).toBe(0);
  expect(toasts).toEqual([{ title: "settings.json has errors: fix it first", level: "error" }]);
});

test("settings.json that isn't an object counts as broken", async () => {
  for (const raw of ["null", "[]", "3"]) {
    const { toasts } = await change(raw, (q) => (q.dock_bounce = true));
    expect(disk.raw).toBe(raw);
    expect(disk.writes).toBe(0);
    expect(toasts).toEqual([{ title: "settings.json has errors: fix it first", level: "error" }]);
  }
});

test("a write that keeps failing is reported, not dropped", async () => {
  const { toasts } = await change("{}", (q) => (q.dock_bounce = true), "read-only file system");
  expect(disk.writes).toBe(3);
  expect(disk.raw).toBe("{}");
  expect(toasts).toEqual([{ title: "Not saved to settings.json", body: "read-only file system", level: "error" }]);
});

test("a settings.json that can't be read, or an edit that throws, says why", async () => {
  const unread = await change("{}", (q) => (q.dock_bounce = true), undefined, "permission denied");
  expect(disk.writes).toBe(0);
  expect(unread.toasts).toEqual([{ title: "Cannot read settings.json", body: "permission denied", level: "error" }]);
  const thrown = await change("{}", () => { throw new Error("no such section"); });
  expect(disk.writes).toBe(0);
  expect(thrown.toasts).toEqual([{ title: "Cannot read settings.json", body: "no such section", level: "error" }]);
});

test("drove probedModelsFor normalizes string arrays and object entries without duplicates", () => {
  const view = new Settings();
  view.droveData = {
    agents: {
      agents: [
        { id: "claude", installed: true, models: ["claude-sonnet-4-6", "claude-opus-4-6", "claude-sonnet-4-6"] },
        { id: "codex", installed: true, models: [{ id: "gpt-5.4", name: "GPT-5.4" }, "gpt-5.3-codex"] },
      ],
    },
  };
  expect(view.probedModelsFor("claude")).toEqual([
    { id: "claude-sonnet-4-6", label: "claude-sonnet-4-6" },
    { id: "claude-opus-4-6", label: "claude-opus-4-6" },
  ]);
  expect(view.probedModelsFor("codex")).toEqual([
    { id: "gpt-5.4", label: "gpt-5.4 · GPT-5.4" },
    { id: "gpt-5.3-codex", label: "gpt-5.3-codex" },
  ]);
});

test("aiAvailableModels merges endpoint models from test results and daemon status cleanly", () => {
  const view = new Settings();
  view.droveData = {
    aiStatus: { availableModels: ["deepseek-chat", "deepseek-reasoner"] },
  };
  view.droveTestResult = {
    ok: true,
    discoveredModel: "gpt-4o-mini",
    availableModels: ["gpt-4o-mini", "deepseek-chat"],
  };
  expect(view.aiAvailableModels()).toEqual(["gpt-4o-mini", "deepseek-chat", "deepseek-reasoner"]);
});

test("roleChain and applyRoleChain support 4+ agents, reordering, and primary promotion", () => {
  const view = new Settings();
  const role: any = {
    primary: { agent: "claude", model: "claude-opus-4-6", effort: "high" },
    fallbacks: [
      { agent: "codex", model: "gpt-5.4", effort: "high" },
      { agent: "agy", model: null, effort: "medium" },
      { agent: "gemini", model: "gemini-2.5-pro", effort: "low" },
    ],
  };

  const chain = view.roleChain(role);
  expect(chain.map((c: any) => c.agent)).toEqual(["claude", "codex", "agy", "gemini"]);

  // Promote #3 (agy) to #1
  const reordered = [chain[2], chain[0], chain[1], chain[3]];
  view.applyRoleChain(role, reordered);
  expect(role.primary).toEqual({ agent: "agy", model: null, effort: "medium" });
  expect(role.fallbacks.map((c: any) => c.agent)).toEqual(["claude", "codex", "gemini"]);

  // Remove #1 -> #2 (claude) becomes new primary, remaining fallbacks preserved
  view.applyRoleChain(role, reordered.slice(1));
  expect(role.primary).toEqual({ agent: "claude", model: "claude-opus-4-6", effort: "high" });
  expect(role.fallbacks.map((c: any) => c.agent)).toEqual(["codex", "gemini"]);
});

test("maybeAutoProbeDrove reuses saved fullProbe models without re-probing", () => {
  const view = new Settings();
  let calls = 0;
  view.probeDroveAgents = () => {
    calls++;
  };
  view.droveMachine = "local";
  view.droveData = {
    agents: { fullProbe: true, agents: [{ id: "claude", installed: true, models: ["claude-opus-4-6"] }] },
  };
  view.maybeAutoProbeDrove({ notify() {} });
  expect(calls).toBe(0);

  view.droveMachine = "wsl";
  view.droveData = {
    agents: { fullProbe: false, agents: [{ id: "claude", installed: true, models: [] }] },
  };
  view.maybeAutoProbeDrove({ notify() {} });
  expect(calls).toBe(1);
});

test("drove settings uses English Drove title, hides feedback button, and preserves codex fast flag", () => {
  expect(source).toContain('{ id: "drove", title: "Drove", zh: "Drove", icon: "layers" }');
  expect(source).not.toContain("st-feedback");
  expect(source).not.toContain("Send feedback");

  const view = new Settings();
  const role: any = {
    primary: { agent: "codex", model: "gpt-6.1-sol", effort: "high", fast: true },
    fallbacks: [{ agent: "claude", model: null, effort: "high", fast: false }],
  };
  const chain = view.roleChain(role);
  expect(chain).toEqual([
    { agent: "codex", model: "gpt-6.1-sol", effort: "high", fast: true, strengths: [] },
    { agent: "claude", model: null, effort: "high", strengths: [] },
  ]);
  view.applyRoleChain(role, chain);
  expect(role.primary).toEqual({ agent: "codex", model: "gpt-6.1-sol", effort: "high", fast: true });
  expect(role.fallbacks).toEqual([{ agent: "claude", model: null, effort: "high" }]);
});

test("cursor context sizing preserves context field and falls back by model family", () => {
  const view = new Settings();
  view.droveData = {
    agents: {
      agents: [
        {
          id: "cursor",
          installed: true,
          models: ["grok-4.7", "claude-opus-5-5"],
          modelContexts: {
            "grok-4.7": ["256k", "500k"],
          },
        },
      ],
    },
  };
  expect(view.cursorContextChoicesFor("grok-4.7")).toEqual(["256k", "500k"]);
  expect(view.cursorContextChoicesFor("claude-opus-5-5")).toEqual(["300k", "1m"]);

  const role: any = {
    primary: { agent: "cursor", model: "grok-4.7", effort: "high", context: "500k" },
    fallbacks: [{ agent: "agy", model: "gemini-4-carbon-high", effort: "high" }],
  };
  const chain = view.roleChain(role);
  expect(chain[0]).toEqual({ agent: "cursor", model: "grok-4.7", effort: "high", context: "500k", strengths: [] });
  view.applyRoleChain(role, chain);
  expect(role.primary).toEqual({ agent: "cursor", model: "grok-4.7", effort: "high", context: "500k" });
});

test("cursor modelHasEmbeddedEffort detects built-in effort slugs and variants while allowing base models", () => {
  const view = new Settings();
  view.droveData = {
    agents: {
      agents: [
        {
          id: "cursor",
          installed: true,
          models: ["grok-4.7", "grok-4.7-high-fast", "claude-opus-5-5"],
          modelContexts: {
            "grok-4.7": ["256k", "500k"],
          },
        },
      ],
    },
  };

  for (const embedded of [
    "grok-4.7-high",
    "grok-4.7-high-fast",
    "claude-opus-5-thinking-xhigh-fast",
    "gpt-5.5-extra-high",
    "gpt-5.6-sol-none",
    "muse-wonder-1.0-minimal",
    "sonnet-4-thinking",
    "claude-opus-5-5[context=300k,effort=medium]",
    "grok-4.7[reasoning=high]",
  ]) {
    expect(view.modelHasEmbeddedEffort(embedded)).toBe(true);
  }

  for (const base of [
    null,
    "",
    "auto",
    "default",
    "grok-4.7",
    "claude-opus-5-5",
    "gpt-5.3-codex-fast",
    "composer-2.5-fast",
    "grok-4.7[context=500k,fast=true]",
  ]) {
    expect(view.modelHasEmbeddedEffort(base)).toBe(false);
  }

  expect(view.cursorContextChoicesFor("grok-4.7-high-fast")).toEqual(["256k", "500k"]);
  expect(view.cursorContextChoicesFor("claude-opus-5-thinking-xhigh-fast")).toEqual(["300k", "1m"]);
});

test("ai assistant parameter helpers format effort and parse context/maxTokens inputs", () => {
  const view = new Settings();
  expect(view.aiEffortLabel("auto")).toBe("Auto");
  expect(view.aiEffortLabel("none")).toBe("Off");
  expect(view.aiEffortLabel("low")).toBe("Low");
  expect(view.aiEffortLabel("medium")).toBe("Med");
  expect(view.aiEffortLabel("high")).toBe("High");

  expect(view.normalizeAiContextInput("")).toBe("");
  expect(view.normalizeAiContextInput("Auto")).toBe("");
  expect(view.normalizeAiContextInput(" DEFAULT ")).toBe("");
  expect(view.normalizeAiContextInput(" 128K ")).toBe("128k");
  expect(view.normalizeAiContextInput("1M")).toBe("1m");

  expect(view.parseAiMaxTokensInput(0)).toBe(0);
  expect(view.parseAiMaxTokensInput("")).toBe(0);
  expect(view.parseAiMaxTokensInput("auto")).toBe(0);
  expect(view.parseAiMaxTokensInput("0")).toBe(0);
  expect(view.parseAiMaxTokensInput(2048)).toBe(2048);
  expect(view.parseAiMaxTokensInput("4k")).toBe(4096);
  expect(view.parseAiMaxTokensInput("16")).toBe(64);
  expect(view.parseAiMaxTokensInput("200000")).toBe(131072);
  expect(view.parseAiMaxTokensInput("not-a-number")).toBeNull();
  expect(view.parseAiMaxTokensInput("-100")).toBeNull();
});

test("aiConfigureDroveRoles probes agents & models before calling roles-ai-config when cache is missing", async () => {
  const view = new Settings();
  const order: string[] = [];
  view.droveMachine = null;
  view.droveData = {
    config: { roles: { coder: { primary: { agent: "claude", model: null, effort: "high" } } }, ai: { model: "m1" } },
    agents: { fullProbe: false, agents: [{ id: "claude", installed: true, models: [] }] },
  };
  view.droveAiRolePromptInput = { value: () => "prefer claude" };
  view.droveUrlInput = { value: () => "http://127.0.0.1:3425/v1" };
  view.droveKeyInput = { value: () => "" };
  view.droveHcJson = async (args: string[]) => {
    order.push(args.join(" "));
    if (args[1] === "probe") {
      return {
        fullProbe: true,
        agents: [{ id: "claude", installed: true, models: ["claude-opus-4-6"] }],
      };
    }
    throw new Error(`unexpected hc call: ${args.join(" ")}`);
  };
  view.droveCallWithJsonFile = async (args: string[], payload: any) => {
    order.push(`${args.join(" ")}:${payload.mode}`);
    expect(view.hasCachedAgentModels()).toBe(true);
    return { ok: true, modelUsed: "m1", latencyMs: 12, settings: view.droveData };
  };

  let spawned: Promise<void> | null = null;
  const cx = {
    notify() {},
    spawn(fn: (cx: any) => Promise<void>) {
      spawned = fn(cx);
    },
  };
  view.aiConfigureDroveRoles("all", cx);
  await spawned;

  expect(order).toEqual(["drove probe --refresh", "drove roles-ai-config:all"]);
  expect(view.droveAiConfigResult?.ok).toBe(true);
  expect(view.droveAiConfigResult?.probedRefreshed).toBe(true);
  expect(view.droveAiConfiguring).toBe(false);
  expect(view.droveAiStage).toBeNull();
});

test("aiConfigureDroveRoles awaits in-flight probe instead of running a duplicate probe", async () => {
  const view = new Settings();
  const order: string[] = [];
  let releaseProbe!: () => void;
  const gate = new Promise<void>((resolve) => { releaseProbe = resolve; });

  view.droveMachine = null;
  view.droveData = {
    config: { roles: { coder: { primary: { agent: "claude", model: null, effort: "high" } } }, ai: { model: "m1" } },
    agents: { fullProbe: false, agents: [] },
  };
  view.droveAiRolePromptInput = { value: () => "" };
  view.droveUrlInput = { value: () => "http://127.0.0.1:3425/v1" };
  view.droveKeyInput = { value: () => "" };
  view.droveHcJson = async (args: string[]) => {
    order.push(args.join(" "));
    await gate;
    return {
      fullProbe: true,
      agents: [{ id: "codex", installed: true, models: ["gpt-5.4"] }],
    };
  };
  view.droveCallWithJsonFile = async (args: string[]) => {
    order.push(args.join(" "));
    expect(view.hasCachedAgentModels()).toBe(true);
    return { ok: true, modelUsed: "m1", latencyMs: 5, settings: view.droveData };
  };

  const pending: Promise<void>[] = [];
  const cx = {
    notify() {},
    spawn(fn: (cx: any) => Promise<void>) {
      pending.push(fn(cx));
    },
  };
  view.probeDroveAgents(cx);
  view.aiConfigureDroveRoles("models", cx);
  expect(view.droveAiStage).toBe("probing");
  releaseProbe();
  await Promise.all(pending);

  expect(order).toEqual(["drove probe --refresh", "drove roles-ai-config"]);
  expect(view.droveAiConfigResult?.probedRefreshed).toBe(true);
});
