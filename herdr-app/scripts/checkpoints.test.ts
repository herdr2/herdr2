import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";

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
