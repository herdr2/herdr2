// Worktree dialog: { space, action: "new" | "open" | "remove" } or
// { folder, machine } for a repo without a space; optional { branch, tab, argv }.

import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, Scrollbar } from "gpui-base";
import { run, props, closeOverlay, theme, spaces, panes, agents, focused, exec, h2, home, appHome, readTextFile, settings } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function sanitizeBranchCandidate(raw) {
  const line = String(raw ?? "")
    .trim()
    .split(/\r?\n/)[0]
    .replace(/^['"`]+|['"`]+$/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9/_-]+/g, "-")
    .replace(/\/+/g, "/")
    .replace(/-+/g, "-")
    .replace(/^[-/]+|[-/]+$/g, "");
  return line.slice(0, 48);
}

function localSemanticBranch(input) {
  const raw = String(input ?? "").trim();
  if (!raw) return "";
  const lower = raw.toLowerCase();
  const prefix = /(fix|bug|error|timeout|crash|修复|报错|异常|超时)/.test(lower)
    ? "fix"
    : /(refactor|cleanup|重构|优化)/.test(lower)
      ? "refactor"
      : /(doc|readme|文档)/.test(lower)
        ? "docs"
        : /(test|spec|测试)/.test(lower)
          ? "test"
          : "feat";
  const asciiWords = lower
    .replace(/[^a-z0-9]+/g, " ")
    .trim()
    .split(/\s+/)
    .filter((w) => w.length >= 2 && !["the", "and", "for", "with", "into", "from"].includes(w))
    .slice(0, 4);
  if (asciiWords.length > 0) {
    return `${prefix}/${asciiWords.join("-")}`;
  }
  return `${prefix}/${slug(raw)}`;
}

// Where a space is now: its first terminal's live folder, else where it was made.
function place(s, all = panes()) {
  if (!s) return null;
  const tabs = new Set((s.tabs ?? []).map((c) => c.id));
  return all.find((p) => tabs.has(p.tab) && p.cwd)?.cwd || s.folder || null;
}

// The mod owns this preference; paths belong to the space's machine, not the app.
async function root(userHome) {
  const raw = await readTextFile(`${appHome()}/settings.json`);
  const directory = (raw === null ? {} : JSON.parse(raw))?.worktrees?.directory ?? "~/.herdr/worktrees";
  if (typeof directory !== "string" || !directory.trim()) throw new Error("worktrees.directory must be a non-empty path.");
  let path = directory.replace(/\\/g, "/");
  if (path === "~" || path.startsWith("~/")) path = userHome.replace(/\\/g, "/").replace(/\/$/, "") + path.slice(1);
  if (!path.startsWith("/") && !/^[A-Za-z]:\//.test(path)) throw new Error("worktrees.directory must be an absolute path or start with ~/.");
  return path.replace(/\/+$/, "");
}
// Home as ~ (git says C:/Users/x where Windows says C:\Users\x).
const tilde = (path, userHome) => {
  const [p, h] = [path, userHome].map((s) => s.replace(/\\/g, "/"));
  return p.startsWith(h) ? `~${p.slice(h.length)}` : path;
};
const slug = (branch) => {
  const normalized = branch.normalize("NFC");
  const name = normalized.replace(/[^\p{L}\p{M}\p{N}._-]+/gu, "-").replace(/^-+|-+$/g, "");
  if (name) return name;
  // Hash the original name so symbol-only branches do not share a folder.
  let hash = 2166136261;
  for (const char of normalized) hash = Math.imul(hash ^ char.codePointAt(0), 16777619);
  return `branch-${(hash >>> 0).toString(16).padStart(8, "0")}`;
};
const ago = (s) => {
  const d = Date.now() / 1000 - s;
  return d < 3600 ? `${Math.max(1, Math.round(d / 60))}m` : d < 86400 ? `${Math.round(d / 3600)}h` : d < 86400 * 60 ? `${Math.round(d / 86400)}d` : `${Math.round(d / 86400 / 30)}mo`;
};

// Every checkout of a repo (`git worktree list`), the main one first, then
// the most recently committed: { path, branch, main, at }.
async function checkouts(repo, on) {
  const list = await git(repo.main, ["worktree", "list", "--porcelain"], on);
  if (!list.ok) return [];
  const refs = await git(repo.main, ["for-each-ref", "--format=%(refname:short)%09%(committerdate:unix)", "refs/heads"], on);
  const when = new Map(refs.out.split("\n").map((l) => l.split("\t")).map(([b, at]) => [b, Number(at) || 0]));
  const out = [];
  for (const block of list.out.split(/\n\n+/)) {
    const lines = block.split("\n");
    const path = lines.find((l) => l.startsWith("worktree "))?.slice(9);
    if (!path || lines.some((l) => l === "bare" || l.startsWith("prunable"))) continue;
    const branch = lines.find((l) => l.startsWith("branch "))?.slice(7).replace(/^refs\/heads\//, "") ?? null;
    out.push({ path, branch, main: out.length === 0, at: branch ? (when.get(branch) ?? 0) : 0 });
  }
  return out.sort((a, b) => b.main - a.main || b.at - a.at);
}

// The space showing a checkout: its first terminal is in it (the deepest checkout wins).
function spaceOf(path, all, on, spaceList, paneList) {
  const inside = (p, dir) => p === dir || p.startsWith(dir + "/");
  return spaceList.find((s) => (s.machine ?? null) === (on ?? null) && (() => {
    const at = place(s, paneList)?.replace(/\\/g, "/");
    if (!at || !inside(at, path)) return false;
    return !all.some((c) => c.path !== path && c.path.length > path.length && inside(at, c.path));
  })());
}

async function git(cwd, args, on, timeout) {
  const r = await exec("git", args, { cwd, on, ...(timeout === undefined ? {} : { timeout }) });
  return { ok: r.code === 0, out: (r.stdout ?? "").trim(), err: (r.stderr ?? r.stdout ?? "").trim() };
}

// The submodule repos git keeps for a checkout (nested ones too, checked out
// or not): { name, dir, pinned }. `config` is the args that read its
// .gitmodules; `pinned` is the commit its parent records ("" if none).
async function modules(gitDir, config, on, out = [], parent = "") {
  const keys = await git(gitDir, ["--git-dir", gitDir, "config", ...config, "-z", "--get-regexp", "^submodule\\..*\\.path$"], on);
  if (!keys.ok) return out; // no .gitmodules, or no submodules in it
  // -z: "key\nvalue" records ended by NUL, so names and paths may hold spaces.
  for (const record of keys.out.split("\0").filter(Boolean)) {
    const at = record.indexOf("\n");
    const name = record.slice("submodule.".length, at - ".path".length);
    const dir = `${gitDir}/modules/${name}`;
    if (!(await git(gitDir, ["--git-dir", dir, "rev-parse", "--git-dir"], on)).ok) continue; // never checked out
    const pinned = await git(gitDir, ["--git-dir", gitDir, "rev-parse", `HEAD:${record.slice(at + 1)}`], on);
    // Names are only unique within their parent: nested ones carry its name.
    out.push({ name: parent + name, dir, pinned: pinned.ok ? pinned.out : "" });
    if ((await git(gitDir, ["--git-dir", dir, "cat-file", "-e", "HEAD:.gitmodules"], on)).ok) await modules(dir, ["--blob", "HEAD:.gitmodules"], on, out, `${parent + name}/`);
  }
  return out;
}

// The worktree's submodules, and the names of those with commits that exist
// nowhere else: not on a remote, and not the commit the parent pins (git
// fetches that by hash, so it is often on no remote branch here).
async function submodules(top, on) {
  const gitDir = await git(top, ["rev-parse", "--absolute-git-dir"], on);
  if (!gitDir.ok) throw new Error(gitDir.err || "git rev-parse failed.");
  const mods = await modules(gitDir.out, ["--file", `${top}/.gitmodules`], on);
  const unpushed = [];
  for (const m of mods) {
    const local = await git(gitDir.out, ["--git-dir", m.dir, "rev-list", "-n1", "--all", "--not", "--remotes", ...(m.pinned ? [m.pinned] : [])], on);
    if (!local.ok) throw new Error(local.err || `Couldn't check submodule ${m.name}.`);
    if (local.out) unpushed.push(m.name);
  }
  return { mods, unpushed };
}

function withAlpha(color, alphaHex) {
  if (typeof color !== "string") return null;
  const c = color.trim();
  return /^#[0-9a-fA-F]{6}$/.test(c) ? `${c}${alphaHex}` : null;
}

// The repo a folder belongs to: its main checkout and name, and whether
// the folder is itself a linked worktree. null outside git.
async function repoOf(folder, on) {
  const top = await git(folder, ["rev-parse", "--show-toplevel"], on);
  const common = await git(folder, ["rev-parse", "--path-format=absolute", "--git-common-dir"], on);
  if (!top.ok || !common.ok) return null;
  const main = common.out.endsWith("/.git") ? common.out.slice(0, -5) : common.out;
  return { top: top.out, main, name: main.split("/").pop(), linked: main !== top.out };
}

// Local branch currently checked out in a worktree (if it exists as a ref), else null.
async function currentBranch(repo, on) {
  const head = await git(repo.top, ["symbolic-ref", "--quiet", "--short", "HEAD"], on);
  if (!head.ok || !head.out) return null;
  const exists = await git(repo.main, ["show-ref", "--verify", "--quiet", `refs/heads/${head.out}`], on);
  return exists.ok ? head.out : null;
}

// True when `branch` has commits not yet merged into the main checkout's HEAD or its upstream.
async function isBranchUnmerged(repoMain, branch, on) {
  if (!branch) return false;
  const ref = `refs/heads/${branch}`;
  const inHead = await git(repoMain, ["merge-base", "--is-ancestor", ref, "HEAD"], on);
  if (inHead.ok) return false;
  const upstream = await git(repoMain, ["rev-parse", "--verify", "--quiet", `${branch}@{upstream}`], on);
  if (upstream.ok && upstream.out) {
    const inUpstream = await git(repoMain, ["merge-base", "--is-ancestor", ref, upstream.out], on);
    if (inUpstream.ok) return false;
  }
  return true;
}

export default class Worktree extends View {
  init(_props, cx) {
    this.on = this.space()?.machine;
    this.userHome = this.on ? null : home();
    this.opening = props()?.action === "open";
    this.text = props()?.branch ?? "";
    this.list = undefined; // open: the checkouts, once read
    this.at = 0;
    this.input = InputState.new({ value: this.text, placeholder: this.opening ? "filter worktrees" : "branch name" });
    this.input.on("change", (_e, cx) => { this.text = this.input.value(); this.error = ""; this.at = 0; cx.notify(); });
    this.input.on("submit", (_e, cx) => this.go(cx));
    this.inputFocused = true;
    this.input.on("focus", (_e, cx) => { this.inputFocused = true; cx.notify(); });
    this.input.on("blur", (_e, cx) => { this.inputFocused = false; cx.notify(); });
    this.handle = cx.focus_handle();
    this.handle.focus(); // the root is mounted from the first frame: keep the keyboard here while loading
    if (props()?.action !== "remove") this.input.focus();
    this.busy = false;
    this.suggesting = false;
    this.error = "";
    this.repo = undefined; // undefined: looking, null: not in git
    this.dirty = false;
    this.unpushed = [];
    this.branch = null;
    this.deleteBranch = Boolean(props()?.deleteBranch);
    this.branchUnmerged = false;
    this.worktreeRemoved = false;
    cx.spawn(async (cx) => {
      try {
        if (this.on) {
          let result;
          try { result = await exec("sh", ["-c", 'printf %s "$HOME"'], { on: this.on }); } catch {}
          if (!result || result.code !== 0 || !result.stdout.trim()) result = await exec("powershell.exe", ["-NoProfile", "-Command", "[Console]::Write($HOME)"], { on: this.on });
          if (result.code !== 0 || !result.stdout.trim()) throw new Error("Could not find the remote home folder.");
          this.userHome = result.stdout.trim().replace(/\\/g, "/");
        }
        if (!this.opening && props()?.action !== "remove") this.root = await root(this.userHome);
        this.folder = props()?.folder || place(this.space()) || this.userHome;
        // The repo is set last: Enter does nothing until the warnings it consents to are known.
        const repo = await repoOf(this.folder, this.on);
        if (repo && props().action === "remove") {
          this.dirty = (await git(repo.top, ["--no-optional-locks", "-c", "core.fsmonitor=false", "status", "--porcelain"], this.on)).out !== "";
          this.unpushed = (await submodules(repo.top, this.on)).unpushed;
          if (repo.linked) {
            this.branch = await currentBranch(repo, this.on);
            if (this.branch) this.branchUnmerged = await isBranchUnmerged(repo.main, this.branch, this.on);
            else this.deleteBranch = false;
          }
        }
        if (repo && this.opening) this.list = await checkouts(repo, this.on);
        this.repo = repo;
        cx.notify();
        // The list (and with it the filter input) only mounts once loading is
        // done, so the keyboard must move there now — a focus call made while
        // loading lands on nothing and the dialog stays keyboard-dead.
        this.handle.focus();
        if (props().action !== "remove") cx.timer.after(100, () => this.input.focus());
        if (props().branch) await this.work(cx);
      } catch (error) { this.repo = null; this.error = String(error); cx.notify(); }
    });
  }

  space() {
    const p = props() ?? {};
    return spaces().find((s) => s.id === p.space) ?? (p.folder ? { folder: p.folder, machine: p.machine ?? null, name: "" } : undefined);
  }

  close() {
    closeOverlay(props().overlay);
  }

  toggleDeleteBranch(cx) {
    if (this.busy || !this.repo?.linked || !this.branch) return;
    this.deleteBranch = !this.deleteBranch;
    this.error = "";
    cx?.notify?.();
  }

  go(cx) {
    if (this.busy || !this.repo) return;
    if (props()?.action === "remove" && !this.repo.linked) return;
    cx.spawn(async (cx) => this.work(cx));
  }

  suggestBranch(cx) {
    if (this.busy || this.suggesting || !this.repo) return;
    const foc = typeof focused === "function" ? focused() : null;
    const focAddr = foc?.shown || foc?.terminal || foc?.agent || null;
    const focAgent = focAddr && typeof agents === "function" ? agents().find((a) => a.address === focAddr) : null;
    const seed = this.text.trim()
      || String(focAgent?.semantic_title || focAgent?.micro_summary || focAgent?.title || "").trim();
    if (!seed) {
      this.error = tr("Type a task description first to generate a semantic branch name.", "请先输入一句话任务描述，再点击生成语义分支名。");
      cx.notify();
      return;
    }
    const branch = sanitizeBranchCandidate(localSemanticBranch(seed));
    if (branch) {
      this.text = branch;
      this.input?.set_value?.(branch);
      this.error = "";
    }
    cx.notify();
  }

  async work(cx) {
    if (this.busy || !this.repo) return;
    if (props()?.action === "remove" && !this.repo.linked) return;
    this.busy = true;
    this.error = "";
    cx.notify();
    try { this.error = props().action === "remove" ? await this.remove() : await this.create(); }
    catch (error) { this.error = String(error); }
    this.busy = false;
    if (!this.error) this.close();
    cx.notify();
  }

  async create() {
    const branch = this.text.trim();
    if (!branch) return "Type a branch name.";
    const path = `${this.root}/${this.repo.name}/${slug(branch)}`;
    const exists = (await git(this.repo.main, ["show-ref", "--verify", "--quiet", `refs/heads/${branch}`], this.on)).ok;
    const add = exists ? ["worktree", "add", path, branch] : ["worktree", "add", "-b", branch, path, "HEAD"];
    const r = await git(this.repo.top, add, this.on, 600_000);
    if (!r.ok) return r.err || "git worktree add failed.";
    const p = props();
    // Leave spaces unnamed so they follow the checkout folder and nest under the repo.
    run("space.new", { folder: path, machine: this.on ?? "", tab: p.tab, argv: p.argv });
    return "";
  }

  shown() {
    const f = this.text.trim().toLowerCase();
    return (this.list ?? []).filter((c) => !f || `${c.branch ?? ""} ${c.path}`.toLowerCase().includes(f));
  }

  open(c) {
    if (!c) return;
    this.close();
    const s = spaceOf(c.path, this.list, this.on, spaces(), panes());
    if (s) run("space.select", { id: s.id });
    else run("space.new", { folder: c.path, machine: this.on ?? "" });
  }

  async remove() {
    const s = this.space();
    if (!this.worktreeRemoved) {
      const status = await git(this.repo.top, ["--no-optional-locks", "-c", "core.fsmonitor=false", "status", "--porcelain"], this.on);
      if (!status.ok) return status.err || tr("git status failed.", "git status 执行失败。");
      // git refuses a worktree with submodules unless forced, because their
      // history goes with it. The popup already showed what's at risk; only
      // risk that appeared since it opened gets one more warning.
      const { mods, unpushed } = await submodules(this.repo.top, this.on);
      let changed = (status.out && !this.dirty) || unpushed.some((name) => !this.unpushed.includes(name));
      this.dirty ||= status.out !== "";
      this.unpushed = [...new Set([...this.unpushed, ...unpushed])];
      const liveBranch = await currentBranch(this.repo, this.on);
      if (liveBranch !== this.branch) {
        const hadDelete = this.deleteBranch;
        this.branch = liveBranch;
        if (!this.branch) {
          this.deleteBranch = false;
          this.branchUnmerged = false;
        } else {
          this.branchUnmerged = await isBranchUnmerged(this.repo.main, this.branch, this.on);
        }
        if (hadDelete || liveBranch) changed = true;
      } else if (this.deleteBranch && this.branch) {
        const unmergedNow = await isBranchUnmerged(this.repo.main, this.branch, this.on);
        if (unmergedNow && !this.branchUnmerged) {
          this.branchUnmerged = true;
          changed = true;
        }
      }
      if (changed) {
        return tr(
          "It changed since this opened. Confirm again to remove it anyway.",
          "打开后工作树状态已变化，请再次确认以继续移除。",
        );
      }
      // Terminals first: a shell sitting in the folder keeps it busy on Windows.
      if (s?.id) run("space.close", { id: s.id, confirmed: true });
      const force = this.dirty || mods.length > 0;
      const r = await git(this.repo.main, ["worktree", "remove", ...(force ? ["--force"] : []), this.repo.top], this.on, 600_000);
      if (!r.ok) return r.err || tr("git worktree remove failed.", "git worktree remove 执行失败。");
      this.worktreeRemoved = true;
    }
    if (this.deleteBranch && this.branch) {
      const flag = this.branchUnmerged ? "-D" : "-d";
      const del = await git(this.repo.main, ["branch", flag, "--", this.branch], this.on);
      if (!del.ok) {
        if (!this.branchUnmerged && /not fully merged/i.test(del.err)) {
          this.branchUnmerged = true;
          return tr(
            `Worktree removed, but branch "${this.branch}" is not fully merged. Confirm again to force-delete it, or press Esc to keep it.`,
            `工作树已移除，但分支「${this.branch}」尚未完全合并。再次确认将强制删除该分支，或按 Esc 保留。`,
          );
        }
        return del.err
          ? tr(`Worktree removed, but failed to delete branch "${this.branch}": ${del.err}`, `工作树已移除，但删除分支「${this.branch}」失败：${del.err}`)
          : tr(`Worktree removed, but failed to delete branch "${this.branch}".`, `工作树已移除，但删除分支「${this.branch}」失败。`);
      }
    }
    return "";
  }

  key(e, cx) {
    if (e.key === "escape") return this.close();
    if (this.opening) {
      const n = this.shown().length;
      if (e.key === "enter") this.open(this.shown()[this.at]);
      else if ((e.key === "down" || e.key === "up") && n) this.at = (this.at + (e.key === "down" ? 1 : n - 1)) % n;
      else return;
      cx.stop_propagation();
      return cx.notify();
    }
    if (props()?.action === "remove") {
      if (e.key === "enter") {
        cx.stop_propagation();
        return this.go(cx);
      }
      if ((e.key === "space" || e.key === " ") && !this.busy && this.repo?.linked && this.branch) {
        cx.stop_propagation();
        return this.toggleDeleteBranch(cx);
      }
    }
  }

  renderOpen(t) {
    const r = this.repo;
    const rows = this.shown();
    const all = this.list ?? [];
    const spaceList = spaces();
    const paneList = panes();
    const row = (c, i) => {
      const on = i === this.at;
      const there = spaceOf(c.path, all, this.on, spaceList, paneList);
      return v_flex()
        .id(`wt-${i}`)
        .flex_none()
        .h(44)
        .px(10)
        .justify_center()
        .rounded(7)
        .cursor_pointer()
        .when(on, (el) => el.bg(t.selected))
        .when(!on, (el) => el.hover((s) => s.bg(t.hover)))
        .on_click(() => this.open(c))
        .child(
          h_flex()
            .gap(8)
            .items_center()
            .child(div().flex_1().min_w(0).truncate().text_color(t.foreground).child(c.branch ?? "detached"))
            .when(c.main, (el) => el.child(div().flex_none().text_size(11).text_color(t.dim).child("main checkout")))
            .when(Boolean(there), (el) => el.child(div().flex_none().text_size(11).text_color(t.accent).child("open")))
            .when(!there && c.at > 0, (el) => el.child(div().flex_none().text_size(11).text_color(t.dim).child(ago(c.at)))),
        )
        .child(div().truncate().text_size(11.5).text_color(t.dim).child(tilde(c.path, this.userHome ?? "")));
    };
    const count = rows.length === all.length ? `${all.length} checkouts` : `${rows.length}/${all.length} checkouts`;
    return v_flex()
      .id("worktree")
      .track_focus(this.handle)
      .on_key_down((e, cx) => this.key(e, cx))
      .size_full()
      .p(16)
      .gap(10)
      .bg(t.popover)
      .text_size(13)
      .child(h_flex().child(div().flex_1().font_semibold().text_color(t.foreground).child(`Open worktree${r ? ` of ${r.name}` : ""}`)).child(div().text_size(11).text_color(t.dim).child(r ? count : "")))
      .when(r === undefined || (r && this.list === undefined), (el) => el.child(div().text_color(t.muted).child("\u2026")))
      .when(r === null, (el) => el.child(div().text_color(t.muted).child(`${this.folder ? tilde(this.folder, this.userHome ?? "") : "This folder"} is not in a git repository.`)))
      .when(Boolean(r && this.list), (el) =>
        el
          .child(Input.new(this.input).h(32).px(10).items_center().rounded(6).border(1).border_color(this.inputFocused ? t.accent : t.input).bg(t.background).text_color(t.foreground))
          .child(
            div()
              .relative()
              .flex_1()
              .min_h(0)
              .child(
                v_flex()
                  .id("wt-list")
                  .size_full()
                  .overflow_y_scroll()
                  .children(rows.map(row))
                  .when(rows.length === 0, (el) => el.child(div().p(10).text_color(t.dim).child("No matching worktrees."))),
              )
              .child(Scrollbar.vertical("wt-list").absolute().inset_0()),
          ),
      )
      .child(h_flex().justify_end().text_size(11).text_color(t.dim).child("\u2191\u2193 choose \u00b7 \u21b5 open \u00b7 esc close"));
  }

  renderRemove(t) {
    const s = this.space();
    const r = this.repo;
    const userHome = this.userHome ?? "";
    const blocked = !r || !r.linked;
    const dangerBg = t.danger ?? t.blocked ?? "#dc2626";
    const accentBg = t.accent ?? t.solid ?? "#2563eb";
    const toggleAccent = this.branchUnmerged ? dangerBg : accentBg;
    const title = s?.name
      ? tr(`Remove worktree ${s.name}?`, `移除工作树「${s.name}」？`)
      : tr("Remove worktree?", "移除工作树？");
    const detail =
      r === undefined
        ? "…"
        : r === null
          ? tr(
              `${this.folder ? tilde(this.folder, userHome) : "This folder"} is not in a git repository.`,
              `${this.folder ? tilde(this.folder, userHome) : "该目录"} 不在 Git 仓库中。`,
            )
          : !r.linked
            ? tr("This is the repository's main checkout, not a worktree.", "这是仓库的主检出目录，不是工作树。")
            : this.worktreeRemoved
              ? tr(`Worktree ${tilde(r.top, userHome)} has already been removed.`, `工作树 ${tilde(r.top, userHome)} 已移除。`)
              : this.branch
                ? this.deleteBranch
                  ? tr(
                      `Closes the space, deletes ${tilde(r.top, userHome)}, and deletes local branch "${this.branch}".`,
                      `关闭工作区、删除 ${tilde(r.top, userHome)}，并同步删除本地分支「${this.branch}」。`,
                    )
                  : tr(
                      `Closes the space and deletes ${tilde(r.top, userHome)}. Local branch "${this.branch}" will be kept.`,
                      `关闭工作区并删除 ${tilde(r.top, userHome)}，本地分支「${this.branch}」将被保留。`,
                    )
                : tr(
                    `Closes the space and deletes ${tilde(r.top, userHome)}. HEAD is detached, so no local branch is affected.`,
                    `关闭工作区并删除 ${tilde(r.top, userHome)}（当前处于游离 HEAD，无关联本地分支）。`,
                  );

    const confirmLabel = this.worktreeRemoved
      ? this.deleteBranch && this.branch
        ? this.branchUnmerged
          ? tr("Force delete branch", "强制删除分支")
          : tr("Delete branch", "删除分支")
        : tr("Done", "完成")
      : this.deleteBranch && this.branch
        ? tr("Remove & delete branch", "移除并删除分支")
        : tr("Remove worktree", "移除工作树");

    const cancelLabel = blocked || (this.worktreeRemoved && !this.deleteBranch)
      ? tr("Close", "关闭")
      : tr("Cancel", "取消");

    const button = (id, label, bg, fg, borderColor, onClick) =>
      h_flex()
        .id(id)
        .h(32)
        .px(14)
        .items_center()
        .rounded(8)
        .border_1()
        .border_color(borderColor)
        .bg(bg)
        .text_size(12)
        .text_color(fg)
        .cursor_pointer()
        .hover((st) => st.opacity(0.88))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click(onClick)
        .child(label);

    return v_flex()
      .id("worktree")
      .track_focus(this.handle)
      .on_key_down((e, cx) => this.key(e, cx))
      .w_full()
      .min_w(0)
      .p(18)
      .gap(12)
      .rounded(8)
      .bg(t.popover)
      .text_size(13)
      .child(
        h_flex()
          .w_full()
          .min_w(0)
          .items_center()
          .gap(8)
          .child(div().flex_1().min_w(0).text_size(15).font_semibold().text_color(t.foreground).child(title))
          .child(div().flex_none().text_size(11).text_color(t.dim).child("Esc")),
      )
      .child(div().w_full().min_w(0).whitespace_normal().line_height("20px").text_color(t.muted).child(detail))
      .when(!this.worktreeRemoved && this.dirty, (el) =>
        el.child(
          div()
            .w_full()
            .min_w(0)
            .whitespace_normal()
            .text_color(t.blocked)
            .child(tr("It has uncommitted changes: they will be lost.", "工作树存在未提交的修改，移除后将会丢失。")),
        ),
      )
      .when(!this.worktreeRemoved && this.unpushed.length > 0, (el) =>
        el.child(
          div()
            .w_full()
            .min_w(0)
            .whitespace_normal()
            .text_color(t.blocked)
            .child(
              tr(
                `Submodule ${this.unpushed.join(", ")} has commits that aren't pushed anywhere: they will be lost.`,
                `子模块 ${this.unpushed.join(", ")} 存在未推送的提交，移除后将会丢失。`,
              ),
            ),
        ),
      )
      .when(!blocked && Boolean(this.branch), (el) =>
        el.child(
          h_flex()
            .id("wt-delete-branch")
            .w_full()
            .min_w(0)
            .px(10)
            .py(8)
            .gap(10)
            .items_start()
            .rounded(8)
            .border_1()
            .border_color(this.deleteBranch ? toggleAccent : t.border)
            .bg(this.deleteBranch ? (withAlpha(toggleAccent, "14") ?? t.raised) : t.raised)
            .cursor_pointer()
            .hover((st) => st.bg(t.hover))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              this.toggleDeleteBranch(cx);
            })
            .child(
              div()
                .mt(1)
                .w(15)
                .h(15)
                .flex_none()
                .rounded(4)
                .border_1()
                .border_color(this.deleteBranch ? toggleAccent : (t.input ?? t.border))
                .bg(this.deleteBranch ? toggleAccent : t.background)
                .flex()
                .items_center()
                .justify_center()
                .text_size(10)
                .font_bold()
                .text_color("#ffffff")
                .child(this.deleteBranch ? "✓" : ""),
            )
            .child(
              v_flex()
                .flex_1()
                .min_w(0)
                .gap(3)
                .child(
                  h_flex()
                    .w_full()
                    .min_w(0)
                    .items_center()
                    .flex_wrap()
                    .gap(6)
                    .child(
                      div()
                        .min_w(0)
                        .whitespace_normal()
                        .text_size(12.5)
                        .font_semibold()
                        .text_color(t.foreground)
                        .child(tr(`Also delete local branch "${this.branch}"`, `同步删除本地分支「${this.branch}」`)),
                    )
                    .when(this.branchUnmerged, (badgeEl) =>
                      badgeEl.child(
                        div()
                          .flex_none()
                          .px(5)
                          .py(1)
                          .rounded(4)
                          .bg("#f59e0b22")
                          .border_1()
                          .border_color("#f59e0b55")
                          .text_size(10)
                          .font_semibold()
                          .text_color("#f59e0b")
                          .child(tr("Unmerged", "未合并")),
                      ),
                    ),
                )
                .child(
                  div()
                    .w_full()
                    .min_w(0)
                    .whitespace_normal()
                    .line_height("16px")
                    .text_size(11)
                    .text_color(this.branchUnmerged && this.deleteBranch ? t.blocked : t.muted)
                    .child(
                      this.branchUnmerged
                        ? tr(
                            "Has unmerged commits — checking this will force-delete (-D) and lose unmerged branch history.",
                            "包含未合并提交 — 勾选后将强制删除（-D），未合并分支历史可能丢失。",
                          )
                        : tr(
                            "Deletes the local branch after removing the worktree (remote branches are not affected).",
                            "移除工作树后同步删除本地分支（不影响远程分支）。",
                          ),
                    ),
                ),
            ),
        ),
      )
      .when(Boolean(this.error), (el) =>
        el.child(div().w_full().min_w(0).whitespace_normal().text_color(t.blocked).child(this.error)),
      )
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .flex_wrap()
          .gap(8)
          .pt(2)
          .child(
            h_flex()
              .items_center()
              .gap(6)
              .text_size(11)
              .text_color(t.dim)
              .when(this.busy, (el) => el.child(div().frames(["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"], 90)))
              .child(
                this.busy
                  ? tr("Working…", "处理中…")
                  : blocked
                    ? tr("Esc close", "Esc 关闭")
                    : this.branch
                      ? tr("Space toggle branch · ↵ confirm", "空格 切换删分支 · ↵ 确认")
                      : tr("↵ remove · Esc cancel", "↵ 移除 · Esc 取消"),
              ),
          )
          .child(
            h_flex()
              .items_center()
              .gap(8)
              .child(
                button("wt-cancel", cancelLabel, t.raised, t.foreground, t.input ?? t.border, (_e, cx) => {
                  cx.stop_propagation();
                  this.close();
                }),
              )
              .when(!blocked && !this.busy, (el) =>
                el.child(
                  button("wt-confirm", confirmLabel, dangerBg, "#ffffff", dangerBg, (_e, cx) => {
                    cx.stop_propagation();
                    this.go(cx);
                  }),
                ),
              ),
          ),
      );
  }

  render(cx) {
    const t = theme();
    if (this.opening) return this.renderOpen(t);
    if (props()?.action === "remove") return this.renderRemove(t);
    const r = this.repo;
    const title = `New worktree${r ? ` of ${r.name}` : ""}`;
    const detail =
      r === undefined
        ? "…"
        : r === null
          ? `${this.folder ? tilde(this.folder, this.userHome ?? "") : "This folder"} is not in a git repository.`
          : `A new space on a checkout in ${tilde(this.root, this.userHome ?? "")}/${r.name}/. A branch that exists is checked out; a new one starts from here.`;
    const blocked = !r;
    return v_flex()
      .id("worktree")
      .track_focus(this.handle)
      .on_key_down((e, cx) => this.key(e, cx))
      .size_full()
      .p(16)
      .gap(10)
      .bg(t.popover)
      .text_size(13)
      .child(div().font_semibold().text_color(t.foreground).child(title))
      .child(div().text_color(t.muted).child(detail))
      .when(!blocked, (el) =>
        el.child(
          h_flex()
            .gap(6)
            .items_center()
            .child(
              Input.new(this.input)
                .flex_1()
                .h(32)
                .px(10)
                .items_center()
                .rounded(6)
                .border(1)
                .border_color(this.inputFocused ? t.accent : t.input)
                .bg(t.background)
                .text_color(t.foreground),
            )
            .child(
              div()
                .id("wt-suggest-branch")
                .h(32)
                .px(9)
                .flex_none()
                .flex()
                .items_center()
                .rounded(6)
                .border(1)
                .border_color(t.border)
                .bg(t.raised)
                .text_size(11.5)
                .text_color(this.suggesting ? t.dim : t.foreground)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .tooltip(tr("Convert task text (or focused agent title) into a clean semantic git branch name", "根据任务描述或当前 Agent 语义标题生成规范分支名"))
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  this.suggestBranch(cx);
                })
                .child(this.suggesting ? "…" : tr("✨ Name", "✨ 语义名")),
            ),
        ),
      )
      .when(Boolean(this.error), (el) => el.child(div().text_color(t.blocked).child(this.error)))
      .child(div().flex_1())
      .child(
        h_flex()
          .justify_end()
          .text_size(11)
          .text_color(t.dim)
          .gap(6)
          .when(this.busy, (el) => el.child(div().frames(["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"], 90)))
          .child(this.busy ? "working…" : blocked ? "esc close" : "↵ create · esc cancel"),
      );
  }
}
