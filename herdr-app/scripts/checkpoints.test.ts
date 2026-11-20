import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";
import {
  annotateDiffRows,
  buildSelectionRange,
  buildDisplayItems,
  resolveTargetCandidates,
} from "../defaults/mods/checkpoints/review_model.js";

const source = await readFile(new URL("../defaults/mods/checkpoints/main.js", import.meta.url), "utf8");
const helpersSource = source.slice(source.indexOf("const untab ="), source.indexOf("\nexport default class"));
const { diffRows, parseUnifiedPatchLines, normalizeVisibleCheckpoints } = new Function(
  `${helpersSource}; return { diffRows, parseUnifiedPatchLines, normalizeVisibleCheckpoints };`,
)();

test("parseUnifiedPatchLines extracts multi-file checkpoint hunks cleanly for diffRows", () => {
  const patch = [
    "diff --git a/src/a.rs b/src/a.rs",
    "index 111..222 100644",
    "--- a/src/a.rs",
    "+++ b/src/a.rs",
    "@@ -1,2 +1,2 @@",
    "-old",
    "+new",
    " keep",
  ].join("\n");
  const lines = parseUnifiedPatchLines(patch);
  expect(lines).toEqual(["File: src/a.rs", "@@ -1,2 +1,2 @@", "-old", "+new", " keep"]);
  const rows = diffRows(lines);
  expect(rows[0]).toEqual({ kind: "", text: "File: src/a.rs" });
  expect(rows[2]).toEqual({ kind: "-", number: 1, text: "old" });
  expect(rows[3]).toEqual({ kind: "+", number: 1, text: "new" });
});

test("normalizeVisibleCheckpoints sorts chronologically with initial T0 on top", () => {
  const raw = [
    { turn: 2, prompt: "Second", ts_ms: 200 },
    { turn: 0, ts_ms: 50 },
    { turn: 1, prompt: "First", ts_ms: 100 },
    { turn: 3, prompt: "Third", ts_ms: 300 },
  ];
  expect(normalizeVisibleCheckpoints(raw).map((c: { turn: number }) => c.turn)).toEqual([0, 1, 2, 3]);

  // Empty list returns [] unless includeInitialZero is true, which synthesizes T0 on top.
  expect(normalizeVisibleCheckpoints([], false)).toEqual([]);
  expect(normalizeVisibleCheckpoints([], true)).toEqual([{ turn: 0, synthetic: true, files: [] }]);
  expect(normalizeVisibleCheckpoints([{ turn: 1, prompt: "First", ts_ms: 100 }], true).map((c: { turn: number }) => c.turn)).toEqual([0, 1]);

  // Explicit manual bookmark at turn 0 stays visible at the top without duplicating T0.
  const withBookmark = [
    { turn: 2, prompt: "Second", ts_ms: 200 },
    { turn: 0, label: "manual bookmark", ts_ms: 50 },
    { turn: 1, prompt: "First", ts_ms: 100 },
  ];
  expect(normalizeVisibleCheckpoints(withBookmark, true).map((c: { turn: number }) => c.turn)).toEqual([0, 1, 2]);
});

test("Checkpoints mod is standalone without changes sub-tab toggles", () => {
  expect(source).toContain('id("checkpoints")');
  expect(source).not.toContain("changes-tab-working");
});

test("Checkpoints multi-file patch annotation scopes Shift+Click ranges within one file", () => {
  const patch = [
    "diff --git a/src/a.rs b/src/a.rs",
    "@@ -1,1 +1,1 @@",
    "+alpha",
    "diff --git a/src/b.rs b/src/b.rs",
    "@@ -5,1 +5,2 @@",
    "+beta1",
    "+beta2",
  ].join("\n");
  const rows = diffRows(parseUnifiedPatchLines(patch));
  const annotated = annotateDiffRows(rows, null);
  expect(annotated[0].isFileHeader).toBe(true);
  expect(annotated[2]).toMatchObject({ file: "src/a.rs", newLine: 1, commentable: true });
  expect(annotated[5]).toMatchObject({ file: "src/b.rs", newLine: 5, commentable: true });
  expect(annotated[6]).toMatchObject({ file: "src/b.rs", newLine: 6, commentable: true });

  // Shift+Click across file boundary falls back to the focused file's single line.
  const crossFile = buildSelectionRange(annotated, 2, 5);
  expect(crossFile).toMatchObject({ file: "src/b.rs", startLine: 5, endLine: 5 });

  // Same-file range works cleanly and anchors inline comment under src/b.rs L6.
  const sameFile = buildSelectionRange(annotated, 5, 6);
  expect(sameFile).toMatchObject({ file: "src/b.rs", startLine: 5, endLine: 6 });

  const display = buildDisplayItems(annotated, [{ id: "k1", path: "src/b.rs", side: "new", start_line: 5, end_line: 6, body: "Extract helper" }], null);
  expect(display.items.some((x: { type: string; outdated?: boolean }) => x.type === "comment" && !x.outdated)).toBe(true);

  const targets = resolveTargetCandidates({
    preferredAddr: "a7",
    terminalsList: [
      { address: "t1", state: "live", program: { agent: true, name: "Codex" } },
      { address: "a7", state: "live", program: { agent: true, name: "Claude" } },
    ],
  });
  expect(targets.defaultTarget).toBe("a7");
});
