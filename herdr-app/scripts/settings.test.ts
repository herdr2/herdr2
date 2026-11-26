import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";

// What a click in the Settings window writes to settings.json, without a window or a core.
const source = await readFile(new URL("../defaults/frame/settings/main.js", import.meta.url), "utf8");
// settings.json as the mod sees it through readTextFile / writeTextFile.
const disk: {
  raw: string | null;
  writes: number;
  fail?: string;
  readFail?: string;
  toasts: any[];
  hcHandler?: (args: string[]) => Promise<any>;
} = { raw: null, writes: 0, toasts: [] };
const values: Record<string, any> = {
  View: class {},
  home: () => "/home/tester",
  appHome: () => "/home",
  hc: async (args: string[]) => {
    if (disk.hcHandler) return await disk.hcHandler(args);
    return { code: 0, stdout: "{}", stderr: "" };
  },
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

test("skillArgs appends --on <machine> only when a remote machine is selected", () => {
  const view = new Settings();
  view.skillMachine = null;
  expect(view.skillArgs(["skill", "status"])).toEqual(["skill", "status"]);
  view.skillMachine = "devbox";
  expect(view.skillArgs(["skill", "status"])).toEqual(["skill", "status", "--on", "devbox"]);
});

test("loadSkills queries local and remote status via hc and captures structured errors", async () => {
  const calls: string[][] = [];
  const view = new Settings();
  try {
    disk.hcHandler = async (args) => {
      calls.push(args);
      if (args.includes("broken-box")) {
        return { code: 1, stdout: JSON.stringify({ error: { message: "remote unreachable" } }), stderr: "" };
      }
      return {
        code: 0,
        stdout: JSON.stringify({ home: args.includes("devbox") ? "/home/dev" : "/home/local", agents: [] }),
        stderr: "",
      };
    };

    await view.loadSkills();
    expect(calls[0]).toEqual(["skill", "status", "--json"]);
    expect(view.skillData?.home).toBe("/home/local");
    expect(view.skillError).toBeNull();
    expect(view.skillLoading).toBe(false);

    view.skillMachine = "devbox";
    await view.loadSkills();
    expect(calls[1]).toEqual(["skill", "status", "--on", "devbox", "--json"]);
    expect(view.skillData?.home).toBe("/home/dev");

    view.skillMachine = "broken-box";
    await view.loadSkills();
    expect(calls[2]).toEqual(["skill", "status", "--on", "broken-box", "--json"]);
    expect(view.skillError).toBe("remote unreachable");
    expect(view.skillLoading).toBe(false);
  } finally {
    disk.hcHandler = undefined;
  }
});

test("installSkills forwards --on, --all/--agent/--target and formats local vs remote summaries", async () => {
  const calls: string[][] = [];
  const view = new Settings();
  view.person = { language: "en" };
  const runInstall = async (opts: any) => {
    let task: Promise<void> | null = null;
    const cx = { notify() {}, spawn(fn: (cx: any) => Promise<void>) { task = fn(cx); } };
    view.installSkills(opts, cx);
    await task;
  };
  try {
    disk.hcHandler = async (args) => {
      calls.push(args);
      return {
        code: 0,
        stdout: JSON.stringify({
          ok: true,
          installedCount: 4,
          targets: [{ agent: "claude", target: "/home/dev/.claude/skills", files: ["a", "b", "c", "d"] }],
          status: { home: "/home/dev", agents: [], summary: { detectedAgents: 1, upToDateAgents: 1 } },
        }),
        stderr: "",
      };
    };

    // 1. Local --all
    await runInstall({});
    expect(calls[0]).toEqual(["skill", "install", "--json", "--all"]);
    expect(view.skillResult?.ok).toBe(true);
    expect(view.skillResult?.summary).toBe("Installed 4 skill file(s) across 1 target(s).");
    expect(view.skillInstalling).toBe(false);

    // 2. Remote --agent (English)
    view.skillMachine = "gpu-1";
    await runInstall({ agent: "claude" });
    expect(calls[1]).toEqual(["skill", "install", "--on", "gpu-1", "--json", "--agent", "claude"]);
    expect(view.skillResult?.summary).toBe("Installed 4 skill file(s) across 1 target(s) (gpu-1).");

    // 3. Remote --target (Chinese)
    view.person = { language: "zh-CN" };
    await runInstall({ target: "/home/dev/.custom/skills" });
    expect(calls[2]).toEqual(["skill", "install", "--on", "gpu-1", "--json", "--target", "/home/dev/.custom/skills"]);
    expect(view.skillResult?.summary).toContain("远程主机 gpu-1");
  } finally {
    disk.hcHandler = undefined;
  }
});

test("aiInstallSkills forwards --on, --lang, and optional --query hint", async () => {
  const calls: string[][] = [];
  const view = new Settings();
  view.person = { language: "zh-CN" };
  view.skillMachine = "build-srv";
  view.skillAiHintInput = { value: () => "  install into ~/.codex/skills  " };
  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (cx: any) => Promise<void>) { task = fn(cx); } };

  try {
    disk.hcHandler = async (args) => {
      calls.push(args);
      return {
        code: 0,
        stdout: JSON.stringify({
          ok: true,
          usedAi: true,
          model: "deepseek-chat",
          summary: "已在远程主机 build-srv 安装完成。",
          targets: [{ agent: "codex", target: "/home/dev/.codex/skills", files: ["f1"] }],
          tips: ["重启远程 Codex 会话"],
          status: { home: "/home/dev", agents: [] },
        }),
        stderr: "",
      };
    };

    view.aiInstallSkills(cx);
    await task;
    expect(calls[0]).toEqual([
      "skill",
      "ai-install",
      "--lang",
      "zh-CN",
      "--on",
      "build-srv",
      "--json",
      "--query",
      "install into ~/.codex/skills",
    ]);
    expect(view.skillResult?.ok).toBe(true);
    expect(view.skillResult?.usedAi).toBe(true);
    expect(view.skillResult?.model).toBe("deepseek-chat");
    expect(view.skillResult?.tips).toEqual(["重启远程 Codex 会话"]);
    expect(view.skillInstalling).toBe(false);
  } finally {
    disk.hcHandler = undefined;
  }
});

test("buildUpdateModal handles available updates, up-to-date, and check errors", () => {
  const view = new Settings();
  const avail = view.buildUpdateModal({
    app: { version: "2.0.3", ready: "2.0.4", notes: "Herdr2 v2.0.4" },
    core: { version: "2.0.3", ready: "2.0.4" },
    available: {
      kind: "app",
      version: "2.0.4",
      notes: "Herdr2 v2.0.4",
      restart: true,
      items: [
        { kind: "app", current: "2.0.3", next: "2.0.4", notes: "Herdr2 v2.0.4" },
        { kind: "core", current: "2.0.3", next: "2.0.4" },
      ],
    },
  });
  expect(avail.kind).toBe("available");
  expect(avail.version).toBe("2.0.4");
  expect(avail.restart).toBe(true);
  expect(avail.needsElevation).toBe(false);
  expect(avail.items.length).toBe(2);

  const elevated = view.buildUpdateModal({
    app: { version: "2.0.3", ready: "2.0.4", needs_elevation: true },
    available: {
      kind: "app",
      version: "2.0.4",
      restart: true,
      needs_elevation: true,
      items: [{ kind: "app", current: "2.0.3", next: "2.0.4" }],
    },
  });
  expect(elevated.kind).toBe("available");
  expect(elevated.needsElevation).toBe(true);

  const upToDate = view.buildUpdateModal({
    app: { version: "2.0.3", ready: null },
    ui: { version: "2.0.3", ready: null },
    core: { version: "2.0.3", ready: null },
    available: null,
    last_error: null,
  });
  expect(upToDate).toEqual({
    kind: "up_to_date",
    appVersion: "2.0.3",
    coreVersion: "2.0.3",
  });

  const failed = view.buildUpdateModal({
    app: { version: "2.0.3", ready: null },
    available: null,
    last_error: "curl: (7) Failed to connect",
    releases_url: "https://example.invalid",
  });
  expect(failed).toEqual({
    kind: "error",
    message: "curl: (7) Failed to connect",
    missingUrl: false,
  });
});

test("triggerUpdateCheck persists edited releases_url and opens modal when check_seq advances", async () => {
  const ran: string[] = [];
  let currentUpdates: any = {
    app: { version: "2.0.3", ready: null },
    ui: { version: "2.0.3", ready: null },
    core: { version: "2.0.3", ready: null },
    available: null,
    checking: false,
    check_seq: 1,
    last_error: null,
  };
  const customValues = {
    ...values,
    run: (cmd: string) => { ran.push(cmd); },
    updates: () => currentUpdates,
  };
  const customArgs = names.map((name) => customValues[name] ?? (() => undefined));
  const CustomSettings = new Function(
    ...names,
    source
      .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
      .replace("export default class Settings", "return class Settings"),
  )(...customArgs);

  Object.assign(disk, { raw: "{}", writes: 0, fail: undefined, readFail: undefined, toasts: [] });
  const view = new CustomSettings();
  view.person = {};
  view.releasesUrlInput = { value: () => "https://github.com/herdr2/herdr2/releases/download/ota-latest/" };

  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (c: any) => Promise<void>) { task = fn(cx); } };
  view.triggerUpdateCheck(cx);
  expect(view.waitingUpdateCheck).toBe(true);
  await task;
  expect(JSON.parse(disk.raw!)).toEqual({
    releases_url: "https://github.com/herdr2/herdr2/releases/download/ota-latest",
  });
  expect(ran).toEqual(["app.check_updates"]);

  // Still checking -> modal stays closed
  currentUpdates = { ...currentUpdates, checking: true };
  view.syncUpdateCheck(cx);
  expect(view.updateModal).toBeNull();

  // Check finishes with no update -> opens "up_to_date" modal
  currentUpdates = { ...currentUpdates, checking: false, check_seq: 2 };
  view.syncUpdateCheck(cx);
  expect(view.waitingUpdateCheck).toBe(false);
  expect(view.updateModal?.kind).toBe("up_to_date");
});

test("AI settings are global across all machines and never forward --on even when droveMachine is remote", async () => {
  expect(source).not.toContain("st-ai-m-");

  const calls: string[][] = [];
  const writtenFiles: Record<string, string> = {};
  const customValues = {
    ...values,
    hc: async (args: string[]) => {
      calls.push(args);
      if (args[0] === "ai" && args[1] === "probe-prompt-mode") {
        return {
          code: 0,
          stdout: JSON.stringify({ ok: true, detectedMode: "shared_prefix", status: { baseUrl: "https://api.example.com/v1", configured: true } }),
          stderr: "",
        };
      }
      if (args[0] === "ai" && args[1] === "test") {
        return { code: 0, stdout: JSON.stringify({ ok: true, latencyMs: 12, availableModels: ["m1"] }), stderr: "" };
      }
      return {
        code: 0,
        stdout: JSON.stringify({ baseUrl: "https://api.example.com/v1", model: "m1", configured: true }),
        stderr: "",
      };
    },
    writeTextFile: async (path: string, text: string) => {
      writtenFiles[path] = text;
    },
  };
  const customArgs = names.map((name) => customValues[name] ?? (() => undefined));
  const CustomSettings = new Function(
    ...names,
    source
      .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
      .replace("export default class Settings", "return class Settings"),
  )(...customArgs);

  const view = new CustomSettings();
  view.droveMachine = "wsl";
  let urlVal = "";
  let keyVal = "";
  let magpieVal = "";
  view.droveUrlInput = { value: () => urlVal, set_value: (v: string) => { urlVal = v; } };
  view.droveKeyInput = { value: () => keyVal, set_value: (v: string) => { keyVal = v; } };
  view.droveMagpieUrlInput = { value: () => magpieVal, set_value: (v: string) => { magpieVal = v; } };

  await view.loadAi();
  expect(calls[0]).toEqual(["ai", "status", "--json"]);
  expect(urlVal).toBe("https://api.example.com/v1");

  // Switching/loading Drove on a remote machine must not overwrite global AI inputs.
  view.syncDroveInputs({ config: { ai: { baseUrl: "http://other.invalid" }, magpie: { gatewayUrl: "http://127.0.0.1:3425" } } });
  expect(urlVal).toBe("https://api.example.com/v1");
  expect(magpieVal).toBe("http://127.0.0.1:3425");

  const pending: Promise<void>[] = [];
  const cx = { notify() {}, spawn(fn: (c: any) => Promise<void>) { pending.push(fn(cx)); } };
  view.saveAiPatch({ effort: "high" }, cx);
  await Promise.all(pending.splice(0));
  expect(calls[1].slice(0, 3)).toEqual(["ai", "settings-set", "--file"]);
  expect(calls[1]).not.toContain("--on");

  view.testCoreAi(cx);
  await Promise.all(pending.splice(0));
  expect(calls[2].slice(0, 3)).toEqual(["ai", "test", "--file"]);
  expect(calls[2]).not.toContain("--on");

  await view.runPromptModeRedetect(cx, false);
  expect(calls[3]).toEqual(["ai", "probe-prompt-mode", "--json"]);
});

test("runDroveProbe passes an extended 180s timeout to hc so cold-start ACP probes do not time out at 30s", async () => {
  const recorded: Array<{ args: string[]; opts: any }> = [];
  const customValues = {
    ...values,
    hc: async (args: string[], opts?: any) => {
      recorded.push({ args, opts });
      return {
        code: 0,
        stdout: JSON.stringify({ fullProbe: true, agents: [{ id: "claude", installed: true, models: ["claude-opus-4-6"] }] }),
        stderr: "",
      };
    },
  };
  const customArgs = names.map((name) => customValues[name] ?? (() => undefined));
  const CustomSettings = new Function(
    ...names,
    source
      .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
      .replace("export default class Settings", "return class Settings"),
  )(...customArgs);

  const view = new CustomSettings();
  view.droveMachine = "gpu-box";
  view.droveData = { config: {}, agents: { fullProbe: false, agents: [] } };
  const ok = await view.runDroveProbe({ notify() {} });
  expect(ok).toBe(true);
  expect(recorded).toEqual([
    {
      args: ["drove", "probe", "--refresh", "--on", "gpu-box", "--json"],
      opts: { timeout: 180000 },
    },
  ]);
});

test("droveProbeSummary separates healthy and errored installed agents and formats stderr details", () => {
  const view = new Settings();
  view.droveData = {
    config: {},
    agents: {
      fullProbe: true,
      agents: [
        { id: "claude", installed: true, models: ["claude-opus-4-6"], error: null },
        {
          id: "codex",
          installed: true,
          models: ["gpt-5.4"],
          error: "ACP initialize failed: process exited\nstderr:\nError: Unauthorized (401)",
        },
        { id: "agy", installed: true, models: [], error: "`agy models` exited with code 1\nstderr:\nPlease login first" },
        { id: "gemini", installed: false, models: [], error: null },
      ],
    },
  };

  const summary = view.droveProbeSummary();
  expect(summary.installed.map((a: any) => a.id)).toEqual(["claude", "codex", "agy"]);
  expect(summary.ready.map((a: any) => a.id)).toEqual(["claude"]);
  expect(summary.errored.map((a: any) => a.id)).toEqual(["codex", "agy"]);
  expect(summary.missing.map((a: any) => a.id)).toEqual(["gemini"]);

  expect(view.formatAgentProbeError(summary.errored[0].error)).toBe(
    "ACP initialize failed: process exited · Error: Unauthorized (401)",
  );
  expect(view.formatAgentProbeError(summary.errored[1].error)).toBe(
    "`agy models` exited with code 1 · Please login first",
  );
  expect(view.formatAgentProbeError("ACP probe timed out after 60s")).toBe("ACP probe timed out after 60s");
});

test("ai protocol is auto-detected without manual selector and synced on connection test", async () => {
  const view = new Settings();
  expect(view.aiProtocolLabel("anthropic")).toBe("Claude");
  expect(view.aiProtocolLabel("claude")).toBe("Claude");
  expect(view.aiProtocolLabel("openai")).toBe("OpenAI");
  expect(view.aiProtocolLabel(null)).toBeNull();
  expect(view.aiProtocolLabel("")).toBeNull();

  // Settings UI displays auto-detected protocol badge without any manual protocol picker.
  expect(source).toContain("st-ai-protocol-badge");
  expect(source).not.toContain("st-ai-protocol-select");

  view.aiStatus = { baseUrl: "https://relay.example.com/v1", configured: true, availableModels: [] };
  view.droveUrlInput = { value: () => "https://relay.example.com/v1" };
  view.droveKeyInput = { value: () => "" };
  view.aiCallWithJsonFile = async () => ({
    ok: true,
    protocol: "anthropic",
    discoveredModel: "claude-sonnet-4-6",
    availableModels: ["claude-sonnet-4-6"],
    latencyMs: 95,
  });

  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (cx: any) => Promise<void>) { task = fn(cx); } };
  view.testCoreAi(cx);
  await task;

  expect(view.aiTestResult?.protocol).toBe("anthropic");
  expect(view.aiStatus?.protocol).toBe("anthropic");
  expect(view.aiStatus?.availableModels).toEqual(["claude-sonnet-4-6"]);
});

test("aiDiagnoseDroveProbeErrors uses remote AI without local overrides and falls back to local AI on older remote h2", async () => {
  const calls: Array<{ args: string[]; payload: any; opts: any }> = [];
  const view = new Settings();
  view.person = { language: "zh-CN" };
  view.droveMachine = "ubuntu-wsl";
  view.aiStatus = { configured: true, model: "local-deepseek" };
  view.droveData = {
    aiStatus: { configured: true },
    config: { ai: { model: "remote-model", baseUrl: "http://127.0.0.1:3425/v1" } },
    agents: {
      fullProbe: true,
      agents: [
        { id: "claude", installed: true, models: ["opus"], error: null },
        { id: "cursor", installed: true, command: ["cursor-agent", "acp"], path: "/home/u/.local/bin/cursor-agent", models: [], error: "ACP probe timed out after 60s" },
      ],
    },
  };
  view.droveUrlInput = { value: () => "https://api.deepseek.com/v1" };
  view.droveKeyInput = { value: () => "" };
  view.droveCallWithJsonFile = async (args: string[], payload: any, opts: any) => {
    calls.push({ args, payload, opts });
    if (!opts?.local) {
      throw new Error("ubuntu-wsl's h2 is older and has no drove.probe.ai_diagnose");
    }
    return {
      ok: true,
      modelUsed: "local-deepseek",
      latencyMs: 320,
      summary: "守护进程缺少代理导致 Cursor 探测超时。",
      items: [
        {
          agent: "cursor",
          category: "env_proxy",
          likelyFalsePositive: true,
          cause: "daemon 缺少代理变量",
          fix: "登录并检查代理后重启",
          command: "cursor-agent login",
          commandArgv: ["cursor-agent", "login"],
        },
      ],
    };
  };

  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (c: any) => Promise<void>) { task = fn(cx); } };
  view.aiDiagnoseDroveProbeErrors(cx);
  await task;

  expect(calls.length).toBe(2);
  // 1st attempt: remote daemon without forwarding local URL/model/key overrides.
  expect(calls[0].args).toEqual(["drove", "probe-ai-diagnose"]);
  expect(calls[0].opts?.local).toBeUndefined();
  expect(calls[0].payload).toEqual({
    agents: ["cursor"],
    language: "zh-CN",
  });
  // 2nd attempt: local AI fallback with full remote probe snapshot + local overrides.
  expect(calls[1].args).toEqual(["drove", "probe-ai-diagnose"]);
  expect(calls[1].opts?.local).toBe(true);
  expect(calls[1].payload).toEqual({
    agents: ["cursor"],
    targetMachine: "ubuntu-wsl",
    erroredAgents: [
      {
        id: "cursor",
        command: ["cursor-agent", "acp"],
        path: "/home/u/.local/bin/cursor-agent",
        modelsCount: 0,
        error: "ACP probe timed out after 60s",
      },
    ],
    healthyAgents: ["claude"],
    language: "zh-CN",
    baseUrl: "https://api.deepseek.com/v1",
    model: "local-deepseek",
  });
  expect(view.droveProbeAiDiagnosing).toBe(false);
  expect(view.droveProbeAiResult?.ok).toBe(true);
  expect(view.droveProbeAiResult?.items?.[0]?.agent).toBe("cursor");
});

test("launchAgentToFixDroveProbeErrors and runProbeFixCommandInTerminal target the selected machine space", async () => {
  const ran: Array<{ cmd: string; args: any }> = [];
  const toasts: any[] = [];
  let closed = 0;
  let currentSpaces: any[] = [
    { id: "sp-local", machine: null, active: true },
    { id: "sp-wsl", machine: "ubuntu-wsl", active: false },
  ];
  const customValues = {
    ...values,
    run: (cmd: string, args: any) => { ran.push({ cmd, args }); },
    notify: (n: any) => { toasts.push(n); },
    spaces: () => currentSpaces,
    knownAgents: async () => [
      { key: "claude", name: "Claude Code", installed: true, run: ["claude", "--dangerously-skip-permissions"] },
    ],
  };
  const customArgs = names.map((name) => customValues[name] ?? (() => undefined));
  const CustomSettings = new Function(
    ...names,
    source
      .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
      .replace("export default class Settings", "return class Settings"),
  )(...customArgs);

  const view = new CustomSettings();
  view.person = { language: "zh-CN" };
  view.close = () => { closed++; };
  view.droveMachine = "ubuntu-wsl";
  view.droveData = {
    agents: {
      fullProbe: true,
      agents: [
        { id: "claude", installed: true, models: ["opus"], error: null },
        { id: "cursor", installed: true, command: ["cursor-agent", "acp"], path: "/usr/local/bin/cursor-agent", error: "ACP probe timed out after 60s" },
      ],
    },
  };
  view.droveProbeAiResult = {
    ok: true,
    summary: "代理环境变量缺失",
    items: [{ agent: "cursor", category: "env_proxy", cause: "no proxy", fix: "set proxy", command: "cursor-agent login" }],
  };

  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (c: any) => Promise<void>) { task = fn(cx); } };
  view.launchAgentToFixDroveProbeErrors(cx);
  await task;

  expect(ran[0].cmd).toBe("tab.new");
  expect(ran[0].args.space).toBe("sp-wsl");
  expect(ran[0].args.folder).toBe("");
  expect(ran[0].args.argv).toEqual(["claude", "--dangerously-skip-permissions"]);
  expect(ran[0].args.task).toContain("cursor");
  expect(ran[0].args.task).toContain("代理环境变量缺失");
  expect(closed).toBe(1);

  // When no space exists on the target machine, create a new space on that machine in its home directory.
  currentSpaces = [{ id: "sp-local", machine: null, active: true }];
  view.runProbeFixCommandInTerminal(["cursor-agent", "login"], "修复 cursor");
  expect(ran[1]).toEqual({
    cmd: "space.new",
    args: { name: "修复 cursor", tab: "修复 cursor", folder: "", argv: ["cursor-agent", "login"], machine: "ubuntu-wsl" },
  });
  expect(closed).toBe(2);
  expect(toasts.length).toBe(2);

  // On local machine, fix agent defaults working directory to local home().
  view.droveMachine = null;
  view.launchAgentToFixDroveProbeErrors(cx);
  await task;
  expect(ran[2].cmd).toBe("tab.new");
  expect(ran[2].args.space).toBe("sp-local");
  expect(ran[2].args.folder).toBe("/home/tester");
});

test("updateRemoteMachines merges live machine list and update status, marking unreachable or outdated as needs_update", () => {
  const view = new Settings();
  view.machineList = [
    { name: "linux-ok", target: "dev@10.0.0.1", version: "2.0.15", state: "connected", temporary: false },
    { name: "linux-old", target: "dev@10.0.0.2", version: "2.0.12", state: "connected", temporary: false },
    { name: "linux-down", target: "dev@10.0.0.3", version: "2.0.15", state: "unreachable", why: "ssh timed out", temporary: false },
    { name: "ephemeral", target: "dev@10.0.0.9", version: "2.0.10", state: "connected", temporary: true },
  ];

  const rows = view.updateRemoteMachines({
    core: { version: "2.0.15", ready: null },
    machines: [
      {
        name: "linux-old",
        target: "dev@10.0.0.2",
        version: "2.0.12",
        state: "needs_update",
        raw_state: "connected",
        ready: "2.0.15",
        needs_update: true,
        unreachable: false,
        updating: true,
        error: null,
      },
    ],
  });

  expect(rows.map((r: any) => r.name)).toEqual(["linux-ok", "linux-old", "linux-down"]);
  expect(rows[0]).toMatchObject({ name: "linux-ok", version: "2.0.15", state: "connected", needsUpdate: false, unreachable: false });
  expect(rows[1]).toMatchObject({ name: "linux-old", version: "2.0.12", state: "needs_update", ready: "2.0.15", needsUpdate: true, updating: true });
  expect(rows[2]).toMatchObject({ name: "linux-down", version: "2.0.15", state: "needs_update", rawState: "unreachable", needsUpdate: true, unreachable: true, why: "ssh timed out" });

  // Remote-only update opens available modal listing the outdated SSH remote host.
  const modal = view.buildUpdateModal({
    app: { version: "2.0.15", ready: null },
    ui: { version: "2.0.15", ready: null },
    core: { version: "2.0.15", ready: null },
    available: null,
  });
  expect(modal.kind).toBe("available");
  expect(modal.restart).toBe(false);
  expect(modal.items).toEqual([
    expect.objectContaining({ kind: "remote", machine: "linux-old", current: "2.0.12", next: "2.0.15", state: "needs_update" }),
  ]);
});

test("launchAiUpdate invokes a local CLI AI agent with GitHub release and failure context", async () => {
  const ran: Array<{ cmd: string; args: any }> = [];
  const toasts: any[] = [];
  let closed = 0;
  let currentSpaces: any[] = [
    { id: "sp-remote", machine: "linux-box", active: true },
    { id: "sp-local", machine: null, active: false },
  ];
  const currentUpdates: any = {
    app: { version: "2.0.12", ready: "2.0.13", error: "curl: (7) Failed to connect" },
    core: { version: "2.0.12", ready: "2.0.13", error: null, managed: null },
    machines: [
      {
        name: "linux-box",
        target: "dev@10.0.0.5",
        version: "2.0.11",
        state: "needs_update",
        raw_state: "unreachable",
        why: "ssh timed out",
        ready: "2.0.13",
        needs_update: true,
        unreachable: true,
      },
    ],
    releases_url: "https://github.com/herdr2/herdr2/releases/download/ota-latest",
    last_error: "curl: (7) Failed to connect",
  };
  let agentList: any[] = [
    { key: "claude", name: "Claude Code", installed: true, run: ["claude", "--dangerously-skip-permissions"] },
  ];
  const customValues = {
    ...values,
    spaces: () => currentSpaces,
    updates: () => currentUpdates,
    window: () => ({ platform: "linux" }),
    knownAgents: async () => agentList,
    run: (cmd: string, args: any) => { ran.push({ cmd, args }); },
    notify: (n: any) => { toasts.push(n); },
  };
  const customArgs = names.map((name) => customValues[name] ?? (() => undefined));
  const CustomSettings = new Function(
    ...names,
    source
      .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
      .replace("export default class Settings", "return class Settings"),
  )(...customArgs);

  const view = new CustomSettings();
  view.person = { language: "zh-CN" };
  view.close = () => { closed++; };

  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (c: any) => Promise<void>) { task = fn(cx); } };
  view.launchAiUpdate(cx, "curl: (7) Failed to connect");
  await task;

  expect(ran.length).toBe(1);
  expect(ran[0].cmd).toBe("tab.new");
  expect(ran[0].args.space).toBe("sp-local");
  expect(ran[0].args.argv).toEqual(["claude", "--dangerously-skip-permissions"]);
  expect(ran[0].args.task).toContain("https://github.com/herdr2/herdr2/releases");
  expect(ran[0].args.task).toContain("curl: (7) Failed to connect");
  expect(ran[0].args.task).toContain("linux-box");
  expect(ran[0].args.task).toContain("h2 machine update");
  expect(closed).toBe(1);
  expect(view.aiUpdateLaunching).toBe(false);

  // When no local space exists, opens a new local space.
  currentSpaces = [{ id: "sp-remote", machine: "linux-box", active: true }];
  view.launchAiUpdate(cx);
  await task;
  expect(ran[1].cmd).toBe("space.new");
  expect(ran[1].args.machine).toBeUndefined();

  // When no local CLI agent is installed, notifies with a helpful error and does not close settings.
  agentList = [];
  const prevClosed = closed;
  view.launchAiUpdate(cx);
  await task;
  expect(closed).toBe(prevClosed);
  expect(toasts.at(-1)?.level).toBe("error");
});

test("drove probe fix panel lets user select agent type, model, and effort and launches with chosen options", async () => {
  const ran: Array<{ cmd: string; args: any }> = [];
  const toasts: any[] = [];
  let closed = 0;
  const knownCalls: any[] = [];
  const currentSpaces: any[] = [
    { id: "sp-local", machine: null, active: true },
    { id: "sp-wsl", machine: "ubuntu-wsl", active: false },
  ];
  const customValues = {
    ...values,
    run: (cmd: string, args: any) => { ran.push({ cmd, args }); },
    notify: (n: any) => { toasts.push(n); },
    spaces: () => currentSpaces,
    knownAgents: async (opts: any) => {
      knownCalls.push(opts);
      return [
        { key: "claude", name: "Claude Code", installed: true, run: ["claude", "--dangerously-skip-permissions"] },
        { key: "codex", name: "Codex", installed: true, run: ["codex", "--no-daemon", "--dangerously-bypass-approvals-and-sandbox"] },
        { key: "antigravity", name: "Antigravity", installed: true, run: ["agy", "--dangerously-skip-permissions"] },
        { key: "cursor", name: "Cursor", installed: true, run: ["agent", "--yolo", "--trust"] },
      ];
    },
  };
  const customArgs = names.map((name) => customValues[name] ?? (() => undefined));
  const CustomSettings = new Function(
    ...names,
    source
      .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
      .replace("export default class Settings", "return class Settings"),
  )(...customArgs);

  const view = new CustomSettings();
  view.person = { language: "zh-CN" };
  view.close = () => { closed++; };
  view.droveMachine = "ubuntu-wsl";
  view.droveData = {
    agents: {
      fullProbe: true,
      agents: [
        { id: "claude", installed: true, models: ["claude-opus-4-6", "claude-sonnet-4-5"], error: null },
        { id: "codex", installed: true, models: ["gpt-5.4"], error: null },
        { id: "agy", installed: true, models: ["gemini-4-carbon-high", "gemini-4-neon"], error: null },
        { id: "cursor", installed: true, models: ["gpt-5.4", "grok-4.7-high"], error: "ACP probe timed out after 60s" },
      ],
    },
  };

  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (c: any) => Promise<void>) { task = fn(cx); } };

  // Opening the fix panel loads knownAgents for the target machine and deduplicates aliases (antigravity -> agy).
  view.toggleDroveProbeFixPanel(cx);
  await task;
  expect(view.droveProbeFixOpen).toBe(true);
  expect(knownCalls[0]).toEqual({ on: "ubuntu-wsl" });
  const opts = view.droveFixAgentOptions();
  expect(opts.filter((o: any) => o.id === "agy").length).toBe(1);
  expect(opts.find((o: any) => o.id === "agy")?.name).toBe("Antigravity");
  expect(opts.find((o: any) => o.id === "cursor")?.argv).toEqual(["cursor-agent", "--yolo", "--trust"]);
  expect(view.droveProbeFixAgent).toBe("claude");

  // Selecting codex + gpt-5.4 + xhigh launches codex with model and effort passed to tab.new.
  view.setDroveProbeFixAgent("codex", cx);
  view.setDroveProbeFixModel("gpt-5.4", cx);
  view.setDroveProbeFixEffort("xhigh", cx);
  view.launchAgentToFixDroveProbeErrors(cx);
  await task;
  expect(ran[0].cmd).toBe("tab.new");
  expect(ran[0].args.space).toBe("sp-wsl");
  expect(ran[0].args.argv).toEqual(["codex", "--no-daemon", "--dangerously-bypass-approvals-and-sandbox"]);
  expect(ran[0].args.model).toBe("gpt-5.4");
  expect(ran[0].args.effort).toBe("xhigh");
  expect(view.droveProbeFixOpen).toBe(false);
  expect(closed).toBe(1);
  expect(toasts[0].title).toContain("Codex");
  expect(toasts[0].title).toContain("gpt-5.4");

  // Switching to claude-sonnet-4-5 clamps xhigh down to high.
  view.setDroveProbeFixAgent("claude", cx);
  view.setDroveProbeFixEffort("xhigh", cx);
  expect(view.droveProbeFixEffort).toBe("xhigh");
  view.setDroveProbeFixModel("claude-sonnet-4-5", cx);
  expect(view.droveProbeFixEffort).toBe("high");

  // Embedded-effort agy model disables separate effort flag and omits effort from tab.new args.
  view.setDroveProbeFixAgent("antigravity", cx);
  expect(view.droveProbeFixAgent).toBe("agy");
  view.setDroveProbeFixModel("gemini-4-carbon-high", cx);
  expect(view.droveFixEffortDisabledReason("agy", "gemini-4-carbon-high")).toBeTruthy();
  view.launchAgentToFixDroveProbeErrors(cx);
  await task;
  expect(ran[1].args.argv).toEqual(["agy", "--dangerously-skip-permissions"]);
  expect(ran[1].args.model).toBe("gemini-4-carbon-high");
  expect(ran[1].args.effort).toBeUndefined();

  // Cursor with base model passes normalized cursor-agent argv + model + effort; without model disables effort.
  view.setDroveProbeFixAgent("cursor", cx);
  expect(view.droveFixEffortDisabledReason("cursor", null)).toBeTruthy();
  view.setDroveProbeFixModel("gpt-5.4", cx);
  expect(view.droveFixEffortDisabledReason("cursor", "gpt-5.4")).toBeNull();
  view.setDroveProbeFixEffort("medium", cx);
  view.launchAgentToFixDroveProbeErrors(cx);
  await task;
  expect(ran[2].args.argv).toEqual(["cursor-agent", "--yolo", "--trust"]);
  expect(ran[2].args.model).toBe("gpt-5.4");
  expect(ran[2].args.effort).toBe("medium");
});

test("AI update panel lets user choose local agent, model, and effort and guards prompt against mid-update interruption", async () => {
  const ran: Array<{ cmd: string; args: any }> = [];
  const toasts: any[] = [];
  const knownCalls: any[] = [];
  const hcCalls: string[][] = [];
  let closed = 0;
  const currentSpaces: any[] = [
    { id: "sp-remote", machine: "ubuntu-wsl", active: true },
    { id: "sp-local", machine: null, active: false },
  ];
  const currentUpdates: any = {
    app: { version: "2.0.13", ready: "2.0.14", error: null },
    core: { version: "2.0.13", ready: "2.0.14", error: null, managed: null },
    machines: [],
    releases_url: "https://github.com/herdr2/herdr2/releases/download/ota-latest",
    last_error: null,
  };
  const customValues = {
    ...values,
    run: (cmd: string, args: any) => { ran.push({ cmd, args }); },
    notify: (n: any) => { toasts.push(n); },
    spaces: () => currentSpaces,
    updates: () => currentUpdates,
    window: () => ({ platform: "linux" }),
    knownAgents: async (opts: any) => {
      knownCalls.push(opts);
      return [
        { key: "claude", name: "Claude Code", installed: true, run: ["claude", "--dangerously-skip-permissions"] },
        { key: "codex", name: "Codex", installed: true, run: ["codex", "--no-daemon", "--dangerously-bypass-approvals-and-sandbox"] },
        { key: "antigravity", name: "Antigravity", installed: true, run: ["agy", "--dangerously-skip-permissions"] },
        { key: "cursor", name: "Cursor", installed: true, run: ["agent", "--yolo", "--trust"] },
      ];
    },
    hc: async (args: string[]) => {
      hcCalls.push(args);
      if (args[0] === "drove" && args[1] === "probe") {
        return {
          code: 0,
          stdout: JSON.stringify({
            fullProbe: true,
            agents: [
              { id: "claude", installed: true, models: ["claude-opus-4-6", "claude-sonnet-4-5"], error: null },
              { id: "codex", installed: true, models: ["gpt-5.4"], error: null },
              { id: "agy", installed: true, models: ["gemini-4-carbon-high", "gemini-4-neon"], error: null },
              { id: "cursor", installed: true, models: ["gpt-5.4"], error: null },
            ],
          }),
          stderr: "",
        };
      }
      return { code: 0, stdout: "{}", stderr: "" };
    },
  };
  const customArgs = names.map((name) => customValues[name] ?? (() => undefined));
  const CustomSettings = new Function(
    ...names,
    source
      .replace(/^import\s+[\s\S]*?from\s+["'][^"']+["'];?\s*\n?/gm, "")
      .replace("export default class Settings", "return class Settings"),
  )(...customArgs);

  const view = new CustomSettings();
  view.person = { language: "zh-CN" };
  view.close = () => { closed++; };
  // Even if Drove tab is currently pointed at a remote machine, AI Update must inspect local agents/models only.
  view.droveMachine = "ubuntu-wsl";
  view.droveData = {
    agents: {
      fullProbe: true,
      agents: [{ id: "codex", installed: true, models: ["remote-only-model"], error: null }],
    },
  };
  view.updateModal = { kind: "error", message: "TLS handshake failed" };

  let task: Promise<void> | null = null;
  const cx = { notify() {}, spawn(fn: (c: any) => Promise<void>) { task = fn(cx); } };

  view.openAiUpdatePanel(cx, "TLS handshake failed");
  await task;

  expect(view.aiUpdateOpen).toBe(true);
  expect(view.updateModal).toBeNull();
  expect(view.aiUpdateExtraError).toBe("TLS handshake failed");
  expect(knownCalls[0]).toEqual({});
  expect(hcCalls[0]).toEqual(["drove", "probe", "--json"]);
  expect(hcCalls[0]).not.toContain("--on");

  const opts = view.aiUpdateAgentOptions();
  expect(opts.filter((o: any) => o.id === "agy").length).toBe(1);
  expect(opts.find((o: any) => o.id === "codex")?.models.map((m: any) => m.id)).toEqual(["gpt-5.4"]);
  expect(view.aiUpdateAgent).toBe("claude");

  // Choose codex + gpt-5.4 + xhigh and launch AI update.
  view.setAiUpdateAgent("codex", cx);
  view.setAiUpdateModel("gpt-5.4", cx);
  view.setAiUpdateEffort("xhigh", cx);
  view.launchAiUpdate(cx);
  await task;

  expect(ran[0].cmd).toBe("tab.new");
  expect(ran[0].args.space).toBe("sp-local");
  expect(ran[0].args.folder).toBe("/home/tester");
  expect(ran[0].args.argv).toEqual(["codex", "--no-daemon", "--dangerously-bypass-approvals-and-sandbox"]);
  expect(ran[0].args.model).toBe("gpt-5.4");
  expect(ran[0].args.effort).toBe("xhigh");
  expect(ran[0].args.task).toContain("TLS handshake failed");
  expect(ran[0].args.task).toContain("PTY holder");
  expect(ran[0].args.task).toContain("h2 server stop");
  expect(view.aiUpdateOpen).toBe(false);
  expect(closed).toBe(1);
  expect(toasts[0].title).toContain("Codex");
  expect(toasts[0].title).toContain("gpt-5.4");

  // Embedded-effort agy model omits separate effort flag on launch.
  view.setAiUpdateAgent("antigravity", cx);
  expect(view.aiUpdateAgent).toBe("agy");
  view.setAiUpdateModel("gemini-4-carbon-high", cx);
  view.setAiUpdateEffort("high", cx);
  view.launchAiUpdate(cx);
  await task;
  expect(ran[1].args.argv).toEqual(["agy", "--dangerously-skip-permissions"]);
  expect(ran[1].args.model).toBe("gemini-4-carbon-high");
  expect(ran[1].args.effort).toBeUndefined();

  // Switching to claude-sonnet-4-5 clamps xhigh down to high.
  view.setAiUpdateAgent("claude", cx);
  view.setAiUpdateEffort("xhigh", cx);
  expect(view.aiUpdateEffort).toBe("xhigh");
  view.setAiUpdateModel("claude-sonnet-4-5", cx);
  expect(view.aiUpdateEffort).toBe("high");
});
