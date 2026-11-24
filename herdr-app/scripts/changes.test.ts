import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";
import {
  ROW_HEIGHT,
  COMPOSER_HEIGHT,
  annotateDiffRows,
  buildSelectionRange,
  extractSnippet,
  buildDisplayItems,
  buildSplitDiffRows,
  buildSplitDisplayItems,
  resolveTargetCandidates,
  makeDraftScopeKey,
  readScopeDrafts,
  writeScopeDrafts,
  isAbsolutePath,
  parentDirectory,
  resolvePathAgainstRoot,
  parseToolPatchLines,
} from "../defaults/mods/changes/review_model.js";
import {
  parseGitStatusV2,
  parseGitLog,
  parseStashCount,
  formatPrimaryRef,
} from "../defaults/mods/changes/git_info.js";

const source = await readFile(new URL("../defaults/mods/changes/main.js", import.meta.url), "utf8");
const rowsSource = source.slice(source.indexOf("const untab ="), source.indexOf("\nexport default class"));
const { diffRows } = new Function(`${rowsSource}; return { diffRows };`)();

test("Changes keeps tab indentation, to the next 4-column stop", () => {
  const rows = diffRows(["@@ -1,2 +1,2 @@", "-\tif x {", "+\t\tif x {", " a\tb", "+    spaces"]);
  expect(rows.map((r: { text: string }) => r.text)).toEqual(["@@ -1,2 +1,2 @@", "    if x {", "        if x {", "a   b", "    spaces"]);
});

test("Changes mod focuses strictly on git working tree without nested checkpoint tabs", () => {
  expect(source).not.toContain("changes-tab-working");
  expect(source).not.toContain("changes-tab-checkpoints");
});

test("Changes uses resizable left-files / right-diff layout and wraps long lines in both unified and split modes", () => {
  expect(source).toContain('h_resizable("changes-split")');
  expect(source).toContain('id("changes-sidebar")');
  expect(source).toContain('id("changes-detail")');
  expect(source).toContain("this.gitLogOpen = false;");
  expect(source).toContain('id("chg-diff-mode-toggle")');
  expect(source).toContain('id("chg-split-header")');
  expect(source).toContain("list(\n              diffListId,");
  expect(source).not.toContain("v_virtual_list");
  expect(source).not.toContain("changes-diff-x");
  expect(source).toContain(".whitespace_normal().child(row.text)");
});

test("buildSplitDiffRows pairs consecutive deletions and additions into aligned before/after rows and preserves comments", () => {
  const rows = diffRows([
    "@@ -10,4 +10,5 @@ fn demo()",
    " ctx before",
    "-old line 1",
    "-old line 2",
    "+new line 1",
    "+new line 2",
    "+new line 3",
    " ctx after",
  ]);
  const annotated = annotateDiffRows(rows, "src/demo.rs");
  const split = buildSplitDiffRows(annotated);

  expect(split.map((r: any) => r.kind)).toEqual(["hunk", "pair", "pair", "pair", "pair", "pair"]);
  expect(split[1].left.text).toBe("ctx before");
  expect(split[1].right.text).toBe("ctx before");
  expect(split[1].left.oldLine).toBe(10);
  expect(split[1].right.newLine).toBe(10);

  expect(split[2].left.text).toBe("old line 1");
  expect(split[2].right.text).toBe("new line 1");
  expect(split[3].left.text).toBe("old line 2");
  expect(split[3].right.text).toBe("new line 2");
  expect(split[4].left).toBeNull();
  expect(split[4].right.text).toBe("new line 3");
  expect(split[5].left.text).toBe("ctx after");
  expect(split[5].right.text).toBe("ctx after");

  const commentNew = { id: "c-new", path: "src/demo.rs", side: "new", start_line: 13, body: "Check extra line" };
  const commentOld = { id: "c-old", path: "src/demo.rs", side: "old", start_line: 11, body: "Why removed?" };
  const { items } = buildSplitDisplayItems(annotated, [commentNew, commentOld], null);
  expect(items.filter((x: any) => x.type === "comment").map((x: any) => x.comment.id)).toEqual(["c-old", "c-new"]);
});

test("annotateDiffRows tracks old/new line numbers and selection ranges accurately", () => {
  const rows = diffRows([
    "@@ -10,3 +10,4 @@ fn demo()",
    " context line",
    "-removed line",
    "+added line 1",
    "+added line 2",
  ]);
  const annotated = annotateDiffRows(rows, "src/demo.rs");
  expect(annotated[0].commentable).toBe(false);
  expect(annotated[1]).toMatchObject({ file: "src/demo.rs", side: "context", oldLine: 10, newLine: 10, commentable: true });
  expect(annotated[2]).toMatchObject({ file: "src/demo.rs", side: "old", oldLine: 11, newLine: null, commentable: true });
  expect(annotated[3]).toMatchObject({ file: "src/demo.rs", side: "new", oldLine: null, newLine: 11, commentable: true });
  expect(annotated[4]).toMatchObject({ file: "src/demo.rs", side: "new", oldLine: null, newLine: 12, commentable: true });

  const range = buildSelectionRange(annotated, 3, 4);
  expect(range).toMatchObject({
    file: "src/demo.rs",
    startRowIdx: 3,
    endRowIdx: 4,
    side: "new",
    startLine: 11,
    endLine: 12,
  });

  const snippet = extractSnippet(annotated, 3, 4, 1);
  expect(snippet).toContain("-removed line");
  expect(snippet).toContain("+added line 1");
  expect(snippet).toContain("+added line 2");
});

test("buildDisplayItems keeps scalar ROW_HEIGHT fast path and expands variable heights for comments/composer", () => {
  const rows = diffRows(["@@ -1,2 +1,2 @@", "-old", "+new"]);
  const annotated = annotateDiffRows(rows, "a.rs");

  const fast = buildDisplayItems(annotated, [], null);
  expect(fast.sizes).toBe(ROW_HEIGHT);
  expect(fast.items.length).toBe(3);

  const comment = { id: "c1", path: "a.rs", side: "new", start_line: 1, body: "Please add error handling" };
  const composer = { file: "a.rs", startRowIdx: 2, endRowIdx: 2, side: "new", startLine: 1, endLine: 1, editingId: "c1" };
  const rich = buildDisplayItems(annotated, [comment], composer);
  expect(Array.isArray(rich.sizes)).toBe(true);
  expect(rich.items.map((x: { type: string }) => x.type)).toEqual(["code", "code", "code", "comment"]);
  expect((rich.items[3] as { editing?: boolean }).editing).toBe(true);
});

test("resolveTargetCandidates prioritizes same-machine focused & same-repo agents and draft store scopes cleanly", () => {
  const { candidates, defaultTarget } = resolveTargetCandidates({
    machine: "box",
    repoRoot: "/work/app",
    panesList: [
      { pane: "p1", terminal: "box/t1", cwd: "/work/other", focused: false, program: { agent: true, name: "Codex" } },
      { pane: "p2", terminal: "box/a2", cwd: "/work/app", focused: true, acp: true, program: { agent: true, name: "Claude" } },
    ],
    agentsList: [{ address: "box/a2", acp: true, status: "working", focused: true, program: { agent: true, name: "Claude" } }],
    terminalsList: [
      { address: "t1", state: "live", cwd: "/work/app", program: { agent: true, name: "Local" } },
      { address: "box/t1", state: "live", cwd: "/work/other", program: { agent: true, name: "Codex" } },
      { address: "box/a2", state: "live", cwd: "/work/app", program: { agent: true, name: "Claude" } },
    ],
  });
  expect(candidates.map((c) => c.address)).toEqual(["box/a2", "box/t1"]);
  expect(defaultTarget).toBe("box/a2");

  const k = makeDraftScopeKey("box", "/work/app/");
  const store = writeScopeDrafts({}, k, [{ id: "1", path: "a.rs", start_line: 2, body: "fix" }]);
  expect(readScopeDrafts(store, k).length).toBe(1);
});

test("parseGitStatusV2 parses branch, tracking, ahead/behind, and staged/unstaged/untracked/conflict counts", () => {
  const raw = [
    "# branch.oid 8962c5907e763c13a7f1a885e20dbc7a2bc2ffca",
    "# branch.head feat/git-panel",
    "# branch.upstream origin/feat/git-panel",
    "# branch.ab +2 -1",
    "1 M. N... 100644 100644 100644 h1 h2 src/staged file.js",
    "1 .M N... 100644 100644 100644 h1 h2 src/unstaged.js",
    "1 MM N... 100644 100644 100644 h1 h2 src/both.js",
    "u UU N... 100644 100644 100644 100644 h1 h2 h3 src/conflict.js",
    "? docs/new note.md",
    "",
  ].join("\0");
  const res = parseGitStatusV2(raw);
  expect(res.branch).toMatchObject({
    name: "feat/git-panel",
    detached: false,
    hasHead: true,
    headShort: "8962c59",
    upstream: "origin/feat/git-panel",
    upstreamGone: false,
    ahead: 2,
    behind: 1,
  });
  expect(res.status).toEqual({
    staged: 2,
    unstaged: 2,
    untracked: 1,
    conflicts: 1,
    total: 5,
    clean: false,
  });
  expect(res.files.map((f) => [f.path, f.status, f.staged, f.fresh])).toEqual([
    ["src/staged file.js", "M", true, false],
    ["src/unstaged.js", "M", false, false],
    ["src/both.js", "M", true, false],
    ["src/conflict.js", "U", false, false],
    ["docs/new note.md", "A", false, true],
  ]);
});

test("parseGitLog, parseStashCount, and formatPrimaryRef handle recent commit entries cleanly", () => {
  const raw = [
    "8962c5907e763c13a7f1a885e20dbc7a2bc2ffca\x1f8962c590\x1ffeat(pty): track cli\x1fAlice\x1f3 mins ago\x1fHEAD -> master, origin/master\x1e",
    "0206eaeede86ac76fd67e79065cf054f0af33300\x1f0206eaee\x1ffix(ui): layout polish\x1fBob\x1f2 hours ago\x1ftag: v0.4.1\x1e",
  ].join("\n");
  const log = parseGitLog(raw);
  expect(log.length).toBe(2);
  expect(log[0]).toMatchObject({
    shortHash: "8962c590",
    subject: "feat(pty): track cli",
    author: "Alice",
    relTime: "3 mins ago",
    refs: ["HEAD -> master", "origin/master"],
  });
  expect(formatPrimaryRef(log[0].refs)).toBe("master");
  expect(formatPrimaryRef(log[1].refs)).toBe("v0.4.1");
  expect(parseStashCount("stash@{0}: WIP on master\nstash@{1}: draft\n")).toBe(2);
  expect(parseStashCount("")).toBe(0);
});

test("resolvePathAgainstRoot and parentDirectory classify in-repo vs external files accurately", () => {
  expect(isAbsolutePath("/tmp/audit/a.py")).toBe(true);
  expect(isAbsolutePath("~/notes/a.md")).toBe(true);
  expect(isAbsolutePath("C:\\work\\a.py")).toBe(true);
  expect(isAbsolutePath("src/app.rs")).toBe(false);

  expect(parentDirectory("/tmp/buildlink-audit-claude/harness.py")).toBe("/tmp/buildlink-audit-claude");
  expect(parentDirectory("/solo.txt")).toBe("/");
  expect(parentDirectory("C:\\tmp\\a.py")).toBe("C:/tmp");

  expect(resolvePathAgainstRoot("/work/buildlink/src/agent.py", "/work/buildlink")).toEqual({
    inside: true,
    relPath: "src/agent.py",
    absPath: "/work/buildlink/src/agent.py",
  });
  expect(resolvePathAgainstRoot("/tmp/buildlink-audit-claude/harness.py", "/work/buildlink")).toEqual({
    inside: false,
    relPath: null,
    absPath: "/tmp/buildlink-audit-claude/harness.py",
  });
  expect(resolvePathAgainstRoot("/work/buildlink-extra/a.py", "/work/buildlink")).toEqual({
    inside: false,
    relPath: null,
    absPath: "/work/buildlink-extra/a.py",
  });
  expect(resolvePathAgainstRoot("src/agent.py", "/work/buildlink")).toEqual({
    inside: true,
    relPath: "src/agent.py",
    absPath: "/work/buildlink/src/agent.py",
  });
});

test("parseToolPatchLines normalizes hunkless ACP proto.rs diffs, unified diffs, and apply_patch blocks for unified & split views", () => {
  // Multi-hunk proto.rs ACP Edit diff (just like Claude ACP editing /tmp/buildlink-audit-claude/harness.py).
  const protoPatch = [
    "--- /tmp/buildlink-audit-claude/harness.py",
    "+++ /tmp/buildlink-audit-claude/harness.py",
    "-         self.base = self.root / \"server\"",
    "-         self.write_profile()",
    "+         self.base = self.root / \"server\"",
    "+         atomic_json(self.state)",
    "+         self.write_profile()",
    "",
    "--- /tmp/buildlink-audit-claude/harness.py",
    "+++ /tmp/buildlink-audit-claude/harness.py",
    "- old_port = 5037",
    "+ new_port = free_port()",
  ].join("\n");

  const lines = parseToolPatchLines(protoPatch);
  expect(lines[0]).toBe("@@ -1,2 +1,3 @@ Edit #1");
  expect(lines[1]).toBe("-        self.base = self.root / \"server\"");
  expect(lines[4]).toBe("+        atomic_json(self.state)");
  expect(lines[6]).toBe("@@ -1,1 +1,1 @@ Edit #2");
  expect(lines[7]).toBe("-old_port = 5037");
  expect(lines[8]).toBe("+new_port = free_port()");

  const rows = diffRows(lines);
  const annotated = annotateDiffRows(rows, "/tmp/buildlink-audit-claude/harness.py");
  expect(annotated[1]).toMatchObject({ file: "/tmp/buildlink-audit-claude/harness.py", side: "old", oldLine: 1, commentable: true });
  expect(annotated[3]).toMatchObject({ file: "/tmp/buildlink-audit-claude/harness.py", side: "new", newLine: 1, commentable: true });

  const split = buildSplitDiffRows(annotated);
  expect(split[0].kind).toBe("hunk");
  expect(split[1].kind).toBe("pair");
  expect(split[1].left.text).toBe("        self.base = self.root / \"server\"");
  expect(split[1].right.text).toBe("        self.base = self.root / \"server\"");

  // Standard unified diff with @@ headers preserves line numbers verbatim.
  const unified = [
    "diff --git a/x.py b/x.py",
    "--- a/x.py",
    "+++ b/x.py",
    "@@ -42,2 +42,3 @@ def run():",
    " keep()",
    "-bad()",
    "+good()",
    "+better()",
  ].join("\n");
  expect(parseToolPatchLines(unified)).toEqual([
    "@@ -42,2 +42,3 @@ def run():",
    " keep()",
    "-bad()",
    "+good()",
    "+better()",
  ]);

  expect(source).toContain('shared("changes.open_request")');
  expect(source).toContain('id("changes-external-section")');
  expect(source).toContain('id("chg-ext-banner")');
});
