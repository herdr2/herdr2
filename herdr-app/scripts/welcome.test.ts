import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import {
  PAGES,
  AI_PAGE,
  AGENTS_PAGE,
  ACP_PERMISSIONS,
  AI_PROVIDERS,
  DEEPSEEK_PRESET,
  PANELS,
  SHORTCUT_HIGHLIGHTS,
  resolveLang,
  clampPage,
  resolveEnabledPanels,
  formatKeyChord,
  boundShortcut,
  normalizeMachines,
  summarizeAiStatus,
  normalizeFetchedModels,
  isAiGateSatisfied,
  canAccessPage,
  agentCustomizePrompt,
  fmtVersion,
  findLayoutNode,
  normalizeAdapterStatus,
  normalizeAgentProbe,
  summarizeDetectedAgents,
  formatShortProbeError,
} from "../defaults/frame/welcome/model.js";

test("welcome uses 5 compact pages including Agents & ACP setup with 6 core shortcuts", () => {
  expect(PAGES).toBe(5);
  expect(AI_PAGE).toBe(3);
  expect(AGENTS_PAGE).toBe(4);
  expect(ACP_PERMISSIONS.map((p) => p.id)).toEqual(["approve-reads", "approve-all", "ask", "deny-all"]);
  expect(PANELS.length).toBe(3);
  expect(SHORTCUT_HIGHLIGHTS.length).toBe(6);
});

test("clampPage bounds page indices to 1..5", () => {
  expect(clampPage(0)).toBe(1);
  expect(clampPage(-3)).toBe(1);
  expect(clampPage(3)).toBe(3);
  expect(clampPage(4)).toBe(4);
  expect(clampPage(99)).toBe(5);
  expect(clampPage("abc")).toBe(1);
});

test("resolveLang detects zh-CN and defaults to en", () => {
  expect(resolveLang({ language: "zh-CN" })).toBe("zh-CN");
  expect(resolveLang({ language: "zh" })).toBe("zh-CN");
  expect(resolveLang({ language: "en" })).toBe("en");
  expect(resolveLang(null, "zh-CN")).toBe("zh-CN");
  expect(resolveLang({})).toBe("en");
});

test("resolveEnabledPanels preserves defaults and migrates checkpoints once", () => {
  expect(resolveEnabledPanels(null, false)).toEqual(["files", "changes", "checkpoints"]);
  expect(resolveEnabledPanels(["files"], false)).toEqual(["files", "checkpoints"]);
  expect(resolveEnabledPanels(["files"], true)).toEqual(["files"]);
});

test("formatKeyChord and boundShortcut format macOS and Linux/Windows chords", () => {
  expect(formatKeyChord("ctrl-shift-p", false)).toBe("Ctrl+Shift+P");
  expect(formatKeyChord("cmd-shift-p", true)).toBe("⇧⌘P");
  expect(formatKeyChord("ctrl-alt-enter", false)).toBe("Ctrl+Alt+Enter");

  const keyList = [
    { section: "global", keys: "ctrl-shift-p", action: "app.palette" },
    { section: "global", keys: "ctrl-shift-h", action: "agent.handoff" },
  ];
  expect(boundShortcut(keyList, "app.palette", false)).toBe("Ctrl+Shift+P");
  expect(boundShortcut(keyList, "agent.briefing", false, "ctrl-shift-i")).toBe("Ctrl+Shift+I");
});

test("normalizeMachines filters out temporary sandboxes and handles malformed output", () => {
  const stdout = JSON.stringify({
    machines: [
      { name: "wsl", state: "connected", temporary: false },
      { name: "tmp-box", state: "connected", temporary: true },
      { name: "gpu", state: "needs_login" },
    ],
  });
  expect(normalizeMachines(stdout).map((m) => m.name)).toEqual(["wsl", "gpu"]);
  expect(normalizeMachines("not-json")).toEqual([]);
});

test("summarizeAiStatus extracts configured flag, model, baseUrl, keyMasked, and availableModels", () => {
  const stdout = JSON.stringify({
    configured: true,
    enabled: true,
    model: " gpt-5.4 ",
    baseUrl: "https://api.example.com/v1",
    keyMasked: "sk-1***90",
    availableModels: ["gpt-5.4", "deepseek-chat"],
  });
  expect(summarizeAiStatus(stdout)).toEqual({
    configured: true,
    enabled: true,
    model: "gpt-5.4",
    baseUrl: "https://api.example.com/v1",
    keyMasked: "sk-1***90",
    availableModels: ["gpt-5.4", "deepseek-chat"],
  });
  expect(
    summarizeAiStatus(
      JSON.stringify({
        configured: true,
        enabled: true,
        model: "claude-sonnet-4-6",
        baseUrl: "https://api.anthropic.com",
        protocol: "anthropic",
        availableModels: ["claude-sonnet-4-6"],
      }),
    ),
  ).toEqual({
    configured: true,
    enabled: true,
    model: "claude-sonnet-4-6",
    baseUrl: "https://api.anthropic.com",
    keyMasked: null,
    availableModels: ["claude-sonnet-4-6"],
    protocol: "anthropic",
  });
  expect(summarizeAiStatus("bad")).toBeNull();
});

test("agentCustomizePrompt points to ~/.config/herdr2-app/AGENTS.md in both languages", () => {
  expect(agentCustomizePrompt("en")).toContain("~/.config/herdr2-app/AGENTS.md");
  expect(agentCustomizePrompt("zh-CN")).toContain("~/.config/herdr2-app/AGENTS.md");
  expect(agentCustomizePrompt("en")).not.toContain("~/.config/herdr-app/");
});

test("fmtVersion normalizes leading v and findLayoutNode locates nested nodes", () => {
  expect(fmtVersion("0.3.1")).toBe("v0.3.1");
  expect(fmtVersion("v0.3.1")).toBe("v0.3.1");
  expect(fmtVersion("")).toBe("");
  const tree = { id: "root", children: [{ id: "work", children: [{ id: "panel", active: "files" }] }] };
  expect(findLayoutNode(tree, "panel")).toEqual({ id: "panel", active: "files" });
  expect(findLayoutNode(tree, "missing")).toBeNull();
});

test("Welcome view persists appearance, switches language, toggles panels, and records completion", async () => {
  const source = await readFile(new URL("../defaults/frame/welcome/main.js", import.meta.url), "utf8");
  const modelMod = await import("../defaults/frame/welcome/model.js");

  const ran: [string, any][] = [];
  const closed: string[] = [];
  const sharedState: Record<string, any> = {};
  const disk: { raw: string | null } = { raw: '{"zoom":1.1}' };
  const pickedFolder = "/work/demo";

  const env: Record<string, any> = {
    View: class {},
    div: () => ({}),
    svg: () => ({}),
    image: () => ({}),
    v_flex: () => ({}),
    h_flex: () => ({}),
    ...modelMod,
    spaces: () => [],
    run: (cmd: string, args?: any) => { ran.push([cmd, args]); },
    theme: () => ({}),
    props: () => ({ overlay: "ov-welcome" }),
    closeOverlay: (id: string) => { closed.push(id); },
    shared: (k: string) => sharedState[k],
    setShared: (k: string, v: any) => { sharedState[k] = v; },
    h2: async () => ({ code: 0, stdout: "{}" }),
    hc: async () => ({ code: 0, stdout: "{}" }),
    appHome: () => "/home/.config/herdr2-app",
    readTextFile: async () => disk.raw,
    writeTextFile: async (_p: string, text: string) => { disk.raw = text; },
    settings: () => ({ language: "en", zoom: 1.1 }),
    layout: () => ({ id: "root", children: [{ id: "panel", children: [{ id: "panel-body", active: "files" }] }] }),
    keys: () => [],
    updates: () => ({ app: { version: "0.3.0" }, core: { version: "0.3.0" } }),
    appWindow: () => ({ platform: "linux" }),
    pickFolder: async () => pickedFolder,
    checking: () => true,
  };

  const body = source
    .replace(/^import[\s\S]*?from\s+["'][^"']+["'];\s*\n/gm, "")
    .replace("export default class Welcome", "return class Welcome");
  const Welcome = new Function(...Object.keys(env), body)(...Object.values(env));

  const view = new Welcome();
  const cx = {
    notify: () => {},
    spawn: (fn: (cx: any) => Promise<void>) => { void fn({ notify: () => {} }); },
  };

  // 1. Language switch runs app.language immediately
  view.setLanguage("zh-CN", cx);
  expect(ran).toContainEqual(["app.language", { language: "zh-CN" }]);
  expect(view.lang()).toBe("zh-CN");

  // 2. Appearance writes settings.json without clobbering zoom
  view.setAppearance("dark", cx);
  await new Promise((r) => setTimeout(r, 10));
  expect(JSON.parse(disk.raw!)).toEqual({ zoom: 1.1, appearance: "dark" });

  // 3. Toggle panel updates workspace.panels cleanly
  view.togglePanel("files");
  expect(sharedState["workspace.panels"]).toEqual(["changes", "checkpoints"]);
  expect(sharedState["workspace.panels_has_checkpoints"]).toBe(true);
  expect(ran).toContainEqual(["layout.hide", { id: "panel" }]);

  // 4. Open project folder calls pickFolder -> space.new with folder & marks welcomed
  view.openProjectFolder(cx);
  await new Promise((r) => setTimeout(r, 10));
  expect(sharedState["frame.welcomed"]).toBe("finished");
  expect(closed).toContain("ov-welcome");
  expect(ran).toContainEqual(["space.new", { folder: "/work/demo" }]);

  // 5. Ensure no legacy hc / ~/.config/herdr-app references or forced scrollbar remain
  expect(source).not.toContain("hc machine login");
  expect(source).not.toContain("~/.config/herdr-app");
  expect(source).not.toContain("Scrollbar.vertical");
});

test("Welcome AI config: two providers only, no default model, fetch models, optional key, and skip", async () => {
  expect(DEEPSEEK_PRESET.baseUrl).toBe("https://api.deepseek.com/v1");
  expect((DEEPSEEK_PRESET as any).model).toBeUndefined();
  expect(AI_PROVIDERS.map((p) => p.id)).toEqual(["deepseek", "custom"]);
  expect(AI_PAGE).toBe(3);

  // Unverified AI blocks proceeding past AI_PAGE unless skipped/bypassed
  expect(isAiGateSatisfied(null, null, false)).toBe(false);
  expect(isAiGateSatisfied({ configured: true }, null, false)).toBe(false);
  expect(isAiGateSatisfied({ configured: true }, { ok: false }, false)).toBe(false);
  expect(canAccessPage(4, 3, false)).toBe(3);
  expect(canAccessPage(2, 1, false)).toBe(2);

  // Verified AI unlocks Page 4 and completion
  expect(isAiGateSatisfied({ configured: true }, { ok: true }, false)).toBe(true);
  expect(canAccessPage(4, 3, true)).toBe(4);

  // Skip / bypass flag unlocks when explicitly set
  expect(isAiGateSatisfied(null, null, true)).toBe(true);

  // normalizeFetchedModels deduplicates and extracts errors cleanly
  expect(normalizeFetchedModels(JSON.stringify({ ok: true, models: ["gpt-5.4", " gpt-5.4 ", "claude-sonnet"] }))).toEqual({
    ok: true,
    models: ["gpt-5.4", "claude-sonnet"],
    error: null,
    latencyMs: 0,
  });
  expect(normalizeFetchedModels(JSON.stringify({ ok: false, error: "HTTP 401: Unauthorized" }))).toEqual({
    ok: false,
    models: [],
    error: "HTTP 401: Unauthorized",
    latencyMs: 0,
  });

  // Behavioral test: keyless local Magpie flow + skip flow
  const source = await readFile(new URL("../defaults/frame/welcome/main.js", import.meta.url), "utf8");
  const modelMod = await import("../defaults/frame/welcome/model.js");

  const makeInput = () => {
    let val = "";
    return {
      value: () => val,
      set_value: (v: string) => { val = v; },
      set_masked: () => {},
    };
  };

  const writtenFiles: Record<string, string> = {};
  const coreCalls: { args: string[]; payload: any }[] = [];
  const env: Record<string, any> = {
    View: class {},
    div: () => ({}),
    svg: () => ({}),
    image: () => ({}),
    v_flex: () => ({}),
    h_flex: () => ({}),
    InputState: { new: () => makeInput() },
    Input: { new: () => ({}) },
    Popup: { new: () => ({}) },
    ...modelMod,
    spaces: () => [],
    run: () => {},
    theme: () => ({}),
    props: () => ({ overlay: "ov-welcome" }),
    closeOverlay: () => {},
    shared: () => undefined,
    setShared: () => {},
    h2: async (args: string[]) => {
      const fileIdx = args.indexOf("--file");
      const payload = fileIdx >= 0 ? JSON.parse(writtenFiles[args[fileIdx + 1]] ?? "{}") : null;
      coreCalls.push({ args, payload });
      if (args[0] === "ai" && args[1] === "models") {
        return { code: 0, stdout: JSON.stringify({ ok: true, models: ["magpie-model-a", "magpie-model-b"], latencyMs: 12 }) };
      }
      if (args[0] === "ai" && args[1] === "test") {
        return { code: 0, stdout: JSON.stringify({ ok: true, discoveredModel: payload.model, availableModels: ["magpie-model-a", "magpie-model-b"], latencyMs: 25 }) };
      }
      if (args[0] === "ai" && args[1] === "settings-set") {
        return { code: 0, stdout: JSON.stringify({ configured: true, enabled: true, baseUrl: payload.baseUrl, model: payload.model, availableModels: ["magpie-model-a", "magpie-model-b"] }) };
      }
      return { code: 0, stdout: "{}" };
    },
    hc: async () => ({ code: 0, stdout: "{}" }),
    appHome: () => "/home/.config/herdr2-app",
    readTextFile: async () => "{}",
    writeTextFile: async (p: string, text: string) => { writtenFiles[p] = text; },
    settings: () => ({ language: "en", zoom: 1 }),
    layout: () => ({ id: "root", children: [] }),
    keys: () => [],
    updates: () => ({ app: { version: "0.3.0" }, core: { version: "0.3.0" } }),
    appWindow: () => ({ platform: "linux" }),
    pickFolder: async () => null,
    checking: () => false,
  };

  const body = source
    .replace(/^import[\s\S]*?from\s+["'][^"']+["'];\s*\n/gm, "")
    .replace("export default class Welcome", "return class Welcome");
  const Welcome = new Function(...Object.keys(env), body)(...Object.values(env));

  const cx = {
    notify: () => {},
    focus_handle: () => ({ focus: () => {} }),
    timer: { after: () => {}, every: () => {} },
    spawn: (fn: (cx: any) => Promise<void>) => { void fn({ notify: () => {} }); },
  };

  // Scenario A: Local keyless Magpie flow
  const view = new Welcome();
  view.init({}, cx);
  await new Promise((r) => setTimeout(r, 10));
  expect(view.aiSelectedModel).toBe("");
  expect(view.aiReady()).toBe(false);

  // Switch to Custom OpenAI-compatible clears DeepSeek URL and keeps model empty
  view.selectAiProvider("custom", cx);
  expect(view.aiUrlInput.value()).toBe("");
  expect(view.aiSelectedModel).toBe("");

  // Enter local Magpie URL without API key
  view.aiUrlInput.set_value("http://127.0.0.1:8080/v1");
  expect(view.aiKeyInput.value()).toBe("");

  // Saving without selecting a model fails with model selection prompt (NOT API key prompt)
  view.saveAndTestAi(cx);
  expect(view.aiError).toContain("model");
  expect(view.aiError.toLowerCase()).not.toContain("key");

  // Fetch models without API key succeeds and opens dropdown with no default preselected
  view.fetchAiModels(cx);
  await new Promise((r) => setTimeout(r, 15));
  expect(view.aiModels).toEqual(["magpie-model-a", "magpie-model-b"]);
  expect(view.aiSelectedModel).toBe("");
  expect(view.aiModelMenuOpen).toBe(true);
  const modelsCall = coreCalls.find((c) => c.args[1] === "models");
  expect(modelsCall?.payload).toEqual({ baseUrl: "http://127.0.0.1:8080/v1" });

  // User picks a model from dropdown and saves/tests with empty API key
  view.selectAiModel("magpie-model-b", cx);
  expect(view.aiSelectedModel).toBe("magpie-model-b");
  expect(view.aiModelMenuOpen).toBe(false);

  view.saveAndTestAi(cx);
  await new Promise((r) => setTimeout(r, 15));
  expect(view.aiError).toBeNull();
  expect(view.aiReady()).toBe(true);
  const saveCall = coreCalls.find((c) => c.args[1] === "settings-set");
  expect(saveCall?.payload).toEqual({ enabled: true, baseUrl: "http://127.0.0.1:8080/v1", model: "magpie-model-b" });

  // Scenario B: Skip button unlocks Page 4 (Agents & ACP) directly without configuring AI
  const view2 = new Welcome();
  view2.init({}, cx);
  await new Promise((r) => setTimeout(r, 10));
  view2.page = AI_PAGE;
  expect(view2.aiReady()).toBe(false);
  view2.skipAi(cx);
  await new Promise((r) => setTimeout(r, 10));
  expect(view2.aiSkipped).toBe(true);
  expect(view2.aiReady()).toBe(true);
  expect(view2.page).toBe(AGENTS_PAGE);
});

test("Welcome Agents & ACP step summarizes detected agents, installs adapters, and persists ACP config", async () => {
  const adapterStatus = normalizeAdapterStatus(
    JSON.stringify({
      adapters_dir: "/home/.local/share/herdr2/adapters",
      auto_install: true,
      registry_url: "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json",
      default_permissions: "approve-reads",
      inject_mcp: true,
      adapters: [
        { preset_id: "claude", binary_name: "claude-agent-acp", companion_cli: "claude", companion_installed: true, installed: true, source: "managed", state: "ready", version: "0.9.0" },
        { preset_id: "codex", binary_name: "codex-acp", companion_cli: "codex", companion_installed: true, installed: false, source: "missing", state: "missing" },
      ],
    }),
  );
  const probe = normalizeAgentProbe(
    JSON.stringify({
      fullProbe: true,
      agents: [
        { id: "claude", installed: true, models: ["claude-opus-4-6", "claude-sonnet-4-6"] },
        { id: "cursor", installed: true, models: [], error: "ACP initialize failed\nstderr:\nPlease login" },
      ],
    }),
  );
  const known = [
    { key: "claude", name: "Claude Code", installed: true },
    { key: "codex", name: "Codex", installed: true },
    { key: "antigravity", name: "Antigravity", installed: true },
  ];
  const summary = summarizeDetectedAgents(known, probe, adapterStatus);
  expect(summary.cliCount).toBe(3);
  expect(summary.acpReadyCount).toBe(1);
  expect(summary.acpErrorCount).toBe(1);
  expect(summary.missingAdapters.map((a: any) => a.preset_id)).toEqual(["codex"]);
  expect(summary.rows.find((r: any) => r.id === "agy")?.name).toBe("Antigravity");
  expect(formatShortProbeError("ACP initialize failed\nstderr:\nPlease login")).toBe("ACP initialize failed · Please login");

  const source = await readFile(new URL("../defaults/frame/welcome/main.js", import.meta.url), "utf8");
  const modelMod = await import("../defaults/frame/welcome/model.js");
  const h2Calls: string[][] = [];
  const env: Record<string, any> = {
    View: class {},
    div: () => ({}),
    svg: () => ({}),
    image: () => ({}),
    v_flex: () => ({}),
    h_flex: () => ({}),
    InputState: { new: () => ({ value: () => "", set_value: () => {}, set_masked: () => {} }) },
    Input: { new: () => ({}) },
    Popup: { new: () => ({}) },
    ...modelMod,
    spaces: () => [],
    run: () => {},
    theme: () => ({}),
    props: () => ({ overlay: "ov-welcome" }),
    closeOverlay: () => {},
    shared: () => undefined,
    setShared: () => {},
    knownAgents: async () => known,
    h2: async (args: string[]) => {
      h2Calls.push(args);
      if (args[0] === "adapter" && args[1] === "status") {
        return {
          code: 0,
          stdout: JSON.stringify({
            auto_install: true,
            default_permissions: "approve-reads",
            inject_mcp: true,
            adapters: [
              { preset_id: "codex", binary_name: "codex-acp", companion_cli: "codex", companion_installed: true, installed: false, source: "missing", state: "missing" },
            ],
          }),
        };
      }
      if (args[0] === "adapter" && args[1] === "install") {
        return {
          code: 0,
          stdout: JSON.stringify({ results: [{ preset_id: "codex", action: "installed", version: "0.4.1" }] }),
        };
      }
      if (args[0] === "adapter" && args[1] === "config") {
        return {
          code: 0,
          stdout: JSON.stringify({
            auto_install: false,
            default_permissions: "approve-all",
            inject_mcp: false,
            adapters: [],
          }),
        };
      }
      if (args[0] === "drove" && args[1] === "probe") {
        return { code: 0, stdout: JSON.stringify({ fullProbe: true, agents: [{ id: "codex", installed: true, models: ["gpt-5.4"] }] }) };
      }
      return { code: 0, stdout: "{}" };
    },
    hc: async () => ({ code: 0, stdout: "{}" }),
    appHome: () => "/home/.config/herdr2-app",
    readTextFile: async () => "{}",
    writeTextFile: async () => {},
    settings: () => ({ language: "en", zoom: 1 }),
    layout: () => ({ id: "root", children: [] }),
    keys: () => [],
    updates: () => ({}),
    appWindow: () => ({ platform: "linux" }),
    pickFolder: async () => null,
    checking: () => false,
  };

  const body = source
    .replace(/^import[\s\S]*?from\s+["'][^"']+["'];\s*\n/gm, "")
    .replace("export default class Welcome", "return class Welcome");
  const Welcome = new Function(...Object.keys(env), body)(...Object.values(env));
  const pending: Promise<void>[] = [];
  const cx = {
    notify: () => {},
    focus_handle: () => ({ focus: () => {} }),
    timer: { after: () => {}, every: () => {} },
    spawn: (fn: (c: any) => Promise<void>) => { pending.push(fn({ notify: () => {} })); },
  };

  const view = new Welcome();
  view.init({}, cx);
  await Promise.all(pending.splice(0));

  view.installAdapters(cx, "detected", "codex");
  await Promise.all(pending.splice(0));
  expect(h2Calls).toContainEqual(["adapter", "install", "codex", "--json"]);
  expect(view.agentsNotice).toContain("codex");
  expect(view.adapterBusy).toBe(false);

  view.saveAcpConfig({ autoInstall: false, defaultPermissions: "approve-all", injectMcp: false }, cx);
  await Promise.all(pending.splice(0));
  expect(h2Calls).toContainEqual([
    "adapter",
    "config",
    "--auto-install",
    "false",
    "--default-permissions",
    "approve-all",
    "--inject-mcp",
    "false",
    "--json",
  ]);
  expect(view.adapterStatus?.autoInstall).toBe(false);
  expect(view.adapterStatus?.defaultPermissions).toBe("approve-all");
  expect(view.adapterStatus?.injectMcp).toBe(false);
});
