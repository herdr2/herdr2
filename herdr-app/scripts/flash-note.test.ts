import { test, expect } from "bun:test";
import {
  normalizeNotes,
  upsertNote,
  markEntriesSent,
  moveNoteToTrash,
  restoreNoteFromTrash,
  noteEntryStats,
  noteDisplayTitle,
  formatEntriesForSend,
  catalogForPreset,
} from "../defaults/mods/flash-note/model.js";

test("Flash note model migrates legacy single notes into single-entry groups cleanly", () => {
  const legacy = [
    {
      id: "n1",
      title: "Legacy idea",
      text: "Fix retry jitter",
      createdAt: 1000,
      updatedAt: 2000,
      sentTo: "Claude (a1)",
      sentAt: 2100,
    },
  ];
  const normalized = normalizeNotes(legacy);
  expect(normalized).toHaveLength(1);
  expect(normalized[0].entries).toEqual([
    { id: "n1-e0", text: "Fix retry jitter", sentTo: "Claude (a1)", sentAt: 2100 },
  ]);
  expect(noteEntryStats(normalized[0])).toEqual({
    total: 1,
    sentCount: 1,
    unsentCount: 0,
    allSent: true,
    partialSent: false,
    isGroup: false,
  });
});

test("Flash note model saves, updates, tracks partial sends, and trashes/restores multi-entry groups", () => {
  const { list: savedList, note: group } = upsertNote([], {
    title: "Release checklist",
    entries: [
      { id: "e1", text: "Add mermaid preview" },
      { id: "e2", text: "   " },
      { id: "e3", text: "Add math formula rendering" },
      { id: "e4", text: "Support flash note groups" },
    ],
    now: 5000,
  });

  expect(group).not.toBeNull();
  expect(savedList).toHaveLength(1);
  expect(group!.entries.map((e: any) => e.text)).toEqual([
    "Add mermaid preview",
    "Add math formula rendering",
    "Support flash note groups",
  ]);
  expect(group!.sentTo).toBeNull();
  expect(noteEntryStats(group)).toEqual({
    total: 3,
    sentCount: 0,
    unsentCount: 3,
    allSent: false,
    partialSent: false,
    isGroup: true,
  });
  expect(noteDisplayTitle({ entries: group!.entries, text: group!.text })).toBe("Add mermaid preview");

  // Send only entry e1 and e4 (partial send)
  const partial = markEntriesSent(savedList, group!.id, ["e1", "e4"], "Claude Code (a1)", 6000);
  expect(partial.note).not.toBeNull();
  expect(partial.note!.sentTo).toBeNull(); // still has e3 unsent
  expect(partial.note!.entries.map((e: any) => [e.id, e.sentTo])).toEqual([
    ["e1", "Claude Code (a1)"],
    ["e3", null],
    ["e4", "Claude Code (a1)"],
  ]);
  expect(noteEntryStats(partial.note)).toEqual({
    total: 3,
    sentCount: 2,
    unsentCount: 1,
    allSent: false,
    partialSent: true,
    isGroup: true,
  });

  // Send remaining entry e3 -> whole group becomes sent
  const full = markEntriesSent(partial.list, group!.id, ["e3"], "Codex (a2)", 7000);
  expect(full.note!.sentTo).toBe("Codex (a2)");
  expect(noteEntryStats(full.note)).toEqual({
    total: 3,
    sentCount: 3,
    unsentCount: 0,
    allSent: true,
    partialSent: false,
    isGroup: true,
  });

  // Move to trash & restore preserves all 3 entries and their sent statuses
  const trashed = moveNoteToTrash(full.list, [], group!.id, 8000);
  expect(trashed.list).toHaveLength(0);
  expect(trashed.trash).toHaveLength(1);
  expect(trashed.trash[0].entries).toHaveLength(3);

  const restored = restoreNoteFromTrash(trashed.list, trashed.trash, group!.id, 9000);
  expect(restored.list).toHaveLength(1);
  expect(restored.list[0].entries.map((e: any) => e.id)).toEqual(["e1", "e3", "e4"]);
});

test("formatEntriesForSend keeps single entry plain and formats multi-entry groups cleanly", () => {
  expect(formatEntriesForSend([{ text: "Only one idea" }], "Ignored title")).toBe("Only one idea");
  expect(formatEntriesForSend([{ text: "First idea" }, { text: "Second idea" }], "")).toBe(
    "1. First idea\n\n2. Second idea",
  );
  expect(formatEntriesForSend([{ text: "First idea" }, { text: "Second idea" }], "Sprint Tasks")).toBe(
    "[Sprint Tasks]\n\n1. First idea\n\n2. Second idea",
  );
});

test("catalogForPreset exposes accurate per-model efforts for all ZCode GLM-5 models", () => {
  expect(catalogForPreset({}, "zcode", "GLM-5.4").efforts).toEqual(["low", "high", "max"]);
  expect(catalogForPreset({}, "zcode", "glm-5.4").efforts).toEqual(["low", "high", "max"]);
  expect(catalogForPreset({}, "zcode", "account:bigmodel-individual-coding-plan\\GLM-5.4").efforts).toEqual([
    "low",
    "high",
    "max",
  ]);
  expect(catalogForPreset({}, "zcode", "GLM-5.3").efforts).toEqual(["low", "high", "max"]);
  expect(catalogForPreset({}, "zcode", "GLM-5.3-Flash").efforts).toEqual(["low", "high", "max"]);
  expect(catalogForPreset({}, "zcode", "GLM-5.2").efforts).toEqual(["disabled", "high", "max"]);
  expect(catalogForPreset({}, "zcode", "GLM-5-Turbo").efforts).toEqual(["disabled", "enabled"]);
});
