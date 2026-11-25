import { test, expect } from "bun:test";
import { readFile } from "node:fs/promises";
import { mkdtempSync, mkdirSync, realpathSync, rmSync, existsSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const source = (await readFile(new URL("../defaults/mods/worktree/main.js", import.meta.url), "utf8"))
  .replace(/^import .*;\n/gm, "").replace("export default class Worktree", "class Worktree");

// Execute conditional builders and retain text and handlers, without native GPUI layout.
function element() {
  const node = { id: "", children: [] as any[], onClick: null as ((e: any, cx: any) => void) | null };
  const el: any = new Proxy(node, { get: (_, method) => {
    if (method === "node") return node;
    return (...args: any[]) => {
      if (method === "id") node.id = args[0];
      if (method === "on_click") node.onClick = args[0];
      if (method === "child") node.children.push(args[0]?.node ?? args[0]);
      if (method === "children") node.children.push(...args[0].map((child: any) => child?.node ?? child));
      if (method === "when" && args[0]) args[1](el);
      return el;
    };
  } });
  return el;
}
function textOf(node: any): string {
  return typeof node === "string" ? node : (node?.children ?? []).map(textOf).join(" ");
}
function findById(node: any, id: string): any {
  if (!node || typeof node !== "object") return null;
  if (node.id === id) return node;
  for (const child of node.children ?? []) {
    const found = findById(child, id);
    if (found) return found;
  }
  return null;
}
function rowsOf(node: any): any[] {
  return node?.id?.match(/^wt-\d+$/) ? [node] : (node?.children ?? []).flatMap(rowsOf);
}

type HarnessOpts = {
  props?: Record<string, any>;
  exec?: (command: string, args: string[], options: any) => Promise<any>;
  language?: string;
};

function harness(focusedMachine: string | null = null, raw: string | null = null, opts: HarnessOpts = {}) {
  let spaceList: any[] = [];
  let paneList: any[] = [];
  const reads = { spaces: 0, panes: 0 };
  const writes: unknown[] = [];
  const destinations: (string | null)[] = [];
  const executions: any[] = [];
  let closed = 0;
  const propsObj = { overlay: "overlay", ...(opts.props ?? {}) };
  const globals = {
    View: class {}, div: element, v_flex: element, h_flex: element,
    Input: { new: element },
    InputState: { new: () => ({ on() {}, focus() {}, value: () => "", set_value() {} }) },
    Scrollbar: { vertical: element }, theme: () => ({}),
    home: () => "/home/user",
    appHome: () => "/app", readTextFile: async () => raw,
    settings: () => ({ language: opts.language ?? "en" }),
    spaces: () => { reads.spaces++; return structuredClone(spaceList); },
    panes: () => { reads.panes++; return structuredClone(paneList); },
    props: () => propsObj, closeOverlay: () => { closed++; },
    run: (command: string, args: any) => {
      writes.push([command, args]);
      if (command === "space.new") destinations.push(args.machine === "" ? null : args.machine ?? focusedMachine);
    },
    exec: async (command: string, args: string[], options: any) => {
      executions.push({ command, args, ...options });
      if (opts.exec) return opts.exec(command, args, options);
      return { code: 0, stdout: "", stderr: "" };
    },
  };
  const { Worktree, root } = new Function(...Object.keys(globals), `${source}; return { Worktree, root };`)(...Object.values(globals));
  const worktree = new Worktree();
  Object.assign(worktree, { opening: true, text: "", at: 0, repo: { name: "repo" }, userHome: "/home/user", root: "/home/user/.herdr/worktrees" });
  return {
    worktree, root, reads, writes, destinations, executions,
    closed: () => closed,
    snapshot: (spaces: unknown[], panes: unknown[]) => { spaceList = spaces; paneList = panes; },
  };
}

test("open render reads spaces and panes once and refreshes both on the next render", () => {
  const { worktree, reads, snapshot } = harness();
  worktree.list = Array.from({ length: 80 }, (_, i) => ({ path: `/repo/wt-${i}`, branch: `branch-${i}`, at: 0 }));
  const spaces = worktree.list.map((c: any, i: number) => ({ id: `s${i}`, tabs: [{ id: `tab${i}` }] }));
  const panes = worktree.list.map((c: any, i: number) => ({ tab: `tab${i}`, cwd: `${c.path}/src` }));
  snapshot(spaces, panes);
  const first = rowsOf(worktree.render({}).node);
  expect(reads).toEqual({ spaces: 1, panes: 1 });
  expect(first).toHaveLength(80);
  for (let i = 0; i < first.length; i++) {
    expect(textOf(first[i])).toContain(`branch-${i}`);
    expect(textOf(first[i])).toContain(" open ");
  }
  snapshot(spaces.slice(0, -1), panes.map((p: any, i: number) => i === 0 ? { ...p, cwd: "/elsewhere" } : p));
  const second = rowsOf(worktree.render({}).node);
  expect(reads).toEqual({ spaces: 2, panes: 2 });
  expect(textOf(second[0])).not.toContain(" open ");
  expect(textOf(second[79])).not.toContain(" open ");
  expect(textOf(second[1])).toContain(" open ");
});

test("open clicks read fresh snapshots once and select an existing space or create one", () => {
  const { worktree, reads, writes, snapshot } = harness();
  const checkout = { path: "/repo/worktree", branch: "branch", at: 0 };
  worktree.list = [checkout];
  snapshot([], []);
  worktree.render({});
  snapshot([{ id: "existing", tabs: [{ id: "tab" }] }], [{ tab: "tab", cwd: checkout.path }]);
  worktree.open(checkout);
  expect(reads).toEqual({ spaces: 2, panes: 2 });
  expect(writes).toEqual([["space.select", { id: "existing" }]]);
  snapshot([], []);
  worktree.open(checkout);
  expect(reads).toEqual({ spaces: 3, panes: 3 });
  expect(writes[1]).toEqual(["space.new", { folder: checkout.path, machine: "" }]);
  worktree.open(undefined);
  expect(reads).toEqual({ spaces: 3, panes: 3 });
});

for (const on of [undefined, null, "server"]) {
  for (const focused of [null, "server"]) {
    test(`create and open stay on ${on ?? "local"} while ${focused ?? "local"} is focused`, async () => {
      const { worktree, destinations, executions } = harness(focused);
      Object.assign(worktree, {
        on, text: "feature", repo: { name: "repo", main: "/repo", top: "/repo" }, list: [],
      });
      expect(await worktree.create()).toBe("");
      expect(executions).toHaveLength(2);
      expect(executions.every(e => (e.on ?? null) === (on ?? null))).toBe(true);
      expect(executions[1].args).toEqual(["worktree", "add", "/home/user/.herdr/worktrees/repo/feature", "feature"]);
      worktree.open({ path: "/repo/existing" });
      expect(destinations).toEqual([on ?? null, on ?? null]);
    });
  }
}

for (const [branch, folder] of [
  ["음악수행", "음악수행"], ["쪼지학", "쪼지학"],
  ["हिन्दी", "हिन्दी"], ["हिंदी", "हिंदी"],
  ["का", "का"], ["कि", "कि"],
  ["বাংলা", "বাংলা"],
  ["feature/हिन्दी", "feature-हिन्दी"],
  ["feature/음악수행-１２٣", "feature-음악수행-１２٣"],
  ["음악수행".normalize("NFD"), "음악수행"],
  ["feature/ASCII_1.2--fix", "feature-ASCII_1.2--fix"],
]) {
  test(`branch ${branch} uses folder ${folder}`, async () => {
    const { worktree, executions, writes } = harness();
    Object.assign(worktree, { text: branch, repo: { name: "repo", main: "/repo", top: "/repo" } });
    expect(await worktree.create()).toBe("");
    const path = `/home/user/.herdr/worktrees/repo/${folder}`;
    expect(executions[1].args).toEqual(["worktree", "add", path, branch]);
    expect(writes[0][1].folder).toBe(path);
  });
}

test("symbol-only branches get stable distinct non-empty folders", async () => {
  const folders: string[] = [];
  for (const branch of ["🎵", "🎶", "🎵"]) {
    const { worktree, executions } = harness();
    Object.assign(worktree, { text: branch, repo: { name: "repo", main: "/repo", top: "/repo" } });
    expect(await worktree.create()).toBe("");
    const path = executions[1].args[2];
    expect(path).toMatch(/\/repo\/branch-[0-9a-f]{8}$/);
    folders.push(path);
  }
  expect(folders[0]).toBe(folders[2]);
  expect(folders[0]).not.toBe(folders[1]);
});

test("space matching keeps deepest checkout, machine, live-folder and fallback rules", () => {
  const { worktree, reads, writes, snapshot } = harness();
  const paths = ["C:/repo", "C:/repo/nested", "C:/repo/fallback", "C:/repo-other"];
  worktree.list = paths.map(path => ({ path }));
  snapshot([
    { id: "remote", machine: "server", folder: paths[0] },
    { id: "nested", folder: paths[0], tabs: [{ id: "tab" }] },
    { id: "fallback", folder: paths[2] },
    { id: "other", folder: paths[3] },
  ], [
    { tab: "tab" },
    { tab: "tab", cwd: "C:\\repo\\nested\\src" },
    { tab: "tab", cwd: paths[0] },
  ]);
  for (const checkout of worktree.list) worktree.open(checkout);
  expect(writes).toEqual([
    ["space.new", { folder: paths[0], machine: "" }],
    ["space.select", { id: "nested" }],
    ["space.select", { id: "fallback" }],
    ["space.select", { id: "other" }],
  ]);
  worktree.on = "server";
  worktree.open(worktree.list[0]);
  expect(writes[4]).toEqual(["space.select", { id: "remote" }]);
  expect(reads).toEqual({ spaces: 5, panes: 5 });
});

test("configured roots drive both the checkout and its space, expanding the target home", async () => {
  for (const [directory, userHome, on, expected] of [
    [undefined, "/home/user", null, "/home/user/.herdr/worktrees"],
    ["~/Developer/worktrees", "/home/remote", "server", "/home/remote/Developer/worktrees"],
    ["/srv/worktrees/", "/home/remote", "server", "/srv/worktrees"],
    ["C:\\Developer\\Work Trees\\", "C:\\Users\\user", null, "C:/Developer/Work Trees"],
    ["~\\Developer\\worktrees", "C:\\Users\\user", "server", "C:/Users/user/Developer/worktrees"],
    ["~", "/home/user", null, "/home/user"],
    ["/", "/home/user", null, ""],
  ] as const) {
    const { worktree, root, executions, writes } = harness(null, JSON.stringify({ worktrees: { directory } }));
    Object.assign(worktree, { opening: false, on, userHome, text: "feature/new", repo: { name: "repo", main: "/repo", top: "/repo" }, root: await root(userHome) });
    const folder = `${expected}/repo/feature-new`;
    expect(await worktree.create()).toBe("");
    expect(executions[1].args).toEqual(["worktree", "add", folder, "feature/new"]);
    expect(executions[1].on).toBe(on);
    expect(writes).toEqual([["space.new", { folder, machine: on ?? "", tab: undefined, argv: undefined }]]);
    expect(textOf(worktree.render({}).node)).toContain(`${expected.replace(userHome.replace(/\\/g, "/"), "~")}/repo/`);
  }
});

test("missing settings keep the old location, while invalid directories cannot create elsewhere", async () => {
  expect(await harness().root("/home/user")).toBe("/home/user/.herdr/worktrees");
  for (const directory of ["", "   ", 42, {}, "relative/worktrees", "C:relative"]) {
    const { root } = harness(null, JSON.stringify({ worktrees: { directory } }));
    await expect(root("/home/user")).rejects.toThrow("worktrees.directory");
  }
  await expect(harness(null, "{").root("/home/user")).rejects.toThrow();
});

const gitEnv = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_AUTHOR_NAME: "Test",
  GIT_AUTHOR_EMAIL: "test@example.com",
  GIT_COMMITTER_NAME: "Test",
  GIT_COMMITTER_EMAIL: "test@example.com",
};

function runGitSync(cwd: string, ...args: string[]) {
  const r = Bun.spawnSync(["git", ...args], { cwd, env: gitEnv });
  if (r.exitCode !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.stderr.toString()}`);
  return r.stdout.toString().trim();
}

async function execRealGit(_command: string, args: string[], { cwd }: { cwd: string }) {
  const r = Bun.spawnSync(["git", ...args], { cwd, env: gitEnv });
  return { code: r.exitCode, stdout: r.stdout.toString(), stderr: r.stderr.toString() };
}

async function initRemoveDialog(folder: string, opts: { language?: string; exec?: HarnessOpts["exec"] } = {}) {
  const tasks: Promise<unknown>[] = [];
  let notified = 0;
  const cx = {
    focus_handle: () => ({ focus() {} }),
    timer: { after(_ms: number, fn: () => void) { fn(); } },
    notify() { notified++; },
    stop_propagation() {},
    spawn(fn: (acx: any) => Promise<unknown>) { tasks.push(fn(cx)); },
  };
  const h = harness(null, null, {
    props: { space: "wt-space", action: "remove" },
    language: opts.language ?? "en",
    exec: opts.exec ?? execRealGit,
  });
  h.snapshot([{ id: "wt-space", name: "feat-demo", folder }], []);
  h.worktree.init({}, cx);
  await Promise.all(tasks);
  return { ...h, cx, tasks, notified: () => notified };
}

test("remove dialog asks whether to also delete the branch, toggles via click and Space, and supports zh-CN", async () => {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "herdr-wt-ui-")));
  try {
    const repo = join(dir, "repo");
    const wt = join(dir, "wt-topic");
    mkdirSync(repo);
    runGitSync(repo, "init", "-b", "main");
    runGitSync(repo, "commit", "--allow-empty", "-m", "init");
    runGitSync(repo, "worktree", "add", "-b", "feat/topic", wt);

    const en = await initRemoveDialog(wt, { language: "en" });
    expect(en.worktree.branch).toBe("feat/topic");
    expect(en.worktree.deleteBranch).toBe(false);
    expect(en.worktree.branchUnmerged).toBe(false);

    const initialTree = en.worktree.render({}).node;
    expect(textOf(initialTree)).toContain('Also delete local branch "feat/topic"');
    expect(textOf(initialTree)).toContain('Local branch "feat/topic" will be kept.');
    expect(textOf(findById(initialTree, "wt-confirm"))).toContain("Remove worktree");

    findById(initialTree, "wt-delete-branch").onClick({}, en.cx);
    expect(en.worktree.deleteBranch).toBe(true);
    const checkedTree = en.worktree.render({}).node;
    expect(textOf(checkedTree)).toContain('deletes local branch "feat/topic"');
    expect(textOf(findById(checkedTree, "wt-confirm"))).toContain("Remove & delete branch");

    en.worktree.key({ key: "space" }, en.cx);
    expect(en.worktree.deleteBranch).toBe(false);

    const zh = await initRemoveDialog(wt, { language: "zh-CN" });
    const zhTree = zh.worktree.render({}).node;
    expect(textOf(zhTree)).toContain("移除工作树「feat-demo」？");
    expect(textOf(zhTree)).toContain("同步删除本地分支「feat/topic」");
    expect(textOf(findById(zhTree, "wt-confirm"))).toContain("移除工作树");
    findById(zhTree, "wt-delete-branch").onClick({}, zh.cx);
    expect(textOf(findById(zh.worktree.render({}).node, "wt-confirm"))).toContain("移除并删除分支");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("removing a worktree keeps the branch by default and deletes it when requested", async () => {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "herdr-wt-rm-")));
  try {
    const repo = join(dir, "repo");
    mkdirSync(repo);
    runGitSync(repo, "init", "-b", "main");
    runGitSync(repo, "commit", "--allow-empty", "-m", "base");

    // 1. Default (deleteBranch = false): keeps the branch.
    const wtKeep = join(dir, "wt-keep");
    runGitSync(repo, "worktree", "add", "-b", "feat/keep", wtKeep);
    const keep = await initRemoveDialog(wtKeep);
    expect(await keep.worktree.remove()).toBe("");
    expect(existsSync(wtKeep)).toBe(false);
    expect(keep.writes).toEqual([["space.close", { id: "wt-space", confirmed: true }]]);
    expect(runGitSync(repo, "branch", "--list", "feat/keep")).toContain("feat/keep");

    // 2. Merged branch with deleteBranch = true: deletes with -d.
    const wtMerged = join(dir, "wt-merged");
    runGitSync(repo, "worktree", "add", "-b", "feat/merged", wtMerged);
    const merged = await initRemoveDialog(wtMerged);
    merged.worktree.toggleDeleteBranch(merged.cx);
    expect(merged.worktree.deleteBranch).toBe(true);
    expect(merged.worktree.branchUnmerged).toBe(false);
    expect(await merged.worktree.remove()).toBe("");
    expect(existsSync(wtMerged)).toBe(false);
    expect(runGitSync(repo, "branch", "--list", "feat/merged")).toBe("");
    expect(merged.executions.some((e) => e.args[0] === "branch" && e.args[1] === "-d" && e.args[3] === "feat/merged")).toBe(true);

    // 3. Unmerged branch with deleteBranch = true: warns and force-deletes with -D.
    const wtUnmerged = join(dir, "wt-unmerged");
    runGitSync(repo, "worktree", "add", "-b", "feat/unmerged", wtUnmerged);
    writeFileSync(join(wtUnmerged, "note.txt"), "wip\n");
    runGitSync(wtUnmerged, "add", "note.txt");
    runGitSync(wtUnmerged, "commit", "-m", "unmerged work");
    const unmerged = await initRemoveDialog(wtUnmerged);
    expect(unmerged.worktree.branchUnmerged).toBe(true);
    expect(textOf(unmerged.worktree.render({}).node)).toContain("Unmerged");
    unmerged.worktree.toggleDeleteBranch(unmerged.cx);
    expect(await unmerged.worktree.remove()).toBe("");
    expect(existsSync(wtUnmerged)).toBe(false);
    expect(runGitSync(repo, "branch", "--list", "feat/unmerged")).toBe("");
    expect(unmerged.executions.some((e) => e.args[0] === "branch" && e.args[1] === "-D" && e.args[3] === "feat/unmerged")).toBe(true);

    // 4. Branch gains an unmerged commit after dialog opened: warns once before deleting.
    const wtRace = join(dir, "wt-race");
    runGitSync(repo, "worktree", "add", "-b", "feat/race", wtRace);
    const race = await initRemoveDialog(wtRace);
    expect(race.worktree.branchUnmerged).toBe(false);
    race.worktree.toggleDeleteBranch(race.cx);
    writeFileSync(join(wtRace, "late.txt"), "late commit\n");
    runGitSync(wtRace, "add", "late.txt");
    runGitSync(wtRace, "commit", "-m", "late");
    const warning = await race.worktree.remove();
    expect(warning).toContain("changed since this opened");
    expect(existsSync(wtRace)).toBe(true);
    expect(race.worktree.branchUnmerged).toBe(true);
    expect(await race.worktree.remove()).toBe("");
    expect(existsSync(wtRace)).toBe(false);
    expect(runGitSync(repo, "branch", "--list", "feat/race")).toBe("");

    // 5. Detached HEAD: no branch delete option shown, removes cleanly.
    const wtDetached = join(dir, "wt-detached");
    runGitSync(repo, "worktree", "add", "--detach", wtDetached);
    const detached = await initRemoveDialog(wtDetached);
    expect(detached.worktree.branch).toBeNull();
    expect(findById(detached.worktree.render({}).node, "wt-delete-branch")).toBeNull();
    expect(await detached.worktree.remove()).toBe("");
    expect(existsSync(wtDetached)).toBe(false);
    expect(detached.executions.some((e) => e.args[0] === "branch")).toBe(false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("if branch deletion fails after worktree removal, retry only deletes the branch without re-running worktree remove", async () => {
  const calls: string[][] = [];
  const h = await initRemoveDialog("/repo-wt", {
    exec: async (_cmd, args) => {
      calls.push(args);
      if (args.includes("--show-toplevel")) return { code: 0, stdout: "/repo-wt\n", stderr: "" };
      if (args.includes("--git-common-dir")) return { code: 0, stdout: "/repo/.git\n", stderr: "" };
      if (args.includes("--absolute-git-dir")) return { code: 0, stdout: "/repo/.git/worktrees/wt\n", stderr: "" };
      if (args[0] === "symbolic-ref") return { code: 0, stdout: "feat/retry\n", stderr: "" };
      if (args[0] === "show-ref") return { code: 0, stdout: "", stderr: "" };
      if (args[0] === "merge-base") return { code: 0, stdout: "", stderr: "" };
      if (args[0] === "branch" && args[1] === "-d") return { code: 1, stdout: "", stderr: "error: the branch 'feat/retry' is not fully merged." };
      return { code: 0, stdout: "", stderr: "" };
    },
  });
  h.worktree.toggleDeleteBranch(h.cx);
  calls.length = 0;

  const firstErr = await h.worktree.remove();
  expect(firstErr).toContain("not fully merged");
  expect(h.worktree.worktreeRemoved).toBe(true);
  expect(h.worktree.branchUnmerged).toBe(true);
  expect(calls.filter((a) => a[0] === "worktree" && a[1] === "remove")).toHaveLength(1);
  expect(textOf(findById(h.worktree.render({}).node, "wt-confirm"))).toContain("Force delete branch");

  calls.length = 0;
  expect(await h.worktree.remove()).toBe("");
  expect(calls).toEqual([["branch", "-D", "--", "feat/retry"]]);
});
