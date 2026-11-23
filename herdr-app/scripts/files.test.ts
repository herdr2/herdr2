import { test, expect } from "bun:test";
import { readFile, mkdtemp, mkdir, writeFile, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  detectFileKind,
  CODE_THEMES,
  MD_THEMES,
  resolveCodeTheme,
  resolveMdTheme,
  prepareMarkdown,
  resolveRelativeFileLink,
  resolveRelativeAssetPath,
} from "../defaults/mods/files/themes.js";
import {
  parseMarkdownBlocks,
  serializeMarkdownBlocks,
  toggleMarkdownTask,
  extractMarkdownImageUrls,
  prepareBlockMarkdown,
  detectHeadingLevel,
  parseMarkdownTable,
  serializeMarkdownTable,
  parseCodeFence,
  serializeCodeFence,
  resolveFenceLanguage,
  parseMathBlock,
  serializeMathBlock,
} from "../defaults/mods/files/markdown_blocks.js";
import { renderMermaidSvg, detectMermaidKind } from "../defaults/mods/files/mermaid_svg.js";
import { renderMathSvg, findInlineMathSpans, rewriteInlineMathInMarkdown } from "../defaults/mods/files/math_svg.js";

const source = await readFile(new URL("../defaults/mods/files/main.js", import.meta.url), "utf8");
const directorySource = source.slice(source.indexOf("async function directory("), source.indexOf("\nexport default class"));
const loadDirectory = (exec: Function, platform: string) => new Function("exec", "window", `${directorySource}; return directory;`)(exec, () => ({ platform }));
const directory = loadDirectory(
  async (command: string, args: string[]) => {
    // Hidden like the app's spawns: a visible console would give PowerShell UTF-8 and hide code page bugs.
    const p = Bun.spawn([command, ...args], { stdout: "pipe", stderr: "pipe", windowsHide: true });
    const [code, stdout, stderr] = await Promise.all([p.exited, new Response(p.stdout).text(), new Response(p.stderr).text()]);
    return { code, stdout, stderr };
  },
  process.platform === "win32" ? "windows" : process.platform,
);

test("Files retries a remote Windows directory with PowerShell on that machine", async () => {
  const calls: { command: string; args: string[]; on: string }[] = [];
  const list = loadDirectory(async (command: string, args: string[], options: { on: string }) => {
    calls.push({ command, args, on: options.on });
    if (command === "sh") throw new Error("herdr.exec: cannot start sh: program not found");
    return { code: 0, stdout: '{"name":"中文.txt","dir":false}', stderr: "" };
  }, "linux");
  const path = "C:/O’Brien's $folder";
  expect(await list(path, "winvm")).toEqual([{ name: "中文.txt", dir: false }]);
  expect(calls.map(c => [c.command, c.on])).toEqual([["sh", "winvm"], ["powershell", "winvm"]]);
  expect(calls[0].args.at(-1)).toBe(path);
  expect(calls[1].args.at(-1)).toContain("'C:/O’’Brien''s $folder'");
  expect(calls[1].args.at(-1)).toContain("[Console]::OutputEncoding");
});

test("Files does not mask remote command failures with PowerShell", async () => {
  for (const failure of [new Error("connection lost"), new Error("cannot start sh: permission denied"), { code: 1, stdout: "", stderr: "folder denied" }]) {
    const calls: string[] = [];
    const list = loadDirectory(async (command: string) => {
      calls.push(command);
      if (failure instanceof Error) throw failure;
      return failure;
    }, "linux");
    await expect(list("/folder", "linuxvm")).rejects.toThrow(failure instanceof Error ? failure.message : failure.stderr);
    expect(calls).toEqual(["sh"]);
  }
});

test("Files keeps remote POSIX directory listings on sh", async () => {
  const calls: string[] = [];
  const list = loadDirectory(async (command: string) => {
    calls.push(command);
    return { code: 0, stdout: "d\0folder\0f\0file.txt\0", stderr: "" };
  }, "windows");
  expect(await list("/folder", "linuxvm")).toEqual([{ name: "folder", dir: true }, { name: "file.txt", dir: false }]);
  expect(calls).toEqual(["sh"]);
});

test("Files lists literal folder names, not shell expressions", async () => {
  const root = await mkdtemp(join(process.platform === "linux" ? "/var/tmp" : tmpdir(), "herdr-files-"));
  try {
    for (const name of ["Test Folder (ACME)", "O'Brien $folder; (ACME)", "O’Brien ‘quoted’ ‚low‛"]) {
      const folder = join(root, name);
      await mkdir(join(folder, "child"), { recursive: true });
      await writeFile(join(folder, "hello.txt"), "hello");
      const entries = await directory(folder, null);
      expect(entries.sort((a: { name: string }, b: { name: string }) => a.name.localeCompare(b.name))).toEqual([
        { name: "child", dir: true }, { name: "hello.txt", dir: false },
      ]);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("Files preserves Unicode names when listing and expanding folders", async () => {
  const root = await mkdtemp(join(tmpdir(), "herdr-files-unicode-"));
  try {
    const folder = "中文目录 O'Brien (测试)", file = "项目说明.txt", child = "子文件.txt";
    await mkdir(join(root, folder));
    await writeFile(join(root, file), "hello");
    await writeFile(join(root, folder, child), "hello");
    const entries = await directory(root, null);
    expect(entries).toHaveLength(2);
    expect(entries).toContainEqual({ name: folder, dir: true });
    expect(entries).toContainEqual({ name: file, dir: false });
    expect(await directory(join(root, folder), null)).toEqual([{ name: child, dir: false }]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("Files follows directory links one level at a time", async () => {
  const root = await mkdtemp(join(tmpdir(), "herdr-files-links-"));
  try {
    const target = join(root, "versions");
    await mkdir(target);
    for (let minor = 1; minor <= 9; minor++) {
      await mkdir(join(target, `0.${minor}.0`));
    }
    await writeFile(join(target, "0.9.0", "STORY.md"), "story preview");
    await mkdir(join(root, ".hidden"));
    await writeFile(join(root, "ordinary.txt"), "ordinary");
    await symlink("versions", join(root, ".kb"), "dir");
    if (process.platform === "win32") await symlink(target, join(root, "junction"), "junction");
    await symlink(join(root, "ordinary.txt"), join(root, "file-link"), "file");
    await symlink(join(root, "missing"), join(root, "dangling"), "dir");
    // A cycle must be listed without recursively walking it.
    await symlink(target, join(target, "loop"), "dir");

    const entries = await directory(root, null);
    expect(entries).toHaveLength(process.platform === "win32" ? 7 : 6);
    if (process.platform === "win32") expect(entries).toContainEqual({ name: "junction", dir: true });
    expect(entries).toContainEqual({ name: ".kb", dir: true });
    expect(entries).toContainEqual({ name: "versions", dir: true });
    expect(entries).toContainEqual({ name: ".hidden", dir: true });
    expect(entries).toContainEqual({ name: "ordinary.txt", dir: false });
    expect(entries).toContainEqual({ name: "file-link", dir: false });
    expect(entries).toContainEqual({ name: "dangling", dir: false });
    const versions = await directory(join(root, ".kb"), null);
    expect(versions).toHaveLength(10);
    for (let minor = 1; minor <= 9; minor++) {
      expect(versions).toContainEqual({ name: `0.${minor}.0`, dir: true });
    }
    expect(versions).toContainEqual({ name: "loop", dir: true });
    expect(await directory(join(root, ".kb", "0.9.0"), null)).toEqual([{ name: "STORY.md", dir: false }]);
    expect(await readFile(join(root, ".kb", "0.9.0", "STORY.md"), "utf8")).toBe("story preview");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("Files detects Markdown, code languages, and exact filenames", () => {
  expect(detectFileKind("README.md")).toMatchObject({ language: "md", label: "Markdown", isMarkdown: true });
  expect(detectFileKind("docs/guide.MDX")).toMatchObject({ language: "md", label: "MDX", isMarkdown: true });
  expect(detectFileKind("src/main.rs")).toMatchObject({ language: "rs", label: "Rust", isMarkdown: false });
  expect(detectFileKind("web/App.tsx")).toMatchObject({ language: "tsx", label: "TSX", isMarkdown: false });
  expect(detectFileKind("Cargo.toml")).toMatchObject({ language: "toml", label: "TOML", isMarkdown: false });
  expect(detectFileKind("docker/Dockerfile")).toMatchObject({ language: "dockerfile", label: "Dockerfile", isMarkdown: false });
  expect(detectFileKind("Makefile")).toMatchObject({ language: "makefile", label: "Makefile", isMarkdown: false });
  expect(detectFileKind(".env.local")).toMatchObject({ language: "ini", label: "Env", isMarkdown: false });
  expect(detectFileKind("notes")).toMatchObject({ language: "txt", label: "Plain Text", isMarkdown: false });
});

test("Files code and Markdown themes always resolve valid hex colors and never transparent", () => {
  const hexRe = /^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/;
  const appLight = {
    background: "#ffffff",
    surface: "#f8fafc",
    raised: "#f1f5f9",
    foreground: "#0f172a",
    muted: "#475569",
    dim: "#94a3b8",
    accent: "#2563eb",
    border: "#e2e8f0",
    selection: "#2563eb33",
    hover: "#e2e8f0",
    ansi1: "#dc2626",
    ansi2: "#16a34a",
    ansi3: "#ca8a04",
    ansi4: "#2563eb",
    ansi5: "#9333ea",
    ansi6: "#0891b2",
  };
  for (const preset of CODE_THEMES) {
    const resolved = resolveCodeTheme(preset.id, appLight);
    for (const val of [...Object.values(resolved.colors), ...Object.values(resolved.syntax)]) {
      expect(val).not.toBe("transparent");
      expect(hexRe.test(val)).toBe(true);
    }
  }
  for (const preset of MD_THEMES) {
    const resolved = resolveMdTheme(preset.id, appLight);
    expect(hexRe.test(resolved.background)).toBe(true);
    expect(typeof resolved.textView.dark).toBe("boolean");
    expect(resolved.id === "auto" ? resolved.textView.dark : undefined).toBe(preset.id === "auto" ? false : undefined);
    for (const [k, val] of Object.entries(resolved.textView)) {
      if (k === "dark") continue;
      if (k === "syntax") {
        for (const s of Object.values(val as Record<string, string>)) expect(hexRe.test(s)).toBe(true);
      } else {
        expect(val).not.toBe("transparent");
        expect(hexRe.test(val as string)).toBe(true);
      }
    }
  }
});

test("Files formats GitHub callout alerts outside fenced code blocks", () => {
  const input = [
    "> [!NOTE]",
    "> Helpful tip.",
    "",
    "```markdown",
    "> [!WARNING]",
    "> Stay literal inside code fences.",
    "```",
  ].join("\n");
  const out = prepareMarkdown(input);
  expect(out).toContain("> **ⓘ Note**");
  expect(out).toContain("```markdown\n> [!WARNING]");
});

test("Files resolves relative Markdown file links and rejects external or escaping links", () => {
  expect(resolveRelativeFileLink("docs/guide/intro.md", "./setup.md#install")).toBe("docs/guide/setup.md");
  expect(resolveRelativeFileLink("docs/guide/intro.md", "../README.md")).toBe("docs/README.md");
  expect(resolveRelativeFileLink("README.md", "src/main.rs?line=10")).toBe("src/main.rs");
  expect(resolveRelativeFileLink("README.md", "../escape.txt")).toBeNull();
  expect(resolveRelativeFileLink("docs/a.md", "https://example.com/x")).toBeNull();
  expect(resolveRelativeFileLink("docs/a.md", "#section")).toBeNull();
});

test("Files detects binary image extensions separately from SVG and Markdown", () => {
  for (const ext of ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff"]) {
    const kind = detectFileKind(`assets/photo.${ext}`);
    expect(kind.isImage).toBe(true);
    expect(kind.isMarkdown).toBe(false);
    expect(kind.label).toBe(ext.toUpperCase());
  }
  const svg = detectFileKind("icons/logo.svg");
  expect(svg.isImage).toBe(false);
  expect(svg.label).toBe("SVG");
  const md = detectFileKind("docs/README.md");
  expect(md.isImage).toBe(false);
  expect(md.isMarkdown).toBe(true);
});

test("Files resolves relative and root-relative asset paths safely", () => {
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "./img/pic.png")).toBe("docs/guide/img/pic.png");
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "../shared/my%20pic.png?raw=1#top")).toBe("docs/shared/my pic.png");
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "<./img/spaced pic.png>")).toBe("docs/guide/img/spaced pic.png");
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "..\\hero.webp")).toBe("docs/hero.webp");
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "/assets/banner.jpg")).toBe("assets/banner.jpg");
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "../../../escape.png")).toBeNull();
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "https://example.com/a.png")).toBeNull();
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "data:image/png;base64,AA==")).toBeNull();
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "file:///tmp/a.png")).toBeNull();
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "//cdn.example.com/a.png")).toBeNull();
  expect(resolveRelativeAssetPath("docs/guide/intro.md", "#anchor")).toBeNull();
});

test("Files Markdown block parser preserves lossless round-trips across edge cases", () => {
  const samples = [
    "",
    "\n  \n\n",
    "---\ntitle: Spec\n---\r\n\r\n# Heading\r\n\r\n---\r\n\r\nParagraph line 1\r\nParagraph line 2\r\n",
    "````md\n```js\nconst x = 1;\n```\n````\n\n~~~py\nprint('ok')\n~~~\n",
    "| Name | Status |\n| --- | --- |\n| Core | OK |\n\n> [!NOTE] Important note\n> Body line\n",
    "- [ ] First task\n  - nested note\n- [X] Done task\n1. [ ] Ordered task\n",
    "<details>\n<summary>More</summary>\n\nHidden text\n</details>\n\n![solo](./pic.png)\n\n<img src=\"./tag.png\" height=\"40\">\n\n[ref]: ./ref.png \"title\"\n",
  ];

  for (const raw of samples) {
    const doc = parseMarkdownBlocks(raw);
    expect(serializeMarkdownBlocks(doc)).toBe(raw);
  }

  const frontDoc = parseMarkdownBlocks(samples[2]);
  expect(frontDoc.blocks[0].kind).toBe("frontmatter");
  expect(frontDoc.blocks[1].kind).toBe("heading");
  expect(frontDoc.blocks[2].kind).toBe("hr");

  const listDoc = parseMarkdownBlocks(samples[5]);
  expect(listDoc.blocks[0].kind).toBe("list");
  expect(listDoc.blocks[0].tasks).toEqual([
    { lineIndex: 0, checked: false, label: "First task" },
    { lineIndex: 2, checked: true, label: "Done task" },
    { lineIndex: 3, checked: false, label: "Ordered task" },
  ]);

  const mediaDoc = parseMarkdownBlocks(samples[6]);
  expect(mediaDoc.blocks.map((b) => b.kind)).toEqual(["html", "image", "image", "definition"]);
});

test("Files Markdown task toggling updates only the targeted task line and preserves endings", () => {
  const block = "- [ ] Alpha\r\n  - [X] Beta\r\n1. [ ] Gamma";
  const step1 = toggleMarkdownTask(block, 0);
  expect(step1).toBe("- [x] Alpha\r\n  - [X] Beta\r\n1. [ ] Gamma");
  const step2 = toggleMarkdownTask(step1, 1);
  expect(step2).toBe("- [x] Alpha\r\n  - [ ] Beta\r\n1. [ ] Gamma");
  const step3 = toggleMarkdownTask(step2, 2);
  expect(step3).toBe("- [x] Alpha\r\n  - [ ] Beta\r\n1. [x] Gamma");
  expect(toggleMarkdownTask(step3, 99)).toBe(step3);
});

test("Files extracts and rewrites Markdown and HTML image URLs while skipping code", () => {
  const docRaw = [
    "# Demo",
    "",
    "Inline ![a](./img/one.png \"One\") and ![a-dup](./img/one.png) and ![b](<./img/two space.jpg>) plus ![ref-pic][picref].",
    "",
    "HTML <img src='/assets/logo.webp' width='20'> and `<img src='./ignored-inline.png'>` and `![ignored](./inline.png)`.",
    "",
    "```md",
    "![ignored-fence](./fenced.png)",
    "<img src=\"./fenced-html.png\">",
    "```",
    "",
    "> [!WARNING] Watch out",
    "",
    "[picref]: ../shared/ref.png",
  ].join("\n");

  expect(extractMarkdownImageUrls(docRaw)).toEqual([
    "./img/one.png",
    "./img/two space.jpg",
    "/assets/logo.webp",
    "../shared/ref.png",
  ]);

  const parsed = parseMarkdownBlocks(docRaw);
  const remoteMap = new Map([
    ["docs/img/one.png", "data:image/png;base64,ONE"],
    ["docs/img/two space.jpg", "data:image/jpeg;base64,TWO"],
    ["assets/logo.webp", "data:image/webp;base64,LOGO"],
    ["shared/ref.png", "data:image/png;base64,REF"],
  ]);

  const inlineBlockOut = prepareBlockMarkdown(parsed.blocks[1].raw, parsed.defs, remoteMap, "docs/readme.md");
  expect(inlineBlockOut).toContain("![a](data:image/png;base64,ONE \"One\")");
  expect(inlineBlockOut).toContain("![b](data:image/jpeg;base64,TWO)");
  expect(inlineBlockOut).toContain("[picref]: data:image/png;base64,REF");

  const htmlBlockOut = prepareBlockMarkdown(parsed.blocks[2].raw, parsed.defs, remoteMap, "docs/readme.md");
  expect(htmlBlockOut).toContain("<img src='data:image/webp;base64,LOGO' width='20'>");
  expect(htmlBlockOut).toContain("`<img src='./ignored-inline.png'>`");
  expect(htmlBlockOut).toContain("`![ignored](./inline.png)`");

  const fenceOut = prepareBlockMarkdown(parsed.blocks[3].raw, parsed.defs, remoteMap, "docs/readme.md");
  expect(fenceOut.split("\n\n")[0]).toBe(parsed.blocks[3].raw);
  expect(fenceOut).toContain("![ignored-fence](./fenced.png)");
  expect(fenceOut).toContain('<img src="./fenced-html.png">');

  const alertOut = prepareBlockMarkdown(parsed.blocks[4].raw, parsed.defs, remoteMap, "docs/readme.md");
  expect(alertOut).toBe("> **⚠ Warning** Watch out");
});

test("Files detects ATX and Setext heading levels cleanly", () => {
  expect(detectHeadingLevel("# Main Title")).toBe(1);
  expect(detectHeadingLevel("  ## Section")).toBe(2);
  expect(detectHeadingLevel("###### Deepest")).toBe(6);
  expect(detectHeadingLevel("Setext H1\n===")).toBe(1);
  expect(detectHeadingLevel("Setext H2\n---")).toBe(2);
  expect(detectHeadingLevel("Plain paragraph")).toBe(0);
  const doc = parseMarkdownBlocks("# H1\n\n### H3\n\nBody\n");
  expect(doc.blocks.map((b) => b.headingLevel)).toEqual([1, 3, 0]);
});

test("Files parses and serializes GFM pipe tables including alignments and escaped pipes", () => {
  const raw = [
    "| Name | Role | Notes |",
    "| :--- | :---: | ---: |",
    "| Alice | Dev | a \\| b |",
    "| Bob | QA | ok |",
  ].join("\r\n");

  const table = parseMarkdownTable(raw);
  expect(table).not.toBeNull();
  expect(table?.headers).toEqual(["Name", "Role", "Notes"]);
  expect(table?.alignments).toEqual(["left", "center", "right"]);
  expect(table?.rows).toEqual([
    ["Alice", "Dev", "a | b"],
    ["Bob", "QA", "ok"],
  ]);

  if (!table) throw new Error("expected parsed table");
  table.rows[0][1] = "Lead | Architect";
  table.rows.push(["Cara", "PM", "ready"]);
  const serialized = serializeMarkdownTable(table);
  expect(serialized).toContain("\r\n");
  expect(serialized).toContain("Lead \\| Architect");
  const roundTripped = parseMarkdownTable(serialized);
  expect(roundTripped?.alignments).toEqual(["left", "center", "right"]);
  expect(roundTripped?.rows[0][1]).toBe("Lead | Architect");
  expect(roundTripped?.rows[2]).toEqual(["Cara", "PM", "ready"]);
  expect(parseMarkdownTable("| not a table |\nplain")).toBeNull();
});

test("Files parses and serializes fenced code blocks safely", () => {
  const raw = "```rust\nfn main() {\n    println!(\"hi\");\n}\n```";
  const fence = parseCodeFence(raw);
  expect(fence).toMatchObject({ fenceChar: "`", fenceLen: 3, lang: "rust", body: "fn main() {\n    println!(\"hi\");\n}", hasClosing: true });

  expect(resolveFenceLanguage("rust")).toBe("rs");
  expect(resolveFenceLanguage("TypeScript")).toBe("ts");
  expect(resolveFenceLanguage("unknown-lang")).toBe("txt");

  if (!fence) throw new Error("expected parsed fence");
  const withTicks = serializeCodeFence(fence, "````\ninner\n````", "markdown");
  expect(withTicks.startsWith("`````markdown\n")).toBe(true);
  expect(withTicks.endsWith("\n`````")).toBe(true);
  expect(parseCodeFence(withTicks)?.body).toBe("````\ninner\n````");

  const emptyFence = serializeCodeFence(fence, "", "py");
  expect(emptyFence).toBe("```py\n```");
});

import {
  resolveRepoRelativePath,
  selectionToLines,
  linesToSelectionRange,
  extractFileSnippet,
  computeMarkdownBlockRanges,
  mapCommentsToMarkdownBlocks,
  buildFileRows,
  buildFileSelectionRange,
  matchFileCommentRowIndex,
  buildFileDisplayItems,
  ROW_HEIGHT,
} from "../defaults/mods/files/review_model.js";

test("Files review_model maps editor UTF-16 selections and Markdown blocks to exact 1-based lines", () => {
  const src = "line 1\nline 2\nline 3\nline 4\nline 5";
  // Cursor on line 2
  expect(selectionToLines(src, { start: 8, end: 8 })).toEqual({ startLine: 2, endLine: 2 });
  // Full-line selection ending right after newline on line 2 stays on line 2
  expect(selectionToLines(src, { start: 7, end: 14 })).toEqual({ startLine: 2, endLine: 2 });
  // Multi-line selection across lines 2..4
  expect(selectionToLines(src, { start: 8, end: 23 })).toEqual({ startLine: 2, endLine: 4 });

  // Inverse mapping round-trips cleanly
  const back = linesToSelectionRange(src, 2, 4);
  expect(src.slice(back.start, back.end)).toBe("line 2\nline 3\nline 4");

  const snippet = extractFileSnippet(src, 2, 3, 1);
  expect(snippet).toContain("  1 | line 1");
  expect(snippet).toContain("> 2 | line 2");
  expect(snippet).toContain("> 3 | line 3");
  expect(snippet).toContain("  4 | line 4");

  // Repo-relative path matches Changes mod when browsing a subdirectory
  expect(resolveRepoRelativePath("/repo/apps/web", "/repo", "src/App.tsx")).toBe("apps/web/src/App.tsx");
  expect(resolveRepoRelativePath("/repo", "/repo", "README.md")).toBe("README.md");

  // Markdown block ranges & comment mapping
  const md = "# Title\n\nFirst para line 1\nFirst para line 2\n\n```ts\nconst x = 1;\n```";
  const doc = parseMarkdownBlocks(md);
  const ranges = computeMarkdownBlockRanges(doc);
  expect(ranges).toEqual([
    { index: 0, startLine: 1, endLine: 1 },
    { index: 1, startLine: 3, endLine: 4 },
    { index: 2, startLine: 6, endLine: 8 },
  ]);
  const mapped = mapCommentsToMarkdownBlocks(ranges, [
    { id: "c1", path: "README.md", start_line: 3, end_line: 4, body: "Clarify wording" },
    { id: "c2", path: "README.md", start_line: 7, body: "Use let" },
  ]);
  expect(mapped.byBlock.get(1)?.map((c) => c.id)).toEqual(["c1"]);
  expect(mapped.byBlock.get(2)?.map((c) => c.id)).toEqual(["c2"]);

  // Line-level file review rows, Shift+Click range, and inline comment interleaving (consistent with Checkpoints)
  const fileRows = buildFileRows("fn main() {\n\tlet x = 1;\n\tprintln!(\"{x}\");\n}", "src/main.rs");
  expect(fileRows).toHaveLength(4);
  expect(fileRows[0]).toMatchObject({ rowIndex: 0, lineNumber: 1, file: "src/main.rs", text: "fn main() {", commentable: true });
  expect(fileRows[1].text).toBe("    let x = 1;");

  const single = buildFileSelectionRange(fileRows, 1, 1);
  expect(single).toEqual({ path: "src/main.rs", startRowIdx: 1, endRowIdx: 1, startLine: 2, endLine: 2 });
  const multi = buildFileSelectionRange(fileRows, 3, 1);
  expect(multi).toEqual({ path: "src/main.rs", startRowIdx: 1, endRowIdx: 3, startLine: 2, endLine: 4 });

  // Fast scalar sizes when no comments exist
  const plain = buildFileDisplayItems(fileRows, [], null);
  expect(plain.items).toHaveLength(4);
  expect(plain.sizes).toBe(ROW_HEIGHT);

  // Interleaved inline comment immediately after its end_line + orphan comment at end
  const cInline = { id: "fc1", path: "src/main.rs", side: "file", start_line: 2, end_line: 3, body: "Avoid magic number" };
  const cOrphan = { id: "fc2", path: "src/main.rs", side: "file", start_line: 99, body: "Outdated line" };
  expect(matchFileCommentRowIndex(fileRows, cInline)).toBe(2);
  expect(matchFileCommentRowIndex(fileRows, cOrphan)).toBe(-1);
  const interleaved = buildFileDisplayItems(fileRows, [cInline, cOrphan], { editingId: "fc1" });
  expect(interleaved.items.map((it: any) => it.key)).toEqual(["r-0", "r-1", "r-2", "c-fc1", "r-3", "c-orphan-fc2"]);
  expect((interleaved.items[3] as any).editing).toBe(true);
  expect((interleaved.items[5] as any).outdated).toBe(true);
  expect(Array.isArray(interleaved.sizes)).toBe(true);
});

test("Files Markdown block parser parses display math blocks losslessly", () => {
  const samples = [
    "# Formulas\n\n$$E = mc^2$$\n\n$$\n\\int_{-\\infty}^{\\infty} e^{-x^2}\\,dx = \\sqrt{\\pi}\n$$\n\n\\[\n\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}\n\\]\n\nAfter paragraph.\n",
    "Inline $a^2 + b^2 = c^2$ and currency $5 and $10 stay paragraph.\n$$x + y$$\n",
  ];
  for (const raw of samples) {
    const doc = parseMarkdownBlocks(raw);
    expect(serializeMarkdownBlocks(doc)).toBe(raw);
  }
  const doc = parseMarkdownBlocks(samples[0]);
  expect(doc.blocks.map((b: any) => b.kind)).toEqual(["heading", "math", "math", "math", "paragraph"]);
  expect(parseMathBlock(doc.blocks[1].raw)).toMatchObject({ delim: "$$", singleLine: true, body: "E = mc^2" });
  expect(parseMathBlock(doc.blocks[2].raw)?.body).toContain("\\int_{-\\infty}^{\\infty}");
  expect(parseMathBlock(doc.blocks[3].raw)).toMatchObject({ delim: "\\[", singleLine: false });

  const parsed = parseMathBlock("$$\na + b\n$$");
  expect(serializeMathBlock(parsed, "c + d")).toBe("$$\nc + d\n$$");
  const single = parseMathBlock("$$x^2$$");
  expect(serializeMathBlock(single, "y^3")).toBe("$$y^3$$");
});

test("Files math SVG renderer handles fractions, roots, scripts, matrices, accents, and inline disambiguation", () => {
  const formulas = [
    "E = mc^2",
    "\\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}",
    "\\sum_{i=1}^{n} \\frac{1}{i^2} = \\frac{\\pi^2}{6}",
    "\\int_{0}^{\\infty} e^{-x^2} \\, dx = \\frac{\\sqrt{\\pi}}{2}",
    "\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix} \\begin{bmatrix} x \\\\ y \\end{bmatrix}",
    "f(x) = \\begin{cases} x^2 & x \\ge 0 \\\\ -x & x < 0 \\end{cases}",
    "\\left( \\frac{a}{b} \\right) + \\vec{v} + \\widehat{AB} + \\overline{z} + \\boxed{\\mathbb{R}^n}",
    "\\sqrt[3]{x + y} + \\binom{n}{k} + \\lim_{x \\to 0} \\frac{\\sin x}{x} = 1",
  ];

  for (const expr of formulas) {
    const disp = renderMathSvg(expr, { display: true, color: "#e6edf3" });
    expect(disp.ok).toBe(true);
    expect(disp.width).toBeGreaterThan(10);
    expect(disp.height).toBeGreaterThan(15);
    expect(disp.svg).toContain("<svg xmlns=\"http://www.w3.org/2000/svg\"");
    expect(disp.svg).not.toContain("<foreignObject");
    expect(disp.dataUrl.startsWith("data:image/svg+xml,")).toBe(true);

    const inline = renderMathSvg(expr, { display: false, color: "#1f2328" });
    expect(inline.ok).toBe(true);
    expect(inline.width).toBeGreaterThan(8);
    expect(inline.height).toBeGreaterThan(10);
  }

  // Inline math scanner ignores currency, escaped dollars, inline code, and fenced code
  const prose = "Costs $5 and $10, escaped \\$x\\$, code `$a+b$`, real $x^2 + y^2$ and \\(\\alpha + \\beta\\).";
  const spans = findInlineMathSpans(prose);
  expect(spans.map((s: any) => s.expr)).toEqual(["x^2 + y^2", "\\alpha + \\beta"]);

  const md = [
    "Equation $E=mc^2$ and `$raw$` and $5 to $10.",
    "```tex",
    "$ignored_in_fence$",
    "```",
  ].join("\n");
  const rewritten = rewriteInlineMathInMarkdown(md, { color: "#1f2328", fontSize: 13.5 });
  expect(rewritten).toContain('<img src="data:image/svg+xml,');
  expect(rewritten).toContain("`$raw$`");
  expect(rewritten).toContain("$5 to $10");
  expect(rewritten).toContain("$ignored_in_fence$");
});

test("Files Mermaid SVG renderer supports all major diagram types across light and dark themes", () => {
  const lightTheme = { dark: false, background: "#ffffff", surface: "#f6f8fa", foreground: "#1f2328", muted: "#656d76", border: "#d0d7de", accent: "#0969da" };
  const darkTheme = { dark: true, background: "#0d1117", surface: "#161b22", foreground: "#e6edf3", muted: "#8b949e", border: "#30363d", accent: "#58a6ff" };

  const diagrams: [string, string][] = [
    [
      "flowchart",
      `flowchart TD
        subgraph Core["核心流程"]
          A([开始]) --> B{条件判断?}
          B -->|是| C[处理数据]
          B -.->|否| D[(数据库)]
        end
        C ==> E((结束))
        D --> A`,
    ],
    [
      "sequence",
      `sequenceDiagram
        autonumber
        actor U as User
        participant A as API
        U->>+A: POST /login
        alt Valid
          A-->>U: 200 OK
        else Invalid
          A--xU: 401
        end
        Note right of A: Audited
        A->>A: Log metric`,
    ],
    [
      "state",
      `stateDiagram-v2
        [*] --> Idle
        Idle --> Running : start
        state Running {
          [*] --> Working
          Working --> Paused : pause
        }
        Running --> [*]`,
    ],
    [
      "class",
      `classDiagram
        class Animal {
          +String name
          +speak() void
        }
        class Dog {
          +bark() void
        }
        Animal <|-- Dog : inherits`,
    ],
    [
      "er",
      `erDiagram
        USER ||--o{ ORDER : places
        USER {
          int id PK
          string email
        }
        ORDER {
          int id PK
          int user_id FK
        }`,
    ],
    [
      "pie",
      `pie showData
        title Language Share
        "Rust" : 45
        "TypeScript" : 35
        "Other" : 20`,
    ],
    [
      "gantt",
      `gantt
        title Release Plan
        dateFormat YYYY-MM-DD
        section Build
        Design :done, d1, 2026-01-01, 5d
        Implement :active, i1, after d1, 7d
        Ship :milestone, after i1, 0d`,
    ],
    [
      "mindmap",
      `mindmap
        root((Herdr))
          Files
            Mermaid
            Math
          Terminals`,
    ],
    [
      "gitgraph",
      `gitGraph
        commit id: "init"
        branch feat
        checkout feat
        commit id: "add-svg" tag: "v0.1"
        checkout main
        merge feat`,
    ],
    [
      "timeline",
      `timeline
        title Milestones
        Q1 : Parser : Layout
        Q2 : SVG : Math`,
    ],
    [
      "xychart",
      `xychart-beta
        title "Latency"
        x-axis [Jan, Feb, Mar]
        y-axis "ms" 0 --> 100
        bar [30, 55, 40]
        line [25, 50, 35]`,
    ],
  ];

  for (const [expectedKind, src] of diagrams) {
    expect(detectMermaidKind(src)).toBe(expectedKind);
    for (const th of [lightTheme, darkTheme]) {
      const res = renderMermaidSvg(src, { theme: th, scale: 1 });
      expect(res.ok).toBe(true);
      expect(res.kind).toBe(expectedKind);
      expect(res.width).toBeGreaterThan(60);
      expect(res.height).toBeGreaterThan(40);
      expect(res.svg).toContain("<svg xmlns=\"http://www.w3.org/2000/svg\"");
      expect(res.svg).not.toContain("<foreignObject");
      expect(res.svg).not.toContain("<image");
      expect(res.dataUrl.startsWith("data:image/svg+xml,")).toBe(true);
    }
  }

  // Zoom scaling adjusts width & height while keeping viewBox intact
  const zoomed = renderMermaidSvg(diagrams[0][1], { theme: darkTheme, scale: 1.5 });
  const base = renderMermaidSvg(diagrams[0][1], { theme: darkTheme, scale: 1 });
  expect(zoomed.width).toBe(Math.round(base.width * 1.5));
  expect(zoomed.height).toBe(Math.round(base.height * 1.5));

  // Unsupported or empty diagrams fail gracefully
  expect(renderMermaidSvg("", { theme: darkTheme }).ok).toBe(false);
  expect(renderMermaidSvg("unknownDiagram\n  A --> B", { theme: darkTheme }).ok).toBe(false);
});
