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
} from "../defaults/mods/files/themes.js";

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
