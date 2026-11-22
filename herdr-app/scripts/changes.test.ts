import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";
import {
  ROW_HEIGHT,
  COMPOSER_HEIGHT,
  annotateDiffRows,
  buildSelectionRange,
  extractSnippet,
  buildDisplayItems,
  resolveTargetCandidates,
  makeDraftScopeKey,
  readScopeDrafts,
  writeScopeDrafts,
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
