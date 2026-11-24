import { describe, test, expect } from "bun:test";
import {
  isSyntheticCaller,
  parseUserEnvelope,
  splitMarkdownBlocks,
  cleanTerminalSnapshot,
  buildConversationItems,
  summarizeActivityGroup,
  summarizeToolTitle,
  normalizeThoughtText,
  thoughtPreview,
  parsePlanItems,
  parseTerminalShellBlocks,
  buildPtySessionBadges,
  toolIconPath,
  shortenPath,
  normalizeConfigOptions,
  filterConfigChoices,
  parseToolDetail,
  extractTurnChangedFiles,
  extractTurnFilePatch,
  resolveChangedFilePath,
  isAbsoluteFilePath,
  resolveEffortVisual,
  normalizeInboxItems,
  resolvePeerAddress,
} from "../defaults/mods/peek/acp_model.js";

describe("parseUserEnvelope", () => {
  test("strips <h2 from=...> wrapper and preserves multi-line UTF-8 body", () => {
    const raw = `<h2 from="t8" reach="h2 send t8 when done or stuck">请联网检索 GLM-5.5\n重点包含：\n1) 发布时间\n2) 模型规格</h2>`;
    const parsed = parseUserEnvelope(raw, "t8");
    expect(parsed.from).toBe("t8");
    expect(parsed.reach).toBe("h2 send t8 when done or stuck");
    expect(parsed.body).toBe("请联网检索 GLM-5.5\n重点包含：\n1) 发布时间\n2) 模型规格");
  });

  test("extracts from attribute when entry.from is absent and unwraps nested envelopes", () => {
    const raw = `<h2 from="t3"><h2 from="t3">Check auth middleware</h2></h2>`;
    const parsed = parseUserEnvelope(raw, null);
    expect(parsed.from).toBe("t3");
    expect(parsed.body).toBe("Check auth middleware");
  });

  test("returns plain user text untouched", () => {
    const parsed = parseUserEnvelope("hello agent\nfix the build", null);
    expect(parsed.from).toBeNull();
    expect(parsed.body).toBe("hello agent\nfix the build");
  });

  test("ignores synthetic ppid caller ids so human messages are never marked delegated", () => {
    expect(isSyntheticCaller("ppid:42736")).toBe(true);
    expect(isSyntheticCaller("d9152abb89f9af269de4f9e537b55cc0/ppid:42736")).toBe(true);
    expect(isSyntheticCaller("t8")).toBe(false);
    expect(isSyntheticCaller("wsl/a2")).toBe(false);

    const parsed = parseUserEnvelope("你现在使用的是什么大模型？", "d9152abb89f9af269de4f9e537b55cc0/ppid:42736");
    expect(parsed.from).toBeNull();
    expect(parsed.body).toBe("你现在使用的是什么大模型？");

    const items = buildConversationItems([
      { kind: "user", turn: 1, from: "d9152abb89f9af269de4f9e537b55cc0/ppid:42736", text: "你现在使用的是什么大模型？", ts_ms: 10 },
      { kind: "assistant", turn: 1, text: "GLM-5.3-Flash", ts_ms: 20 },
    ]);
    expect(items[0]).toMatchObject({
      kind: "user",
      from: null,
      isDelegated: false,
      body: "你现在使用的是什么大模型？",
    });

    const queued = normalizeInboxItems([
      { id: 1, text: "follow up", from: "d9152abb89f9af269de4f9e537b55cc0/ppid:42736", queued_ms: 1700000000000 },
    ]);
    expect(queued[0]).toMatchObject({
      from: null,
      fromLabel: null,
      body: "follow up",
    });
  });
});

describe("splitMarkdownBlocks", () => {
  test("splits mixed prose and fenced code blocks while preserving languages", () => {
    const md = [
      "# Summary",
      "",
      "Here is the patch:",
      "",
      "```rust",
      "fn main() {",
      "    println!(\"ok\");",
      "}",
      "```",
      "",
      "Run verification:",
      "",
      "```bash",
      "cargo test",
      "```",
    ].join("\n");

    const blocks = splitMarkdownBlocks(md);
    expect(blocks).toEqual([
      { type: "prose", text: "# Summary\n\nHere is the patch:" },
      { type: "code", lang: "rust", code: "fn main() {\n    println!(\"ok\");\n}", closed: true },
      { type: "prose", text: "Run verification:" },
      { type: "code", lang: "bash", code: "cargo test", closed: true },
    ]);
  });

  test("handles unclosed streaming code fences", () => {
    const blocks = splitMarkdownBlocks("Streaming:\n```ts\nconst x = 1;");
    expect(blocks).toEqual([
      { type: "prose", text: "Streaming:" },
      { type: "code", lang: "ts", code: "const x = 1;", closed: false },
    ]);
  });
});

describe("buildConversationItems", () => {
  test("groups real multi-turn ACP entries into T3 Code items", () => {
    const entries = [
      {
        kind: "user",
        turn: 1,
        from: "t8",
        text: '<h2 from="t8" reach="h2 send t8">Inspect server routes</h2>',
        ts_ms: 1794884405000,
      },
      { kind: "thought", turn: 1, text: "Need to read router setup.", ts_ms: 1794884406000 },
      { kind: "tool_call", turn: 1, id: "tc-1", title: "Read src/router.rs", tool_kind: "read", status: "completed", ts_ms: 1794884407000 },
      { kind: "tool_call", turn: 1, id: "tc-2", title: "cargo check", tool_kind: "execute", status: "completed", ts_ms: 1794884408000 },
      { kind: "plan", turn: 1, items: ["Read router", "Check middleware"], ts_ms: 1794884409000 },
      {
        kind: "assistant",
        turn: 1,
        text: "All routes look good.\n\n```rust\nroute(\"/health\")\n```",
        ts_ms: 1794884410000,
      },
      { kind: "stderr", text: "warning: unused var", ts_ms: 1794884411000 },
      { kind: "stderr", text: "warning: dead code", ts_ms: 1794884412000 },
      { kind: "user", turn: 2, text: "Now add tests", ts_ms: 1794884420000 },
      { kind: "tool_call", turn: 2, id: "tc-3", title: "Edit tests/router.rs", tool_kind: "edit", status: "in_progress", ts_ms: 1794884421000 },
    ];

    const items = buildConversationItems(entries, "", true);
    expect(items.map((x) => x.kind)).toEqual([
      "user",
      "activity_group",
      "assistant",
      "stderr_group",
      "user",
      "activity_group",
    ]);

    const firstUser = items[0] as any;
    expect(firstUser.from).toBe("t8");
    expect(firstUser.body).toBe("Inspect server routes");

    const doneGroup = items[1] as any;
    expect(doneGroup.showSpeakerHeader).toBe(true);
    expect(doneGroup.defaultExpanded).toBe(false);
    expect(summarizeActivityGroup(doneGroup)).toBe("2 tool calls · 1 thought · 1 plan");

    const reply = items[2] as any;
    expect(reply.showSpeakerHeader).toBe(false);
    expect(reply.blocks).toHaveLength(2);
    expect(reply.blocks[1]).toMatchObject({ type: "code", lang: "rust" });

    const stderr = items[3] as any;
    expect(stderr.lineCount).toBe(2);

    const activeGroup = items[5] as any;
    expect(activeGroup.runningCount).toBe(1);
    expect(activeGroup.defaultExpanded).toBe(true);
    expect(summarizeActivityGroup(activeGroup)).toBe("Running: Edit tests/router.rs");
  });

  test("falls back to plain text when entries array is empty", () => {
    const items = buildConversationItems([], "Legacy plain output");
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      kind: "assistant",
      text: "Legacy plain output",
      showSpeakerHeader: true,
    });
  });
});

describe("helpers", () => {
  test("maps tool kinds and shortens cwd paths", () => {
    expect(toolIconPath("execute", "bash")).toBe("mods/workspace-chrome/terminal.svg");
    expect(toolIconPath("read", "Read file")).toBe("mods/workspace-chrome/file-text.svg");
    expect(toolIconPath("search", "web search")).toBe("mods/workspace-chrome/search.svg");
    expect(shortenPath("/home/user/repo", "/home/user")).toBe("~/repo");
    expect(shortenPath("/tmp/work", "/home/user")).toBe("/tmp/work");
  });
});

describe("normalizeConfigOptions & filterConfigChoices", () => {
  test("orders capability pills cleanly and resolves current labels", () => {
    const opts = normalizeConfigOptions(
      [
        {
          id: "mode",
          name: "Mode",
          category: "mode",
          kind: "select",
          current_value: "code",
          options: [
            { value: "architect", name: "Architect" },
            { value: "code", name: "Code" },
          ],
        },
        {
          id: "fast-mode",
          name: "Fast Mode",
          category: "model_config",
          kind: "boolean",
          current_value: true,
        },
        {
          id: "reasoning_effort",
          name: "Reasoning Effort",
          category: "thought_level",
          kind: "select",
          current_value: "high",
          options: [
            { value: "low", name: "Low" },
            { value: "high", name: "High", group: "Deep" },
          ],
        },
        {
          id: "model",
          name: "Model",
          category: "model",
          kind: "select",
          current_value: "gpt-5.4",
          options: [
            { value: "gpt-5.4", name: "GPT-5.4", description: "Flagship", group: "OpenAI" },
            { value: "gpt-5.3-codex", name: "GPT-5.3 Codex", group: "OpenAI" },
          ],
        },
      ],
      {},
    );

    expect(opts.map((o) => `${o.id}:${o.iconKind}:${o.pillLabel}`)).toEqual([
      "model:model:GPT-5.4",
      "reasoning_effort:thinking:High",
      "fast-mode:fast:Fast",
      "mode:mode:Code",
    ]);
    expect(opts[2].isNonDefault).toBe(true);

    expect(filterConfigChoices(opts[0].choices, "codex").map((c) => c.value)).toEqual(["gpt-5.3-codex"]);
    expect(filterConfigChoices(opts[0].choices, "flagship").map((c) => c.value)).toEqual(["gpt-5.4"]);
  });

  test("falls back to synthetic custom model option when adapter omits model options", () => {
    const opts = normalizeConfigOptions([], { model: "claude-sonnet-4" });
    expect(opts).toHaveLength(1);
    expect(opts[0]).toMatchObject({
      id: "model",
      shortLabel: "Model",
      iconKind: "model",
      pillLabel: "claude-sonnet-4",
      kind: "custom_text",
      currentValue: "claude-sonnet-4",
      currentLabel: "claude-sonnet-4",
    });
  });

  test("maps Claude bypassPermissions mode to dangerously-skip-permissions and synthesizes fallback modes", () => {
    const opts = normalizeConfigOptions(
      [
        {
          id: "mode",
          name: "Mode",
          category: "mode",
          kind: "select",
          current_value: "bypassPermissions",
          options: [
            { value: "default", name: "Manual" },
            { value: "bypassPermissions", name: "Bypass permissions" },
          ],
        },
      ],
      { agent: "claude" },
    );
    const modeOpt = opts.find((o) => o.id === "mode")!;
    expect(modeOpt.pillLabel).toBe("dangerously-skip-permissions");
    expect(filterConfigChoices(modeOpt.choices, "dangerously").map((c) => c.value)).toEqual(["bypassPermissions"]);
    expect(filterConfigChoices(modeOpt.choices, "skip").map((c) => c.value)).toEqual(["bypassPermissions"]);

    const synthesized = normalizeConfigOptions([], { agent: "claude", model: "sonnet", mode: "default" });
    const synthMode = synthesized.find((o) => o.id === "mode")!;
    expect(synthMode.choices.some((c) => c.value === "bypassPermissions" && c.name === "dangerously-skip-permissions")).toBe(true);
  });

  test("summarizes multi-line Codex tool titles, splits glued thought headings, and drops session/load replays", () => {
    const info = summarizeToolTitle("python3 - <<'PY'\nprint('hello')\nprint('world')\nPY");
    expect(info.summary).toBe("python3 - <<'PY'");
    expect(info.extraLines).toBe(3);
    expect(info.multiline).toBe(true);

    expect(normalizeThoughtText("**Preparing report****Adding final link**")).toBe(
      "**Preparing report**\n\n**Adding final link**",
    );

    const items = buildConversationItems([
      { kind: "user", turn: 1, from: "t8", text: "First prompt", ts_ms: 1 },
      { kind: "assistant", turn: 1, text: "First answer", ts_ms: 2 },
      // Historical replay artifacts emitted at start of resumed turn 2 with from=null
      { kind: "user", turn: 2, from: null, text: "First prompt", ts_ms: 3 },
      { kind: "assistant", turn: 2, text: "First answer", ts_ms: 4 },
      // Authoritative prompt for turn 2
      { kind: "user", turn: 2, from: "t8", text: "Follow-up prompt", ts_ms: 5 },
      { kind: "thought", turn: 2, text: "**Checking dates****Summarizing findings**", ts_ms: 6 },
      { kind: "tool_call", turn: 2, id: "tc-ml", title: "cat > out.txt <<'EOF'\nline 1\nline 2\nEOF", tool_kind: "execute", status: "completed", ts_ms: 7 },
      { kind: "assistant", turn: 2, text: "Follow-up answer", ts_ms: 8 },
    ]);

    expect(items.filter((x) => x.kind === "user").map((x: any) => x.body)).toEqual([
      "First prompt",
      "Follow-up prompt",
    ]);
    const grp = items.find((x) => x.kind === "activity_group") as any;
    expect(grp.steps[0]).toMatchObject({ kind: "thought", text: "**Checking dates**\n\n**Summarizing findings**" });
    expect(grp.steps[1]).toMatchObject({
      kind: "tool_call",
      summaryTitle: "cat > out.txt <<'EOF'",
      extraLines: 3,
      multiline: true,
    });
    expect(summarizeActivityGroup(grp)).not.toContain("\n");

    const singleToolGroup = {
      runningCount: 0,
      steps: [{ kind: "tool_call", title: "cat <<'EOF'\na\nb\nEOF", summaryTitle: "cat <<'EOF'", extraLines: 3 }],
    };
    expect(summarizeActivityGroup(singleToolGroup)).toBe("cat <<'EOF' (+3 lines)");

    // Program metadata alone identifies Claude Code ACP and exposes dangerously-skip-permissions.
    const claudeByProgram = normalizeConfigOptions([], { program: { name: "Claude Code" } });
    const claudeMode = claudeByProgram.find((o) => o.id === "mode")!;
    expect(claudeMode.choices.some((c) => c.value === "bypassPermissions" && c.name === "dangerously-skip-permissions")).toBe(true);
    expect(claudeMode.currentValue).toBe("bypassPermissions");
    expect(claudeMode.pillLabel).toBe("dangerously-skip-permissions");

    // Adapters with permission_mode but no native ACP mode expose Herdr2's permissions selector.
    const codexPerms = normalizeConfigOptions([], { agent: "codex", permission_mode: "approve-reads" });
    const permOpt = codexPerms.find((o) => o.id === "permissions")!;
    expect(permOpt.currentLabel).toBe("Approve reads");
    expect(permOpt.choices.some((c) => c.value === "dangerously-skip-permissions")).toBe(true);

    // Full 5-choice Claude mode list preserves all choices and maps bypassPermissions -> dangerously-skip-permissions.
    const fullClaude = normalizeConfigOptions(
      [
        {
          id: "mode",
          name: "Mode",
          category: "mode",
          kind: "select",
          current_value: "default",
          options: [
            { value: "default", name: "Manual", description: "Always ask before making changes" },
            { value: "acceptEdits", name: "Accept edits", description: "Automatically accept all file edits" },
            { value: "plan", name: "Plan", description: "Create a plan before making changes" },
            { value: "auto", name: "Auto", description: "Claude handles permission decisions" },
            { value: "bypassPermissions", name: "Bypass permissions", description: "Accepts all permissions" },
          ],
        },
      ],
      { agent: "claude" },
    );
    const fullMode = fullClaude.find((o) => o.id === "mode")!;
    expect(fullMode.choices.map((c) => c.value)).toEqual(["default", "acceptEdits", "plan", "auto", "bypassPermissions"]);
    expect(fullMode.choices[4]).toMatchObject({
      value: "bypassPermissions",
      name: "dangerously-skip-permissions",
      description: "Accepts all permissions",
    });

    // Alternate mode option ids (`permission_mode`, `approval_policy`) also normalize under `mode`.
    for (const altId of ["permission_mode", "approval_policy"]) {
      const altOpts = normalizeConfigOptions(
        [
          {
            id: altId,
            name: "Permissions",
            kind: "select",
            current_value: "default",
            options: [{ value: "default", name: "Manual" }],
          },
        ],
        { agent: "claude" },
      );
      const altMode = altOpts.find((o) => o.id === altId)!;
      expect(altMode.iconKind).toBe("mode");
      expect(altMode.choices.some((c) => c.value === "bypassPermissions" && c.name === "dangerously-skip-permissions")).toBe(true);
    }
  });
});

describe("PTY structured formatting & snapshot fallback", () => {
  test("cleanTerminalSnapshot strips wide dividers, NBSP, and blank runs", () => {
    const rule = "─".repeat(200);
    const blanks = "\n".repeat(100);
    const raw = `\n ▐▛███▛█   Claude Code\n${blanks}${rule}\n❯\u00a0Try "fix lint errors"\n${rule}\n  Model:\u00a0GLM-5.3\n\n\n`;
    const cleaned = cleanTerminalSnapshot(raw);
    expect(cleaned).not.toContain(rule);
    expect(cleaned).not.toContain("\u00a0");
    expect(cleaned).not.toContain("\n\n\n");
    expect(cleaned.split("\n")).toEqual([
      " ▐▛███▛█   Claude Code",
      "",
      '❯ Try "fix lint errors"',
      "  Model: GLM-5.3",
    ]);
  });

  test("returns empty state for idle turn-0 PTY agent and snapshot card for non-zero screen fallback", () => {
    const rawScreen = `${"─".repeat(60)}\n❯\u00a0Welcome to Claude Code\n${"─".repeat(60)}`;
    expect(
      buildConversationItems([], rawScreen, false, {
        isPty: true,
        how: "screen",
        turn: 0,
        isAgent: true,
      }),
    ).toEqual([]);

    const snap = buildConversationItems([], rawScreen, false, {
      isPty: true,
      how: "screen",
      turn: 1,
      isAgent: true,
    });
    expect(snap).toHaveLength(1);
    expect(snap[0]).toMatchObject({
      kind: "terminal_snapshot",
      turn: 1,
      text: "❯ Welcome to Claude Code",
      lineCount: 1,
    });
  });

  test("keeps indented fenced code inside numbered lists while splitting top-level fences", () => {
    const md = [
      "1. First step:",
      "",
      "   ```sh",
      "   cargo check",
      "   ```",
      "",
      "2. Second step.",
      "",
      "```rust",
      "fn top_level() {}",
      "```",
    ].join("\n");

    const blocks = splitMarkdownBlocks(md);
    expect(blocks).toEqual([
      {
        type: "prose",
        text: "1. First step:\n\n   ```sh\n   cargo check\n   ```\n\n2. Second step.",
      },
      {
        type: "code",
        lang: "rust",
        code: "fn top_level() {}",
        closed: true,
      },
    ]);
  });

  test("coalesces adjacent assistant entries per turn and marks only final turn reply", () => {
    const items = buildConversationItems([
      { kind: "user", turn: 1, text: "Check status", ts_ms: 100 },
      { kind: "assistant", turn: 1, text: "Working on part A.", ts_ms: 110 },
      { kind: "assistant", turn: 1, text: "Also checking part B.", ts_ms: 120 },
      { kind: "tool_call", turn: 1, id: "t1", title: "h2 ls", tool_kind: "execute", status: "completed", ts_ms: 130 },
      { kind: "assistant", turn: 1, text: "All done.", ts_ms: 140 },
    ]);

    expect(items.map((x) => x.kind)).toEqual(["user", "assistant", "activity_group", "assistant"]);
    const firstReply = items[1] as any;
    expect(firstReply.text).toBe("Working on part A.\n\nAlso checking part B.");
    expect(firstReply.isFinalTurnReply).toBe(false);
    const finalReply = items[3] as any;
    expect(finalReply.text).toBe("All done.");
    expect(finalReply.isFinalTurnReply).toBe(true);
  });

  test("extracts thought preview, parsed plans, tool detail, PTY badges, and interactive shell CommandBlocks", () => {
    expect(thoughtPreview("**Inspecting routes**\n\nMore details")).toBe("Inspecting routes");
    expect(parsePlanItems(["[x] Done step", "[~] Active step", "[ ] Next step"])).toEqual([
      { status: "completed", text: "Done step" },
      { status: "in_progress", text: "Active step" },
      { status: "pending", text: "Next step" },
    ]);

    const items = buildConversationItems([
      { kind: "thought", turn: 1, text: "**Plan verification**\nChecking build output", ts_ms: 1 },
      {
        kind: "tool_call",
        turn: 1,
        id: "t1",
        title: "cargo check",
        tool_kind: "execute",
        status: "completed",
        detail: "Finished `dev` profile in 1.2s",
        ts_ms: 2,
      },
      { kind: "plan", turn: 1, items: ["[x] Compile", "[ ] Test"], ts_ms: 3 },
    ]);
    const grp = items[0] as any;
    expect(grp.steps[0].preview).toBe("Plan verification");
    expect(grp.steps[1]).toMatchObject({
      detail: "Finished `dev` profile in 1.2s",
      expandable: true,
    });
    expect(grp.latestPlan.parsedItems).toHaveLength(2);

    const shellSnap = buildConversationItems([], "zzh@wsl:~/herdr2$ cargo test\nok: 10 passed\n$ git status\nclean", false, {
      isPty: true,
      how: "screen",
      turn: 1,
      isAgent: false,
    });
    expect(shellSnap[0]).toMatchObject({
      kind: "terminal_snapshot",
      shellBlocks: [
        { type: "command", command: "cargo test", output: "ok: 10 passed" },
        { type: "command", command: "git status", output: "clean" },
      ],
    });

    expect(
      buildPtySessionBadges(
        { model: "gpt-5.4", reasoning_effort: "high", mode: "never" },
        {},
      ).map((b) => `${b.iconKind}:${b.label}`),
    ).toEqual(["model:gpt-5.4", "thinking:High", "mode:never"]);
  });

  test("normalizes Cursor context option pill and strips zero-width fast label", () => {
    const cursorOpts = normalizeConfigOptions(
      [
        {
          id: "model",
          name: "Model",
          category: "model",
          kind: "select",
          current_value: "grok-4.7",
          options: [{ value: "grok-4.7", name: "Grok 4.7" }],
        },
        {
          id: "context",
          name: "Context",
          category: "model_config",
          kind: "select",
          current_value: "500k",
          options: [
            { value: "256k", name: "256K" },
            { value: "500k", name: "500K" },
          ],
        },
        {
          id: "reasoning_effort",
          name: "Effort",
          category: "thought_level",
          kind: "select",
          current_value: "high",
          options: [{ value: "high", name: "High" }],
        },
        {
          id: "fast",
          name: "Fast",
          category: "model_config",
          kind: "select",
          current_value: "false",
          options: [
            { value: "false", name: "Off" },
            { value: "true", name: "Fast\u200b\u200b" },
          ],
        },
      ],
      { agent: "cursor" },
    );

    expect(cursorOpts.map((o) => `${o.id}:${o.pillLabel}`)).toEqual([
      "model:Grok 4.7",
      "context:Ctx 500K",
      "reasoning_effort:High",
      "fast:Fast · Off",
    ]);
    const fastOpt = cursorOpts.find((o) => o.id === "fast")!;
    expect(fastOpt.isNonDefault).toBe(false);
    expect(fastOpt.choices[1].name).toBe("Fast");
  });

  test("resolves T3 Code effort bars, diff/command tool details, changed files, user fold summary, and TUI frame cleaning", () => {
    expect(resolveEffortVisual("low")).toEqual({ bars: 4, active: 1 });
    expect(resolveEffortVisual("medium")).toEqual({ bars: 4, active: 2 });
    expect(resolveEffortVisual("high")).toEqual({ bars: 4, active: 3 });
    expect(resolveEffortVisual("max")).toEqual({ bars: 4, active: 4 });
    expect(resolveEffortVisual("disabled")).toEqual({ bars: 4, active: 0 });
    expect(resolveEffortVisual("enabled")).toEqual({ bars: 4, active: 3 });
    expect(resolveEffortVisual("ultra")).toEqual({ bars: 4, active: 4 });
    expect(resolveEffortVisual("unknown")).toBeNull();

    const zcodeOpts = normalizeConfigOptions(
      [
        {
          id: "thought",
          name: "Thought Level",
          category: "thought_level",
          kind: "select",
          current_value: "max",
          options: [
            { value: "low", name: "low" },
            { value: "high", name: "high" },
            { value: "max", name: "max" },
          ],
        },
      ],
      { agent: "zcode" },
    );
    const zcodeThought = zcodeOpts.find((o) => o.id === "thought")!;
    expect(zcodeThought.name).toBe("Reasoning Effort");
    expect(zcodeThought.iconKind).toBe("thinking");
    expect(zcodeThought.pillLabel).toBe("Max");
    expect(zcodeThought.choices.map((c) => c.name)).toEqual(["Low", "High", "Max"]);
    expect(zcodeThought.effortVisual).toEqual({ bars: 4, active: 4 });

    const badges = buildPtySessionBadges({ model: "gpt-5.4", reasoning_effort: "high" }, {});
    expect((badges.find((b) => b.id === "reasoning") as any)?.effortVisual).toEqual({ bars: 4, active: 3 });

    const diffDetail = parseToolDetail({
      tool_kind: "edit",
      title: "Edit /repo/src/app.rs",
      detail: "--- /repo/src/app.rs\n+++ /repo/src/app.rs\n@@ -1,2 +1,2 @@\n- let x = 1;\n+ let x = 2;\n+ let y = 3;",
    });
    expect(diffDetail.mode).toBe("diff");
    expect(diffDetail.additions).toBe(2);
    expect(diffDetail.deletions).toBe(1);

    const cmdDetail = parseToolDetail({
      tool_kind: "execute",
      title: "cargo test -p app",
      detail: "cargo test -p app\n\ntest result: ok. 5 passed",
    });
    expect(cmdDetail.mode).toBe("command");
    expect(cmdDetail.command).toBe("cargo test -p app");
    expect(cmdDetail.output).toBe("test result: ok. 5 passed");

    const items = buildConversationItems(
      [
        {
          kind: "user",
          turn: 1,
          from: "t1",
          text: '<h2 from="t1"># Review PR\n\nPlease inspect `src/app.rs` carefully.</h2>',
          ts_ms: 100,
        },
        {
          kind: "tool_call",
          turn: 1,
          id: "e1",
          title: "Edit /repo/src/app.rs",
          tool_kind: "edit",
          status: "completed",
          detail: "--- /repo/src/app.rs\n+++ /repo/src/app.rs\n- old\n+ new\n+ extra",
          ts_ms: 110,
        },
        {
          kind: "assistant",
          turn: 1,
          text: "Updated `src/app.rs`.",
          ts_ms: 120,
        },
      ],
      "",
      false,
      { cwd: "/repo" },
    );

    const userItem = items[0] as any;
    expect(userItem.isDelegated).toBe(true);
    expect(userItem.foldSummary).toBe("Review PR");

    const finalReply = items[2] as any;
    expect(finalReply.changedFiles).toEqual({
      files: [{ path: "src/app.rs", additions: 2, deletions: 1 }],
      totalAdditions: 2,
      totalDeletions: 1,
    });

    const tui = [
      "╭────────────────────────╮",
      "│ ❯ Write tests          │",
      "│                        │",
      "╰────────────────────────╯",
      "│ standalone bar │",
    ].join("\n");
    const cleaned = cleanTerminalSnapshot(tui);
    expect(cleaned).toBe("❯ Write tests\n│ standalone bar │");
  });

  test("attaches checkpoint stats and rewind dividers across ACP and PTY turns", () => {
    const items = buildConversationItems(
      [
        { kind: "user", turn: 1, text: "Add helper", ts_ms: 10 },
        { kind: "assistant", turn: 1, text: "Added helper.", ts_ms: 20 },
        {
          kind: "checkpoint",
          turn: 1,
          ref_name: "refs/herdr2/checkpoints/default/a1/1",
          commit_oid: "abc",
          files: [{ path: "src/helper.rs", added: 14, deleted: 2, binary: false }],
          ts_ms: 25,
        },
        { kind: "rewind", to_turn: 1, restore_files: true, ts_ms: 30 },
      ],
      "",
      false,
      { cwd: "/repo" },
    );

    expect(items.length).toBe(3);
    const reply = items[1] as any;
    expect(reply.changedFiles).toEqual({
      checkpointTurn: 1,
      fromCheckpoint: true,
      totalAdditions: 14,
      totalDeletions: 2,
      files: [{ path: "src/helper.rs", additions: 14, deletions: 2 }],
    });
    expect(items[2]).toMatchObject({ kind: "rewind", to_turn: 1, restore_files: true });

    const ptyItems = buildConversationItems(
      [
        { kind: "user", turn: 1, text: "Fix bug", ts_ms: 10 },
        { kind: "assistant", turn: 1, text: "Fixed.", ts_ms: 20 },
      ],
      "",
      false,
      {
        isPty: true,
        checkpointsByTurn: [{ turn: 1, files: [{ path: "main.ts", added: 5, deleted: 1 }] }],
      },
    );
    expect((ptyItems[1] as any).changedFiles.checkpointTurn).toBe(1);
    expect((ptyItems[1] as any).changedFiles.totalAdditions).toBe(5);

    // Restoring at Turn A keeps Turn A & earlier turns and displays the rewind divider after Turn A.
    const rewoundA = buildConversationItems([
      { kind: "user", turn: 1, text: "Prompt A", ts_ms: 10 },
      { kind: "assistant", turn: 1, text: "Answer A", ts_ms: 20 },
      { kind: "rewind", to_turn: 1, restore_files: true, ts_ms: 30 },
    ]);
    expect(rewoundA.map((it: any) => it.kind)).toEqual(["user", "assistant", "rewind"]);
    expect((rewoundA[0] as any).turn).toBe(1);
    expect((rewoundA[1] as any).turn).toBe(1);
    expect((rewoundA[2] as any).to_turn).toBe(1);
  });

  test("renders handoff transcript banner and strips <herdr2-handoff> envelope", () => {
    const items = buildConversationItems([
      {
        kind: "handoff",
        turn: 0,
        handoff_kind: "side-chat",
        from_address: "a1",
        from_provider: "claude",
        from_turns: "1-3",
        ts_ms: 5,
      },
      {
        kind: "user",
        turn: 1,
        text: '<herdr2-handoff kind="side-chat" from="a1">\n# Context Handoff\nDetails\n</herdr2-handoff>\n\nCheck race condition in auth.rs',
        ts_ms: 10,
      },
      { kind: "assistant", turn: 1, text: "Found mutex ordering issue.", ts_ms: 20 },
    ]);

    expect(items.map((it: any) => it.kind)).toEqual(["handoff", "user", "assistant"]);
    expect(items[0]).toMatchObject({
      kind: "handoff",
      handoff_kind: "side-chat",
      from_address: "a1",
      from_provider: "claude",
      from_turns: "1-3",
    });
    expect((items[1] as any).body).toBe("Check race condition in auth.rs");
    expect((items[1] as any).handoff).toEqual({ kind: "side-chat", from: "a1" });
  });
});

describe("normalizeInboxItems", () => {
  test("strips <h2 from=...> envelope, resolves friendly sender labels, and formats previews", () => {
    const items = normalizeInboxItems(
      [
        { id: 101, text: "Check logs\nthen retry", queued_ms: 1700000000000 },
        { id: 102, text: '<h2 from="a2">\nRun integration tests\n</h2>', from: "a2", queued_ms: 1700000060000 },
      ],
      [{ address: "wsl/a2", name: "Tester" }],
      "wsl/a1",
    );
    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({
      id: 101,
      index: 1,
      body: "Check logs\nthen retry",
      preview: "Check logs ↵ then retry",
      from: null,
      fromLabel: null,
    });
    expect(items[1]).toMatchObject({
      id: 102,
      index: 2,
      body: "Run integration tests",
      preview: "Run integration tests",
      from: "a2",
      fromLabel: "Tester (a2)",
    });
  });

  test("returns empty array when inbox is empty or missing", () => {
    expect(normalizeInboxItems(null)).toEqual([]);
    expect(normalizeInboxItems([])).toEqual([]);
  });
});

describe("resolvePeerAddress", () => {
  test("prefixes bare handoff source address with current remote machine", () => {
    expect(resolvePeerAddress("t22", "wsl/a4")).toBe("wsl/t22");
    expect(resolvePeerAddress("wsl/t22", "wsl/a4")).toBe("wsl/t22");
    expect(resolvePeerAddress("t22", "a4")).toBe("t22");
    expect(resolvePeerAddress("", "wsl/a4")).toBe("");
  });
});

describe("external changed file helpers", () => {
  test("resolveChangedFilePath resolves relative, external absolute, and tilde paths cleanly", () => {
    expect(isAbsoluteFilePath("/tmp/audit/a.py")).toBe(true);
    expect(isAbsoluteFilePath("src/main.rs")).toBe(false);
    expect(resolveChangedFilePath("src/agent.py", "/home/u/buildlink")).toBe("/home/u/buildlink/src/agent.py");
    expect(resolveChangedFilePath("/tmp/buildlink-audit-claude/harness.py", "/home/u/buildlink")).toBe(
      "/tmp/buildlink-audit-claude/harness.py",
    );
    expect(resolveChangedFilePath("~/notes/audit.md", "/home/u/buildlink", "/home/u", null)).toBe(
      "/home/u/notes/audit.md",
    );
    expect(resolveChangedFilePath("~/notes/audit.md", "/home/u/buildlink", "/home/u", "remote-box")).toBe(
      "~/notes/audit.md",
    );
    expect(resolveChangedFilePath("../other/foo.py", "/home/u/buildlink")).toBe("/home/u/other/foo.py");
  });

  test("extractTurnFilePatch collects multi-block proto.rs diffs and distinguishes create-only vs edited files", () => {
    const entries = [
      {
        kind: "tool_call",
        turn: 1,
        id: "c1",
        tool_kind: "edit",
        status: "completed",
        title: "Write /tmp/buildlink-audit-claude/x0_smoke.py",
        detail: "--- /tmp/buildlink-audit-claude/x0_smoke.py\n+++ /tmp/buildlink-audit-claude/x0_smoke.py\n+ print('hi')\n+ print('bye')",
      },
      {
        kind: "tool_call",
        turn: 1,
        id: "c2",
        tool_kind: "edit",
        status: "completed",
        title: "Edit /tmp/buildlink-audit-claude/harness.py",
        detail: [
          "/tmp/buildlink-audit-claude/harness.py",
          "",
          "--- /tmp/buildlink-audit-claude/harness.py",
          "+++ /tmp/buildlink-audit-claude/harness.py",
          "- old_a()",
          "+ new_a()",
          "",
          "--- /tmp/buildlink-audit-claude/harness.py",
          "+++ /tmp/buildlink-audit-claude/harness.py",
          "- old_b()",
          "+ new_b()",
        ].join("\n"),
      },
    ];

    const smoke = extractTurnFilePatch(entries, 1, "/tmp/buildlink-audit-claude/x0_smoke.py", "/home/u/buildlink");
    expect(smoke.isCreateOnly).toBe(true);
    expect(smoke.hasEdits).toBe(false);
    expect(smoke.patch).toContain("+ print('hi')");

    const harness = extractTurnFilePatch(entries, 1, "/tmp/buildlink-audit-claude/harness.py", "/home/u/buildlink");
    expect(harness.isCreateOnly).toBe(false);
    expect(harness.hasEdits).toBe(true);
    expect(harness.patch).toContain("- old_a()");
    expect(harness.patch).toContain("+ new_b()");
  });

  test("buildConversationItems preserves external edited files alongside git checkpoint files in the same turn", () => {
    const items = buildConversationItems(
      [
        { kind: "user", turn: 1, text: "Edit repo and tmp script", ts_ms: 10 },
        {
          kind: "tool_call",
          turn: 1,
          id: "e1",
          tool_kind: "edit",
          status: "completed",
          title: "Write /tmp/audit/repro.py",
          detail: "--- /tmp/audit/repro.py\n+++ /tmp/audit/repro.py\n+ x = 1\n+ y = 2",
          ts_ms: 15,
        },
        { kind: "assistant", turn: 1, text: "Done.", ts_ms: 20 },
        {
          kind: "checkpoint",
          turn: 1,
          files: [{ path: "src/lib.rs", added: 5, deleted: 1 }],
          ts_ms: 25,
        },
      ],
      "",
      false,
      { cwd: "/repo" },
    );
    const reply = items.find((i: any) => i.kind === "assistant") as any;
    expect(reply.changedFiles.files).toEqual([
      { path: "src/lib.rs", additions: 5, deletions: 1 },
      { path: "/tmp/audit/repro.py", additions: 2, deletions: 0 },
    ]);
    expect(reply.changedFiles.totalAdditions).toBe(7);
    expect(reply.changedFiles.totalDeletions).toBe(1);
  });
});
