import { View, div, svg, list } from "gpui-kit";
import { v_flex, h_flex, h_resizable, resizable_panel, InputState, Input, TextareaState, Textarea, Popup, Scrollbar } from "gpui-base";
import { spaces, panes, agents, terminals, focused, exec, h2, readTextFile, watchFolder, theme, settings, shared, setShared, run, home } from "herdr";
import {
  annotateDiffRows,
  buildSelectionRange,
  extractSnippet,
  buildDisplayItems,
  buildSplitDisplayItems,
  matchCommentRowIndex,
  resolveTargetCandidates,
  makeDraftScopeKey,
  readScopeDrafts,
  writeScopeDrafts,
  formatLineTag,
  withAlpha,
  normalizePath,
  pathsEqual,
  isAbsolutePath,
  parentDirectory,
  deriveLogicalRepoRoot,
  resolvePathAgainstRoot,
  resolvePathAgainstProject,
  parseToolPatchLines,
} from "./review_model.js";
import {
  parseGitStatusV2,
  parseGitLog,
  parseStashCount,
  formatPrimaryRef,
} from "./git_info.js";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function location() {
  const space = spaces().find((s) => s.active), f = focused();
  const pane = f?.pane ? panes().find((p) => p.pane === f.pane) : null;
  if (f?.agent) {
    const agentRec = terminals().find((t) => t.address === f.agent) ?? null;
    const on = f.agent.includes("/") ? f.agent.split("/")[0] : null;
    return { cwd: f.cwd || agentRec?.cwd || pane?.cwd || space?.folder, on };
  }
  const on = f?.pane === null ? (f.terminal?.includes("/") ? f.terminal.split("/")[0] : null) : (space?.machine || null);
  return { cwd: pane?.cwd || f?.cwd || space?.folder, on };
}
const key = () => JSON.stringify(location());
// Reads write nothing under .git (no index refresh or lock, no fsmonitor cookie):
// the folder watch would see it, and every refresh would start the next one.
const READ = ["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "diff.autoRefreshIndex=false"];
const countLines = (text) => text ? text.split("\n").length - Number(text.endsWith("\n")) : 0;

// Text draws a tab as nothing: pad each to the next 4-column stop.
const untab = (text) => text.replace(/[^\t]*\t/g, (s) => s.slice(0, -1).padEnd((s.length - 1) - (s.length - 1) % 4 + 4));

// Numbers each diff line once, so a frame only draws the rows on screen.
function diffRows(lines) {
  let old = 0, next = 0;
  return lines.map((line) => {
    const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(line);
    if (match) { old = +match[1]; next = +match[2]; return { kind: "@", text: line }; }
    const kind = line[0];
    if (kind === "-") return { kind, number: old++, text: untab(line.slice(1)) || " " };
    if (kind === "+") return { kind, number: next++, text: untab(line.slice(1)) || " " };
    if (kind === " ") { old++; return { kind, number: next++, text: untab(line.slice(1)) || " " }; }
    return { kind: "", text: line };
  });
}

export default class Changes extends View {
  init(_props, cx) {
    this.files = [];
    this.rows = [];
    this.annotatedRows = [];
    this.diffNumWidth = 30;
    this.message = "";
    this.handle = cx.focus_handle();
    this.input = InputState.new({ placeholder: tr("Commit message", "输入提交信息") });
    this.input.on("change", (_e, cx) => {
      const hadMessage = Boolean(this.message.trim());
      this.message = this.input.value();
      if (hadMessage !== Boolean(this.message.trim())) cx.notify();
    });
    this.input.on("submit", (_e, cx) => this.commit(cx));
    this.input.on("focus", (_e, cx) => { this.typing = true; cx.notify(); });
    this.input.on("blur", (_e, cx) => { this.typing = false; cx.notify(); });

    // Single reusable inline comment editor (created once in init(), never inside render()).
    this.commentInput = TextareaState.new({
      placeholder: tr("Ask, note, or comment on selected lines… (Ctrl/Cmd+Enter to add)", "针对选中行提问、批注或写修改说明… (Ctrl/Cmd+Enter 添加)"),
    });
    this.commentInput.set_auto_grow(2, 4);
    this.commentTyping = false;
    this.commentInput.on("focus", (_e, cx) => { this.commentTyping = true; cx.notify(); });
    this.commentInput.on("blur", (_e, cx) => { this.commentTyping = false; cx.notify(); });
    this.commentInput.on("change", (_e, cx) => { cx.notify(); });
    this.commentInput.on("submit", (e, cx) => {
      if (e?.ctrl || e?.meta || (!e?.shift && e?.key === "enter")) {
        this.saveComposer(cx, false);
      }
    });

    this.composer = null;
    this.anchorRowIdx = null;
    this.lastShift = false;
    this.targetOverride = null;
    this.targetPopupOpen = false;
    this.steerMode = false;
    this.sendingReqId = null;
    this.sendingIds = [];
    this.reviewError = null;
    this.lastHandledResultId = null;
    this.aiCommitLoading = false;
    this.aiCommitUsedAi = false;
    this.gitBranch = null;
    this.gitStatus = null;
    this.gitLog = [];
    this.stashCount = 0;
    this.gitLogOpen = false;
    this.selectedCommit = null;
    this.commitLoading = false;
    this.root = null;
    this.logicalRoot = null;
    this.cwd = null;
    this.externalFiles = [];
    this.selectedExternal = null;
    this.externalLoading = false;
    this.pendingOpenRequest = null;
    this.lastHandledOpenSeq = 0;
    try {
      const existingReq = shared("changes.open_request");
      const reqSeq = Number(existingReq?.seq ?? 0);
      const reqTs = Number(existingReq?.ts ?? 0);
      if (reqSeq > 0 && reqTs > 0 && Date.now() - reqTs > 15000) {
        this.lastHandledOpenSeq = reqSeq;
      }
    } catch {}

    this.refresh(cx);
    this.consumeOpenRequest(cx);
    cx.timer.every(10000, (cx) => this.refresh(cx));
  }

  watch(root, cx) {
    if (this.on || this.watched === root) return;
    this.watched = root;
    cx.spawn(async (cx) => {
      try {
        while (this.watched === root) {
          await watchFolder(root);
          if (this.watched === root) this.refresh(cx);
        }
      } catch { this.watched = null; }
    });
  }

  viewKey() {
    return `${key()}:${shared("checkpoint.sync") ?? ""}`;
  }

  scopeKey() {
    if (this.root) return makeDraftScopeKey(this.on, this.root);
    if (this.selectedExternal?.path) {
      return makeDraftScopeKey(
        this.selectedExternal.on ?? this.on,
        this.selectedExternal.extRoot || parentDirectory(this.selectedExternal.path) || this.selectedExternal.path,
      );
    }
    return null;
  }

  expandLocalPath(rawPath, on) {
    const p = String(rawPath ?? "").trim();
    if (!p) return "";
    if (!on && p.startsWith("~/") && typeof home === "function") {
      const h = String(home() ?? "").replace(/[\\/]+$/, "");
      if (h) return `${h}/${p.slice(2)}`;
    }
    return p;
  }

  consumeOpenRequest(cx) {
    let req = null;
    try { req = shared("changes.open_request"); } catch {}
    if (!req || typeof req !== "object") return;
    const seq = Number(req.seq ?? 0);
    if (!seq || seq <= this.lastHandledOpenSeq) return;
    if (typeof req.path !== "string" || !req.path.trim()) return;
    this.lastHandledOpenSeq = seq;
    if (this.loading || this.busy) {
      this.pendingOpenRequest = req;
      return;
    }
    this.applyOpenRequest(req, cx);
  }

  resolveInCurrentProject(entry, reqOn, reqFromCheckpoint = false) {
    const loc = location();
    const currentOn = (this.on !== undefined ? this.on : loc.on) || null;
    const sameMachine = (reqOn || null) === currentOn;
    const ep = this.expandLocalPath(entry?.path ?? "", reqOn);
    const disp = String(entry?.displayPath ?? "").trim();
    if (!ep && !disp) {
      return { sameMachine, inside: false, inRepo: false, relPath: null, absPath: "" };
    }
    if (!sameMachine) {
      return { sameMachine: false, inside: false, inRepo: false, relPath: null, absPath: normalizePath(ep || disp) };
    }

    const fromCheckpoint = Boolean(entry?.fromCheckpoint ?? reqFromCheckpoint);
    const projectFolder = this.cwd || loc.cwd || null;
    let res = resolvePathAgainstProject(
      { path: ep, displayPath: disp, fromCheckpoint },
      { repoRoot: this.root, logicalRoot: this.logicalRoot, projectFolder },
    );

    // Fallback for relative displayPath matching an existing working-tree entry
    // (e.g. when agent cwd is a subdirectory of the Git repository).
    if (this.root && disp && !isAbsolutePath(disp)) {
      const cleanDisp = normalizePath(disp);
      if (cleanDisp && !cleanDisp.startsWith("..") && this.files.some((f) => f.path === cleanDisp)) {
        if (!res.inside || !this.files.some((f) => f.path === res.relPath)) {
          const normRoot = normalizePath(this.root);
          const rootPrefix = normRoot === "/" ? "" : normRoot.replace(/\/+$/, "");
          res = { inside: true, inRepo: true, relPath: cleanDisp, absPath: `${rootPrefix}/${cleanDisp}` };
        }
      }
    }

    return { sameMachine: true, ...res };
  }

  openInRepoFile(relPath, cx) {
    this.selectedCommit = null;
    this.commitLoading = false;
    this.selectedExternal = null;
    this.externalLoading = false;
    this.open = relPath;
    this.closed = false;
    this.composer = null;
    this.anchorRowIdx = null;
    this.note = null;
    this.setLines([]);
    const context = this.context;
    cx.spawn(async (cx) => {
      try { await this.load(relPath, context); }
      catch (e) { if (this.viewKey() === context && this.open === relPath) this.note = `Cannot preview this diff (too large or unreadable): ${e}`; }
      cx.notify();
    });
    cx.notify();
  }

  applyOpenRequest(req, cx) {
    const reqOn = req?.on || null;
    const rawPath = this.expandLocalPath(req?.path ?? "", reqOn);
    if (!rawPath) return;

    const primaryRes = this.resolveInCurrentProject(
      { path: rawPath, displayPath: req?.displayPath, fromCheckpoint: req?.fromCheckpoint },
      reqOn,
      Boolean(req?.fromCheckpoint),
    );

    if (primaryRes.sameMachine && primaryRes.inside) {
      if (primaryRes.inRepo && primaryRes.relPath && this.files.some((f) => f.path === primaryRes.relPath)) {
        this.openInRepoFile(primaryRes.relPath, cx);
        return;
      }
      // `this.files` may be stale (e.g. remote machine without watchFolder, or before the
      // 10s poll / watcher fired). Refresh once before deciding there is no uncommitted diff.
      if (primaryRes.inRepo && !req?._refreshed && !this.busy) {
        this.pendingOpenRequest = { ...req, _refreshed: true };
        this.refresh(cx);
        if (this.loading) return;
        this.pendingOpenRequest = null;
      }
      // Still inside the current project (e.g. already committed, gitignored, clean, or
      // non-Git workspace): preview it as an in-project file without adding it to `externalFiles`.
      const inProjectTarget = {
        path: primaryRes.absPath || rawPath,
        displayPath: primaryRes.relPath || String(req?.displayPath || rawPath),
        relPath: primaryRes.relPath || null,
        inProject: true,
        on: reqOn,
        add: Number(req?.additions ?? 0) || 0,
        del: Number(req?.deletions ?? 0) || 0,
        status: (Number(req?.deletions ?? 0) || 0) > 0 ? "M" : "A",
        patch: typeof req?.patch === "string" ? req.patch : "",
        isCreateOnly: Boolean(req?.isCreateOnly),
        sourceKind: null,
        extRoot: primaryRes.inRepo ? this.root : null,
        terminal: req?.terminal ?? null,
        turn: req?.turn ?? null,
        _refreshed: Boolean(req?._refreshed),
      };
      this.pickExternal(inProjectTarget, cx);
      return;
    }

    const prevByPath = new Map((this.externalFiles ?? []).map((f) => [f.path, f]));
    const nextList = [];
    const seen = new Set();
    const pushExt = (entry, isPrimary = false) => {
      const ep = this.expandLocalPath(entry?.path ?? "", reqOn);
      if (!ep || seen.has(ep)) return null;
      if (!isPrimary) {
        const res = this.resolveInCurrentProject(entry, reqOn, Boolean(req?.fromCheckpoint));
        if (res.sameMachine && res.inside) return null;
      }
      seen.add(ep);
      const prev = prevByPath.get(ep);
      const add = Number(entry?.additions ?? entry?.add ?? prev?.add ?? 0) || 0;
      const del = Number(entry?.deletions ?? entry?.del ?? prev?.del ?? 0) || 0;
      const item = {
        path: ep,
        displayPath: String(entry?.displayPath || prev?.displayPath || ep),
        relPath: null,
        inProject: false,
        on: reqOn,
        add,
        del,
        status: prev?.status || (del > 0 ? "M" : "A"),
        patch: typeof entry?.patch === "string" && entry.patch ? entry.patch : (prev?.patch || ""),
        isCreateOnly: entry?.isCreateOnly !== undefined ? Boolean(entry.isCreateOnly) : Boolean(prev?.isCreateOnly),
        sourceKind: prev?.sourceKind || null,
        extRoot: prev?.extRoot || null,
        terminal: req?.terminal ?? prev?.terminal ?? null,
        turn: req?.turn ?? prev?.turn ?? null,
      };
      nextList.push(item);
      return item;
    };

    const rawSiblings = Array.isArray(req?.files) ? req.files : [];
    for (const sf of rawSiblings) {
      const sfPath = this.expandLocalPath(sf?.path ?? "", reqOn);
      pushExt(sf, sfPath === rawPath);
    }
    let target = nextList.find((x) => x.path === rawPath) ?? null;
    if (!target) {
      target = pushExt({
        path: rawPath,
        displayPath: req?.displayPath || rawPath,
        additions: req?.additions ?? 0,
        deletions: req?.deletions ?? 0,
        patch: req?.patch ?? "",
        isCreateOnly: req?.isCreateOnly ?? false,
      }, true);
    }
    if (!target) return;

    this.externalFiles = nextList;
    this.pickExternal(target, cx);
  }

  drafts() {
    const sk = this.scopeKey();
    if (!sk) return [];
    return readScopeDrafts(shared("diff_review.drafts"), sk);
  }

  setDrafts(nextList) {
    const sk = this.scopeKey();
    if (!sk) return;
    const updated = writeScopeDrafts(shared("diff_review.drafts"), sk, nextList);
    setShared("diff_review.drafts", updated);
  }

  syncLastResult() {
    const res = shared("diff_review.last_result");
    if (!res || typeof res !== "object") return;
    const rid = res.request_id;
    if (!rid || rid === this.lastHandledResultId) return;
    if (this.sendingReqId && rid === this.sendingReqId) {
      this.lastHandledResultId = rid;
      this.sendingReqId = null;
      if (res.ok) {
        const sentSet = new Set(this.sendingIds ?? []);
        const remaining = this.drafts().filter((c) => !sentSet.has(c.id));
        this.setDrafts(remaining);
        this.sendingIds = [];
        this.reviewError = null;
      } else {
        this.sendingIds = [];
        this.reviewError = String(res.error || tr("Failed to send line notes", "发送行级消息失败"));
      }
    }
  }

  refresh(cx) {
    if (this.loading || this.busy) return;
    const { cwd, on } = location(), context = this.viewKey();
    if (context !== this.context) {
      const folderChanged = this.folderKey !== key();
      this.folderKey = key();
      this.context = context;
      if (folderChanged) {
        this.root = null;
        this.logicalRoot = null;
        this.cwd = null;
        this.files = [];
        this.open = null;
        this.gitBranch = null;
        this.gitStatus = null;
        this.gitLog = [];
        this.stashCount = 0;
        this.selectedCommit = null;
        this.commitLoading = false;
        this.externalFiles = [];
        this.selectedExternal = null;
        this.externalLoading = false;
        this.composer = null;
        this.anchorRowIdx = null;
        this.targetPopupOpen = false;
        this.reviewError = null;
        this.setLines([]);
        this.message = "";
        this.input.set_value("");
        this.error = null;
        this.note = null;
        this.watched = null;
        this.closed = false;
        this.limited = false;
      }
    }
    if (!cwd) return;
    this.loading = true;
    cx.spawn(async (cx) => {
      try {
        const top = await exec("git", ["rev-parse", "--show-toplevel", "--show-prefix"], { cwd, on });
        if (this.viewKey() !== context) return;
        if (top.code !== 0) {
          this.root = null;
          this.logicalRoot = null;
          this.cwd = cwd;
          this.on = on;
          this.files = [];
          this.gitBranch = null;
          this.gitStatus = null;
          this.gitLog = [];
          this.stashCount = 0;
          this.selectedCommit = null;
          this.commitLoading = false;
          if (!this.selectedExternal) {
            this.setLines([]);
            this.open = null;
          }
          this.error = "Not a git repository";
          return;
        }
        const [topRootLine = "", topPrefixLine = ""] = String(top.stdout ?? "").replace(/\r\n/g, "\n").split("\n");
        const root = topRootLine.trim(), git = (args) => exec("git", [...READ, ...args], { cwd: root, on });
        const logicalRoot = deriveLogicalRepoRoot(cwd, topPrefixLine.trim());
        const [statusRes, logRes, stashRes] = await Promise.all([
          git(["status", "--porcelain=v2", "--branch", "-z", "--no-renames", "--untracked-files=all"]),
          git(["log", "-n", "8", "--format=%H%x1f%h%x1f%s%x1f%an%x1f%ar%x1f%D%x1e"]).catch(() => ({ code: 1, stdout: "", stderr: "" })),
          git(["stash", "list"]).catch(() => ({ code: 1, stdout: "", stderr: "" })),
        ]);
        if (statusRes.code !== 0) throw new Error(statusRes.stderr);
        const parsed = parseGitStatusV2(statusRes.stdout, { limit: 1000 });
        const hasHead = parsed.branch.hasHead;
        const gitLog = logRes.code === 0 ? parseGitLog(logRes.stdout) : [];
        const stashCount = stashRes.code === 0 ? parseStashCount(stashRes.stdout) : 0;

        const stats = {};
        if (parsed.files.length > 0) {
          const results = hasHead
            ? [await git(["diff", "HEAD", "--numstat", "-z", "--no-renames"])]
            : [await git(["diff", "--cached", "--numstat", "-z", "--no-renames"]), await git(["diff", "--numstat", "-z", "--no-renames"])];
          for (const result of results) {
            if (result.code !== 0) throw new Error(result.stderr);
            for (const entry of result.stdout.split("\0")) {
              const [a, d, ...parts] = entry.split("\t");
              if (!parts.length) continue;
              const path = parts.join("\t"), old = stats[path] ?? { add: 0, del: 0 };
              stats[path] = { add: old.add + (Number(a) || 0), del: old.del + (Number(d) || 0), binary: a === "-" };
            }
          }
        }
        const files = parsed.files.map((f) => ({ ...f, ...(stats[f.path] ?? { add: 0, del: 0 }) }));
        if (this.viewKey() !== context) return;
        this.root = root;
        this.logicalRoot = logicalRoot;
        this.cwd = cwd;
        this.on = on;
        this.hasHead = hasHead;
        this.gitBranch = parsed.branch;
        this.gitStatus = parsed.status;
        this.gitLog = gitLog;
        this.stashCount = stashCount;
        this.files = files;
        this.limited = parsed.limited;
        this.error = null;
        this.watch(root, cx);
        // Reconcile any external list entries that actually belong to the current repository.
        if (Array.isArray(this.externalFiles) && this.externalFiles.length > 0) {
          this.externalFiles = this.externalFiles.filter((ext) => {
            const res = this.resolveInCurrentProject(ext, ext?.on || null, false);
            return !(res.sameMachine && res.inside);
          });
        }
        if (this.selectedExternal) {
          const selRes = this.resolveInCurrentProject(this.selectedExternal, this.selectedExternal.on || null, false);
          const promotedRel = selRes.sameMachine && selRes.inside && selRes.inRepo ? selRes.relPath : (this.selectedExternal.inProject ? this.selectedExternal.relPath : null);
          if (promotedRel && files.some((f) => f.path === promotedRel)) {
            this.selectedExternal = null;
            this.externalLoading = false;
            this.open = promotedRel;
            this.closed = false;
          }
        }
        if (!this.selectedCommit && !this.selectedExternal) {
          if (this.open && !files.some((f) => f.path === this.open)) { this.open = null; this.composer = null; this.anchorRowIdx = null; this.setLines([]); }
          // The first file's diff shows until you pick another.
          if (!this.open && !this.closed && files.length) this.open = files[0].path;
          if (this.open) {
            try { await this.load(this.open, context); }
            catch { if (this.viewKey() === context) this.setLines(["Cannot preview this diff (too large or unreadable)"]); }
          }
        }
      } catch (e) { if (this.viewKey() === context) this.error = String(e); }
      finally {
        this.loading = false;
        if (this.pendingOpenRequest && !this.busy) {
          const pending = this.pendingOpenRequest;
          this.pendingOpenRequest = null;
          this.applyOpenRequest(pending, cx);
        } else {
          cx.notify();
        }
      }
    });
  }

  async load(path, context) {
    const file = this.files.find((f) => f.path === path), root = this.root, on = this.on;
    if (!file) return;
    let lines;
    if (file.fresh || (!this.hasHead && file.status === "A")) {
      const text = await readTextFile(`${root}/${path}`, { on }).catch(() => null);
      if (text === null) lines = ["Cannot preview this file (binary, too large, or unreadable)"];
      else if (text.includes("\0")) lines = ["Binary file · no text diff"];
      else {
        file.add = countLines(text);
        lines = [`@@ -0,0 +1,${file.add} @@`, ...text.slice(0, 400000).replace(/\n$/, "").split("\n").slice(0, 1500).map((l) => `+${l}`)];
      }
    } else {
      const r = await exec("git", [...READ, "diff", ...(this.hasHead ? ["HEAD"] : []), "--no-ext-diff", "--no-textconv", "--no-color", "--", path], { cwd: root, on });
      if (r.code !== 0) throw new Error(r.stderr);
      const all = r.stdout.replace(/\n$/, "").split("\n"), first = all.findIndex((l) => l.startsWith("@@"));
      lines = first < 0 ? [file.binary ? "Binary file changed" : "No text diff available"] : all.slice(first);
    }
    if (this.viewKey() === context && this.open === path) {
      this.setLines(lines.slice(0, 1500).map((line) => line.length > 4000 ? line.slice(0, 4000) + "… (line truncated)" : line));
    }
  }

  async loadCommit(commit, context) {
    if (!commit?.hash || !this.root) return;
    const r = await exec(
      "git",
      [...READ, "show", "--no-ext-diff", "--no-textconv", "--no-color", "--stat", "-p", commit.hash],
      { cwd: this.root, on: this.on },
    );
    if (r.code !== 0) throw new Error(r.stderr || r.stdout);
    const lines = r.stdout ? r.stdout.replace(/\n$/, "").split("\n") : [tr("Empty commit", "空提交")];
    if (this.viewKey() === context && this.selectedCommit?.hash === commit.hash) {
      this.setLines(lines.slice(0, 1500).map((line) => (line.length > 4000 ? line.slice(0, 4000) + "… (line truncated)" : line)));
    }
  }

  async loadExternal(ext, context) {
    if (!ext?.path) return;
    const extOn = ext.on ?? this.on ?? null;
    const extPath = ext.path;
    let lines = null;

    // 1) If the file lives inside a Git repository, load its real Git diff/status
    //    and re-check whether it actually belongs to the current project (e.g. via symlink).
    const dir = parentDirectory(extPath);
    if (dir) {
      const top = await exec("git", ["rev-parse", "--show-toplevel", "--show-prefix"], { cwd: dir, on: extOn }).catch(() => ({ code: 1, stdout: "", stderr: "" }));
      if (top?.code === 0 && top.stdout?.trim()) {
        const [extRootLine = "", prefixLine = ""] = String(top.stdout).replace(/\r\n/g, "\n").split("\n");
        const extRoot = extRootLine.trim();
        const prefix = prefixLine.trim();
        const baseName = normalizePath(extPath).split("/").pop() || "";
        const rel = prefix ? `${prefix}${baseName}` : (resolvePathAgainstRoot(extPath, extRoot).relPath || baseName);

        if (this.root && (extOn || null) === (this.on || null) && pathsEqual(extRoot, this.root) && rel) {
          // Confirmed inside the current Git repository (e.g. traversed a symlink):
          // never keep it in the external list, and promote to in-repo view when dirty.
          this.externalFiles = (this.externalFiles ?? []).filter((f) => f.path !== extPath);
          ext.inProject = true;
          ext.relPath = rel;
          ext.extRoot = extRoot;
          if (this.files.some((f) => f.path === rel)) {
            if (this.viewKey() === context && this.selectedExternal?.path === extPath) {
              this.selectedExternal = null;
              this.externalLoading = false;
              this.open = rel;
              this.closed = false;
              await this.load(rel, context);
            }
            return;
          }
          if (!ext._refreshed) {
            ext._needsRefresh = true;
          }
        }

        if (rel) {
          const extGit = (args) => exec("git", [...READ, ...args], { cwd: extRoot, on: extOn });
          const stRes = await extGit(["status", "--porcelain=v2", "--branch", "-z", "--no-renames", "--untracked-files=all", "--", rel]).catch(() => ({ code: 1, stdout: "", stderr: "" }));
          const parsedSt = stRes?.code === 0 ? parseGitStatusV2(stRes.stdout, { limit: 10 }) : null;
          const hasHead = parsedSt ? parsedSt.branch.hasHead : true;
          const stFile = parsedSt?.files?.find((f) => f.path === rel) ?? null;
          if (stFile && (stFile.fresh || (!hasHead && stFile.status === "A"))) {
            const text = await readTextFile(extPath, { on: extOn }).catch(() => null);
            if (text !== null) {
              if (text.includes("\0")) {
                lines = [tr("Binary file · no text diff", "二进制文件 · 无文本差异")];
              } else {
                const n = countLines(text);
                ext.add = n;
                ext.del = 0;
                ext.status = "A";
                ext.sourceKind = "git-untracked";
                ext.extRoot = extRoot;
                lines = [`@@ -0,0 +1,${n} @@`, ...text.slice(0, 400000).replace(/\n$/, "").split("\n").slice(0, 1500).map((l) => `+${l}`)];
              }
            }
          } else {
            const r = await extGit(["diff", ...(hasHead ? ["HEAD"] : []), "--no-ext-diff", "--no-textconv", "--no-color", "--", rel]).catch(() => ({ code: 1, stdout: "", stderr: "" }));
            if (r?.code === 0 && r.stdout) {
              const all = r.stdout.replace(/\n$/, "").split("\n");
              const first = all.findIndex((l) => l.startsWith("@@"));
              if (first >= 0) {
                lines = all.slice(first);
                let addCount = 0;
                let delCount = 0;
                for (const l of lines) {
                  if (l.startsWith("+") && !l.startsWith("+++")) addCount += 1;
                  else if (l.startsWith("-") && !l.startsWith("---")) delCount += 1;
                }
                ext.add = addCount;
                ext.del = delCount;
                ext.status = stFile?.status || "M";
                ext.sourceKind = "git-diff";
                ext.extRoot = extRoot;
              }
            }
          }
        }
      }
    }

    const unreadableMsg = ext.inProject
      ? tr("Cannot preview this file (deleted, binary, too large, or unreadable)", "无法预览该文件（已删除、二进制、过大或不可读）")
      : tr("Cannot preview this external file (deleted, binary, too large, or unreadable)", "无法预览该项目外文件（已删除、二进制、过大或不可读）");

    // 2) For non-Git or already-committed files with real recorded edits, show the agent's recorded diff.
    if (!lines && ext.patch && !ext.isCreateOnly) {
      const patchLines = parseToolPatchLines(ext.patch);
      if (patchLines.length > 0) {
        lines = patchLines;
        ext.sourceKind = "agent-diff";
        if (!ext.status) ext.status = ext.del > 0 ? "M" : "A";
      }
    }

    // 3) Otherwise read the complete file on disk (preferred for newly created non-Git files so we don't truncate to tool preview),
    //    falling back to the recorded agent patch if the file was deleted/unreadable.
    if (!lines) {
      const text = await readTextFile(extPath, { on: extOn }).catch(() => null);
      if (text !== null) {
        if (text.includes("\0")) {
          lines = [tr("Binary file · no text diff", "二进制文件 · 无文本差异")];
          ext.sourceKind = "file-content";
        } else {
          const n = countLines(text);
          ext.add = n;
          ext.del = 0;
          ext.status = ext.status || "A";
          ext.sourceKind = "file-content";
          lines = [`@@ -0,0 +1,${n} @@`, ...text.slice(0, 400000).replace(/\n$/, "").split("\n").slice(0, 1500).map((l) => `+${l}`)];
        }
      } else if (ext.patch) {
        const patchLines = parseToolPatchLines(ext.patch);
        lines = patchLines.length > 0 ? patchLines : [unreadableMsg];
        ext.sourceKind = "agent-diff";
      } else {
        lines = [unreadableMsg];
      }
    }

    if (this.viewKey() === context && this.selectedExternal?.path === extPath) {
      this.setLines(lines.slice(0, 1500).map((line) => (line.length > 4000 ? line.slice(0, 4000) + "… (line truncated)" : line)));
    }
  }

  setLines(lines) {
    this.rows = diffRows(lines);
    const activeFile = this.selectedCommit ? null : (this.selectedExternal?.path ?? this.open);
    this.annotatedRows = annotateDiffRows(this.rows, activeFile);
    const maxNum = this.rows.reduce((m, row) => (typeof row.number === "number" && row.number > m ? row.number : m), 0);
    const numCols = Math.max(3, String(maxNum).length);
    this.diffNumWidth = Math.max(30, numCols * 8 + 6);
  }

  pick(path, cx) {
    this.selectedCommit = null;
    this.commitLoading = false;
    this.selectedExternal = null;
    this.externalLoading = false;
    this.open = this.open === path ? null : path;
    this.composer = null;
    this.anchorRowIdx = null;
    this.setLines([]);
    this.note = null;
    this.closed = !this.open;
    const context = this.context;
    if (this.open) cx.spawn(async (cx) => {
      try { await this.load(path, context); }
      catch (e) { if (this.viewKey() === context && this.open === path) this.note = `Cannot preview this diff (too large or unreadable): ${e}`; }
      cx.notify();
    });
    cx.notify();
  }

  pickExternal(ext, cx) {
    if (!ext?.path) return;
    this.selectedCommit = null;
    this.commitLoading = false;
    this.open = null;
    this.selectedExternal = ext;
    this.externalLoading = true;
    this.composer = null;
    this.anchorRowIdx = null;
    this.note = null;
    this.setLines([]);
    const context = this.context;
    cx.spawn(async (cx) => {
      try {
        await this.loadExternal(ext, context);
      } catch (e) {
        if (this.viewKey() === context && this.selectedExternal?.path === ext.path) {
          this.setLines([ext.inProject ? tr(`Cannot load diff: ${e}`, `无法加载变更：${e}`) : tr(`Cannot load external diff: ${e}`, `无法加载项目外变更：${e}`)]);
        }
      } finally {
        if (this.selectedExternal?.path === ext.path) this.externalLoading = false;
        if (ext._needsRefresh) {
          ext._needsRefresh = false;
          ext._refreshed = true;
          this.refresh(cx);
        }
        cx.notify();
      }
    });
    cx.notify();
  }

  closeExternalPreview(cx, clearList = false) {
    this.selectedExternal = null;
    this.externalLoading = false;
    if (clearList) this.externalFiles = [];
    this.setLines([]);
    this.note = null;
    const context = this.context;
    if (!this.closed && this.files.length) {
      this.open = this.files[0].path;
      cx.spawn(async (cx) => {
        try { await this.load(this.open, context); }
        catch (e) { if (this.viewKey() === context) this.note = `Cannot preview this diff (too large or unreadable): ${e}`; }
        cx.notify();
      });
    }
    cx.notify();
  }

  pickCommit(commit, cx) {
    if (!commit?.hash) return;
    if (this.selectedCommit?.hash === commit.hash) {
      this.closeCommitPreview(cx);
      return;
    }
    this.selectedCommit = commit;
    this.commitLoading = true;
    this.selectedExternal = null;
    this.externalLoading = false;
    this.open = null;
    this.composer = null;
    this.anchorRowIdx = null;
    this.note = null;
    this.setLines([]);
    const context = this.context;
    cx.spawn(async (cx) => {
      try {
        await this.loadCommit(commit, context);
      } catch (e) {
        if (this.viewKey() === context && this.selectedCommit?.hash === commit.hash) {
          this.setLines([tr(`Cannot load commit diff: ${e}`, `无法加载提交差异：${e}`)]);
        }
      } finally {
        if (this.selectedCommit?.hash === commit.hash) this.commitLoading = false;
        cx.notify();
      }
    });
    cx.notify();
  }

  closeCommitPreview(cx) {
    this.selectedCommit = null;
    this.commitLoading = false;
    this.setLines([]);
    this.note = null;
    const context = this.context;
    if (!this.closed && this.files.length) {
      this.open = this.files[0].path;
      cx.spawn(async (cx) => {
        try { await this.load(this.open, context); }
        catch (e) { if (this.viewKey() === context) this.note = `Cannot preview this diff (too large or unreadable): ${e}`; }
        cx.notify();
      });
    }
    cx.notify();
  }

  clickDiffRow(row, e, cx) {
    if (this.selectedCommit) return;
    if (!row?.commentable) return;
    const shift = Boolean(e?.modifiers?.shift || e?.shift);
    const anchor = shift && this.anchorRowIdx != null ? this.anchorRowIdx : row.rowIndex;
    const range = buildSelectionRange(this.annotatedRows, anchor, row.rowIndex);
    if (!range) return;

    if (!shift && this.composer && !this.composer.editingId && this.composer.startRowIdx === range.startRowIdx && this.composer.endRowIdx === range.endRowIdx && !String(this.commentInput.value() ?? "").trim()) {
      this.closeComposer(cx);
      return;
    }

    this.anchorRowIdx = shift && this.anchorRowIdx != null ? this.anchorRowIdx : row.rowIndex;
    const preserveText = Boolean(shift && this.composer);
    this.composer = {
      ...range,
      editingId: preserveText ? (this.composer?.editingId ?? null) : null,
    };
    if (!preserveText) this.commentInput.set_value("");
    this.reviewError = null;
    this.commentInput.focus();
    cx.notify();
  }

  editComment(comment, cx) {
    const idx = this.annotatedRows.findIndex(
      (r) => r.commentable && r.file === comment.path && ((comment.side === "old" ? r.oldLine : (r.newLine ?? r.oldLine)) === (comment.end_line || comment.start_line)),
    );
    const rowIdx = idx >= 0 ? idx : Math.max(0, this.annotatedRows.length - 1);
    this.anchorRowIdx = rowIdx;
    this.composer = {
      file: comment.path,
      startRowIdx: rowIdx,
      endRowIdx: rowIdx,
      side: comment.side || "new",
      startLine: comment.start_line,
      endLine: comment.end_line || comment.start_line,
      hunkHeader: comment.hunk_header || "",
      existingSnippet: comment.snippet || "",
      editingId: comment.id,
    };
    this.commentInput.set_value(comment.body || "");
    this.commentInput.focus();
    cx.notify();
  }

  deleteComment(id, cx) {
    const next = this.drafts().filter((c) => c.id !== id);
    this.setDrafts(next);
    if (this.composer?.editingId === id) {
      this.closeComposer(cx);
      return;
    }
    cx.notify();
  }

  closeComposer(cx) {
    this.composer = null;
    this.anchorRowIdx = null;
    this.commentTyping = false;
    this.commentInput.set_value("");
    this.handle.focus();
    cx.notify();
  }

  saveComposer(cx, sendImmediately = false) {
    if (!this.composer) return;
    const body = String(this.commentInput.value() ?? "").trim();
    if (!body) return;

    const c = this.composer;
    const snippet = c.editingId && c.existingSnippet
      ? c.existingSnippet
      : extractSnippet(this.annotatedRows, c.startRowIdx, c.endRowIdx);
    const item = {
      id: c.editingId || `rc-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
      path: c.file,
      side: c.side || "new",
      start_line: c.startLine,
      end_line: c.endLine > c.startLine ? c.endLine : undefined,
      hunk_header: c.hunkHeader || undefined,
      snippet: snippet || undefined,
      body,
    };

    const current = this.drafts();
    const next = c.editingId
      ? current.map((x) => (x.id === c.editingId ? item : x))
      : [...current, item];
    this.setDrafts(next);
    this.composer = null;
    this.anchorRowIdx = null;
    this.commentTyping = false;
    this.commentInput.set_value("");
    this.reviewError = null;

    if (sendImmediately) {
      this.sendReview(cx, next);
    } else {
      cx.notify();
    }
  }

  targetInfo() {
    const preferred = this.targetOverride || this.selectedExternal?.terminal || null;
    const { candidates, defaultTarget } = resolveTargetCandidates({
      panesList: panes(),
      agentsList: agents(),
      terminalsList: terminals(),
      machine: this.selectedExternal?.on ?? this.on ?? null,
      focusedInfo: focused(),
      repoRoot: this.selectedExternal?.extRoot || this.root,
      preferredAddr: preferred,
    });
    const chosen = candidates.find((c) => c.address === preferred) ?? candidates.find((c) => c.address === defaultTarget) ?? null;
    return { candidates, chosen };
  }

  sendReview(cx, explicitList = null) {
    if (this.sendingReqId) return;
    const list = explicitList ?? this.drafts();
    if (!list.length) return;
    const { chosen } = this.targetInfo();
    if (!chosen?.address) {
      this.reviewError = tr("No live agent or terminal on this machine", "当前机器暂无运行中的 Agent 或终端");
      cx.notify();
      return;
    }
    const reqId = `rev-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
    this.sendingReqId = reqId;
    this.sendingIds = list.map((c) => c.id);
    this.reviewError = null;
    this.targetPopupOpen = false;
    const comments = list.map((c) => ({
      path: c.path,
      side: c.side || "new",
      start_line: Number(c.start_line) || 1,
      ...(c.end_line && Number(c.end_line) > Number(c.start_line) ? { end_line: Number(c.end_line) } : {}),
      ...(c.hunk_header ? { hunk_header: c.hunk_header } : {}),
      ...(c.snippet ? { snippet: c.snippet } : {}),
      body: c.body,
    }));
    const repoRoot = this.selectedExternal ? (this.selectedExternal.extRoot || (this.selectedExternal.inProject ? this.root : null)) : this.root;
    run("agent.send_review", {
      request_id: reqId,
      target: chosen.address,
      source: "changes",
      ...(repoRoot ? { repo_root: repoRoot } : {}),
      comments,
      steer: Boolean(this.steerMode && chosen.kind === "acp" && chosen.status === "working"),
      focus: false,
    });
    cx.notify();
  }

  commit(cx) {
    if (!this.message.trim() || !this.root || this.busy || this.loading || this.context !== this.viewKey()) return;
    const root = this.root, on = this.on, context = this.context, message = this.message.trim();
    this.busy = true; this.note = null;
    cx.spawn(async (cx) => {
      try {
        for (const args of [["add", "-A"], ["commit", "-m", message]]) {
          const r = await exec("git", args, { cwd: root, on });
          if (r.code !== 0) throw new Error((r.stderr || r.stdout).trim());
        }
        if (this.viewKey() === context) { this.message = ""; this.aiCommitUsedAi = false; this.input.set_value(""); }
      } catch (e) { if (this.viewKey() === context) this.note = `Commit failed: ${e}`; }
      finally { this.busy = false; this.refresh(cx); cx.notify(); }
    });
    cx.notify();
  }

  suggestCommitMessage(cx) {
    if (this.aiCommitLoading || !this.root || !this.files.length || this.busy || typeof h2 !== "function") return;
    const root = this.root, on = this.on, context = this.context;
    this.aiCommitLoading = true;
    this.note = null;
    cx.notify();
    cx.spawn(async (acx) => {
      try {
        const args = ["ai", "commit-message", "--json", "--cwd", root, "--lang", isZh() ? "zh-CN" : "en"];
        if (on) args.push("--on", on);
        const res = await h2(args, { timeout: 15000 });
        if (this.viewKey() !== context) return;
        if (res?.code === 0 && res.stdout) {
          const parsed = JSON.parse(res.stdout);
          const msg = String(parsed?.message ?? "").trim();
          if (msg) {
            this.message = msg;
            this.aiCommitUsedAi = Boolean(parsed?.usedAi);
            this.input.set_value(msg);
            this.input.focus();
          }
        } else {
          const errText = String(res?.stderr || res?.stdout || "").trim();
          if (errText) this.note = tr(`AI commit message failed: ${errText}`, `AI 生成提交信息失败：${errText}`);
        }
      } catch (e) {
        if (this.viewKey() === context) this.note = tr(`AI commit message failed: ${e}`, `AI 生成提交信息失败：${e}`);
      } finally {
        this.aiCommitLoading = false;
        acx.notify();
      }
    });
  }

  diffMode() {
    try {
      return shared("diff.viewMode") === "split" ? "split" : "unified";
    } catch {
      return "unified";
    }
  }

  setDiffMode(mode, cx) {
    const next = mode === "split" ? "split" : "unified";
    if (this.diffMode() === next) return;
    setShared("diff.viewMode", next);
    cx.notify();
  }

  handleVirtualItemClick(itemKey, cx) {
    if (typeof itemKey !== "string") return;
    if (itemKey.startsWith("r-") || itemKey.startsWith("s-")) {
      const idx = Number(itemKey.slice(2));
      const row = Number.isFinite(idx) ? this.annotatedRows[idx] : null;
      if (row?.commentable) {
        this.clickDiffRow(row, { shift: Boolean(this.lastShift) }, cx);
      }
      return;
    }
    if (itemKey.startsWith("c-orphan-")) {
      const id = itemKey.slice("c-orphan-".length);
      const comment = this.drafts().find((c) => c.id === id);
      if (comment) this.editComment(comment, cx);
      return;
    }
    if (itemKey.startsWith("c-")) {
      const id = itemKey.slice(2);
      const comment = this.drafts().find((c) => c.id === id);
      if (comment) this.editComment(comment, cx);
    }
  }

  renderCodeRow(row, t, selectedRange, commentedSet) {
    if (row.kind === "@") {
      return div()
        .w_full()
        .min_w(0)
        .px(12)
        .py(2)
        .min_h(20)
        .line_height("16px")
        .whitespace_normal()
        .bg(t.raised)
        .text_color(t.dim)
        .child(row.text);
    }
    const inSel = Boolean(
      selectedRange && row.file === selectedRange.file && row.rowIndex >= selectedRange.startRowIdx && row.rowIndex <= selectedRange.endRowIdx,
    );
    const hasComment = commentedSet.has(row.rowIndex);
    const accent = t.accent ?? "#3b82f6";
    const rowBg = inSel
      ? (withAlpha(accent, "24") ?? t.selected)
      : row.kind === "+"
        ? (t.ansi2 + "18")
        : row.kind === "-"
          ? (t.ansi1 + "18")
          : null;

    return h_flex()
      .w_full()
      .min_w(0)
      .min_h(20)
      .pr(14)
      .gap(6)
      .items_start()
      .when(Boolean(rowBg), (e) => e.bg(rowBg))
      .when(row.commentable, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha(accent, "18") ?? t.hover)))
      .child(
        div()
          .w(3)
          .self_stretch()
          .flex_none()
          .when(inSel || hasComment, (e) => e.bg(accent)),
      )
      .child(
        div()
          .w(14)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_center()
          .text_size(11)
          .text_color(inSel || hasComment ? accent : t.dim)
          .child(row.commentable ? (hasComment ? "●" : "+") : ""),
      )
      .child(
        div()
          .w(this.diffNumWidth ?? 30)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_right()
          .text_color(inSel ? t.foreground : t.dim)
          .child(row.number === undefined ? "" : String(row.number)),
      )
      .child(div().flex_1().min_w(0).py(1).line_height("18px").whitespace_normal().child(row.text));
  }

  renderSplitCell(row, side, t, selectedRange, commentedSet, padRight) {
    const accent = t.accent ?? "#3b82f6";
    if (!row) {
      return h_flex()
        .flex_1()
        .min_w(0)
        .min_h(20)
        .items_start()
        .bg(withAlpha(t.raised, "99") ?? t.surface ?? t.background)
        .child(div().w(3).self_stretch().flex_none())
        .child(div().w(this.diffNumWidth ?? 30).flex_none().py(1).line_height("18px").child(""))
        .child(div().w(12).flex_none().py(1).line_height("18px").child(""))
        .child(div().flex_1().min_w(0).py(1).pr(padRight).line_height("18px").child(" "));
    }

    const inSel = Boolean(
      selectedRange && row.file === selectedRange.file && row.rowIndex >= selectedRange.startRowIdx && row.rowIndex <= selectedRange.endRowIdx,
    );
    const hasComment = commentedSet.has(row.rowIndex);
    const isDel = side === "left" && row.kind === "-";
    const isAdd = side === "right" && row.kind === "+";
    const cellBg = inSel
      ? (withAlpha(accent, "24") ?? t.selected)
      : isAdd
        ? (t.ansi2 + "18")
        : isDel
          ? (t.ansi1 + "18")
          : null;
    const lineNum = side === "left"
      ? (row.oldLine ?? (row.kind === "-" ? row.number : null))
      : (row.newLine ?? (row.kind === "+" ? row.number : null));
    const sign = isDel ? "−" : isAdd ? "+" : "";
    const signColor = isDel ? t.ansi1 : isAdd ? t.ansi2 : t.dim;

    return h_flex()
      .flex_1()
      .min_w(0)
      .min_h(20)
      .gap(4)
      .items_start()
      .when(Boolean(cellBg), (e) => e.bg(cellBg))
      .child(
        div()
          .w(3)
          .self_stretch()
          .flex_none()
          .when(inSel || hasComment, (e) => e.bg(accent)),
      )
      .child(
        div()
          .w(this.diffNumWidth ?? 30)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_right()
          .text_color(inSel ? t.foreground : t.dim)
          .child(lineNum == null ? "" : String(lineNum)),
      )
      .child(
        div()
          .w(12)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_center()
          .text_color(hasComment ? accent : signColor)
          .child(hasComment ? "●" : sign),
      )
      .child(
        div()
          .flex_1()
          .min_w(0)
          .py(1)
          .pr(padRight)
          .line_height("18px")
          .whitespace_normal()
          .child(row.text),
      );
  }

  renderSplitRow(srow, t, selectedRange, commentedSet) {
    if (srow.kind === "hunk") {
      return div()
        .w_full()
        .min_w(0)
        .px(12)
        .py(2)
        .min_h(20)
        .line_height("16px")
        .whitespace_normal()
        .bg(t.raised)
        .text_color(t.dim)
        .child(srow.row.text);
    }
    if (srow.kind === "file") {
      return h_flex()
        .w_full()
        .min_w(0)
        .px(12)
        .py(4)
        .min_h(24)
        .items_center()
        .bg(t.raised)
        .border_y(1)
        .border_color(t.border)
        .child(div().flex_1().min_w(0).font_semibold().text_size(11).line_height("16px").text_color(t.foreground).whitespace_normal().child(srow.row.text));
    }
    if (srow.kind === "meta") {
      return div()
        .w_full()
        .min_w(0)
        .px(12)
        .py(1)
        .min_h(20)
        .line_height("18px")
        .whitespace_normal()
        .text_color(t.muted)
        .child(srow.row.text);
    }

    const accent = t.accent ?? "#3b82f6";
    const commentable = Boolean(srow.right?.commentable || srow.left?.commentable);
    return h_flex()
      .w_full()
      .min_w(0)
      .items_stretch()
      .when(commentable, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha(accent, "12") ?? t.hover)))
      .child(this.renderSplitCell(srow.left, "left", t, selectedRange, commentedSet, 6))
      .child(div().w(1).self_stretch().flex_none().bg(t.border))
      .child(this.renderSplitCell(srow.right, "right", t, selectedRange, commentedSet, 14));
  }

  renderDiffModeToggle(t, mode) {
    const pill = (id, label, active, nextMode) =>
      div()
        .id(id)
        .flex_none()
        .px(7)
        .py(2)
        .rounded(4)
        .text_size(10.5)
        .font_medium()
        .cursor_pointer()
        .when(active, (e) => e.bg(t.selected ?? t.raised))
        .text_color(active ? t.foreground : t.muted)
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click((_e, cx) => this.setDiffMode(nextMode, cx))
        .child(label);

    return h_flex()
      .id("chg-diff-mode-toggle")
      .flex_none()
      .p(2)
      .gap(2)
      .items_center()
      .rounded(6)
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .child(pill("chg-diff-mode-unified", tr("Unified", "直接"), mode === "unified", "unified"))
      .child(pill("chg-diff-mode-split", tr("Split", "分栏"), mode === "split", "split"));
  }

  renderSavedComment(item, t) {
    const { comment, outdated, editing } = item;
    const accent = t.accent ?? "#3b82f6";
    return div()
      .w_full()
      .min_w(0)
      .px(10)
      .py(4)
      .flex_none()
      .cursor_pointer()
      .child(
        v_flex()
          .w_full()
          .min_w(0)
          .max_w(580)
          .p(8)
          .gap(4)
          .rounded(8)
          .bg(editing ? (withAlpha(accent, "1c") ?? t.selected) : t.raised)
          .border_1()
          .border_color(editing ? accent : outdated ? t.blocked : (withAlpha(accent, "66") ?? t.border))
          .font_family("sans-serif")
          .child(
            h_flex()
              .w_full()
              .items_center()
              .gap(6)
              .child(
                div()
                  .px(5)
                  .py(1)
                  .rounded(4)
                  .bg(withAlpha(accent, "1f") ?? t.background)
                  .text_size(10)
                  .font_semibold()
                  .text_color(accent)
                  .child(formatLineTag(comment)),
              )
              .when(outdated, (e) =>
                e.child(
                  div()
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .bg(withAlpha(t.blocked, "22") ?? t.background)
                    .text_size(10)
                    .text_color(t.blocked)
                    .child(tr("Line moved", "行已变动")),
                ),
              )
              .child(div().flex_1())
              .child(
                div()
                  .text_size(10)
                  .text_color(editing ? accent : t.dim)
                  .child(editing ? tr("Editing below…", "正在下方编辑…") : tr("Click to edit / delete", "点击编辑或删除")),
              ),
          )
          .child(
            div()
              .w_full()
              .text_size(12)
              .line_height("16px")
              .text_color(t.foreground)
              .whitespace_normal()
              .child(comment.body),
          ),
      );
  }

  renderDockedComposer(composer, t) {
    const accent = t.accent ?? "#3b82f6";
    const hasText = Boolean(String(this.commentInput.value() ?? "").trim());
    const rangeTag = formatLineTag({
      start_line: composer.startLine,
      end_line: composer.endLine,
      side: composer.side,
    });

    return v_flex()
      .id("chg-inline-composer")
      .flex_none()
      .w_full()
      .p(10)
      .gap(6)
      .bg(t.popover ?? t.raised)
      .border_t(1)
      .border_color(this.commentTyping ? accent : t.border)
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .child(
        h_flex()
          .w_full()
          .items_center()
          .gap(6)
          .child(
            div()
              .px(5)
              .py(1)
              .rounded(4)
              .bg(withAlpha(accent, "22") ?? t.raised)
              .text_size(10)
              .font_semibold()
              .text_color(accent)
              .child(`${composer.file} · ${rangeTag}`),
          )
          .child(div().flex_1())
          .child(
            div()
              .text_size(10)
              .text_color(t.dim)
              .child(tr("Shift+Click a diff line to extend range", "Shift+点击差异行可扩展行区间")),
          ),
      )
      .child(
        div()
          .w_full()
          .h(54)
          .px(8)
          .py(4)
          .rounded(6)
          .bg(t.background)
          .border_1()
          .border_color(this.commentTyping ? accent : t.border)
          .cursor_text()
          .child(Textarea.new(this.commentInput).size_full().text_size(12).text_color(t.foreground)),
      )
      .child(
        h_flex()
          .w_full()
          .items_center()
          .gap(6)
          .when(Boolean(composer.editingId), (e) =>
            e.child(
              div()
                .id("chg-comp-delete")
                .px(8)
                .py(3)
                .rounded(5)
                .text_size(11)
                .text_color(t.ansi1)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_click((_e, cx) => this.deleteComment(composer.editingId, cx))
                .child(tr("Delete", "删除")),
            ),
          )
          .child(div().flex_1())
          .child(
            div()
              .id("chg-comp-cancel")
              .px(8)
              .py(3)
              .rounded(5)
              .text_size(11)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click((_e, cx) => this.closeComposer(cx))
              .child(tr("Cancel", "取消")),
          )
          .child(
            div()
              .id("chg-comp-save")
              .px(9)
              .py(3)
              .rounded(5)
              .border_1()
              .border_color(hasText ? accent : t.border)
              .bg(hasText ? (withAlpha(accent, "1f") ?? t.raised) : t.raised)
              .text_size(11)
              .text_color(hasText ? t.foreground : t.dim)
              .when(hasText, (e) => e.cursor_pointer().hover((s) => s.bg(t.hover)).on_click((_e, cx) => this.saveComposer(cx, false)))
              .child(composer.editingId ? tr("Update", "更新") : tr("Add", "添加")),
          )
          .child(
            div()
              .id("chg-comp-send")
              .px(9)
              .py(3)
              .rounded(5)
              .bg(hasText && !this.sendingReqId ? t.solid : t.raised)
              .text_size(11)
              .text_color(hasText && !this.sendingReqId ? t.solid_foreground : t.dim)
              .when(hasText && !this.sendingReqId, (e) => e.cursor_pointer().on_click((_e, cx) => this.saveComposer(cx, true)))
              .child(tr("Send now", "立即发送")),
          ),
      );
  }

  renderReviewBar(t, allDrafts) {
    const { candidates, chosen } = this.targetInfo();
    const fileSet = new Set(allDrafts.map((c) => c.path));
    const accent = t.accent ?? "#3b82f6";
    const isWorkingAcp = Boolean(chosen && chosen.kind === "acp" && chosen.status === "working");
    const canSend = Boolean(allDrafts.length > 0 && chosen && !this.sendingReqId);
    const statusColor = (st) => (st === "working" ? t.working : st === "blocked" ? t.blocked : st === "done" ? t.done : t.dim);

    const trigger = h_flex()
      .id("chg-review-target-btn")
      .h(26)
      .max_w(210)
      .px(8)
      .gap(5)
      .items_center()
      .rounded(6)
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        this.targetPopupOpen = !this.targetPopupOpen;
        cx.notify();
      })
      .when(Boolean(chosen), (e) => e.child(div().w(6).h(6).flex_none().rounded(3).bg(statusColor(chosen.status))))
      .child(
        div()
          .min_w(0)
          .truncate()
          .text_size(11)
          .text_color(chosen ? t.foreground : t.dim)
          .child(chosen ? chosen.label : tr("No live agent", "无存活 Agent")),
      )
      .child(div().flex_none().text_size(9).text_color(t.dim).child("▾"));

    return v_flex()
      .id("chg-review-bar")
      .flex_none()
      .px(10)
      .py(8)
      .gap(6)
      .border_t(1)
      .border_color(withAlpha(accent, "55") ?? t.border)
      .bg(t.raised)
      .when(Boolean(this.reviewError), (e) =>
        e.child(div().text_size(11).text_color(t.ansi1).whitespace_normal().child(this.reviewError)),
      )
      .child(
        h_flex()
          .w_full()
          .gap(6)
          .items_center()
          .flex_wrap()
          .child(
            div()
              .px(6)
              .py(2)
              .rounded(5)
              .bg(withAlpha(accent, "20") ?? t.background)
              .text_size(11)
              .font_semibold()
              .text_color(accent)
              .child(
                tr(
                  `${allDrafts.length} note${allDrafts.length === 1 ? "" : "s"} · ${fileSet.size} file${fileSet.size === 1 ? "" : "s"}`,
                  `${allDrafts.length} 条批注 · ${fileSet.size} 个文件`,
                ),
              ),
          )
          .child(
            div()
              .id("chg-review-clear")
              .px(6)
              .py(2)
              .rounded(5)
              .text_size(11)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click((_e, cx) => {
                this.setDrafts([]);
                this.reviewError = null;
                cx.notify();
              })
              .child(tr("Clear", "清空")),
          )
          .child(div().flex_1())
          .child(
            Popup.new("chg-review-target-popup", trigger)
              .anchor("top_left")
              .when(this.targetPopupOpen, (el) =>
                el.content(
                  v_flex()
                    .w(240)
                    .max_h(220)
                    .p(4)
                    .gap(2)
                    .rounded(8)
                    .bg(t.popover ?? t.background)
                    .border_1()
                    .border_color(t.border)
                    .overflow_y_scroll()
                    .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                    .children(
                      candidates.length
                        ? candidates.map((c) => {
                            const active = chosen?.address === c.address;
                            return h_flex()
                              .id(`chg-target-${c.address}`)
                              .h(26)
                              .px(8)
                              .gap(6)
                              .items_center()
                              .rounded(6)
                              .cursor_pointer()
                              .when(active, (x) => x.bg(t.selected))
                              .hover((s) => s.bg(t.hover))
                              .on_click((_e, cx) => {
                                this.targetOverride = c.address;
                                this.targetPopupOpen = false;
                                cx.notify();
                              })
                              .child(div().w(6).h(6).flex_none().rounded(3).bg(statusColor(c.status)))
                              .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(t.foreground).child(c.label))
                              .child(
                                div()
                                  .flex_none()
                                  .px(4)
                                  .py(1)
                                  .rounded(4)
                                  .bg(t.raised)
                                  .text_size(9.5)
                                  .text_color(t.dim)
                                  .child(c.kind === "acp" ? "ACP" : "CLI"),
                              );
                          })
                        : [div().p(8).text_size(11).text_color(t.dim).child(tr("No live sessions", "暂无存活会话"))],
                    ),
                ),
              ),
          )
          .when(isWorkingAcp, (e) =>
            e.child(
              div()
                .id("chg-review-steer")
                .h(26)
                .px(7)
                .flex()
                .items_center()
                .rounded(6)
                .border_1()
                .border_color(this.steerMode ? t.blocked : t.border)
                .bg(this.steerMode ? (withAlpha(t.blocked, "22") ?? t.background) : t.background)
                .text_size(10.5)
                .text_color(this.steerMode ? t.blocked : t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_click((_e, cx) => {
                  this.steerMode = !this.steerMode;
                  cx.notify();
                })
                .child(this.steerMode ? tr("Steer", "抢占") : tr("Queue", "排队")),
            ),
          )
          .child(
            div()
              .id("chg-review-send")
              .h(26)
              .px(11)
              .flex()
              .items_center()
              .rounded(6)
              .bg(canSend ? t.solid : t.background)
              .text_size(11.5)
              .font_medium()
              .text_color(canSend ? t.solid_foreground : t.dim)
              .when(canSend, (e) => e.cursor_pointer().on_click((_e, cx) => this.sendReview(cx)))
              .child(this.sendingReqId ? tr("Sending…", "发送中…") : tr("Send to Agent", "发给 Agent")),
          ),
      );
  }

  renderGitInfo(t, cx) {
    const branch = this.gitBranch;
    const st = this.gitStatus;
    if (!this.root || (!branch && !st)) return null;
    const accent = t.accent ?? "#3b82f6";
    const mono = settings().font_family ?? "JetBrains Mono";
    const branchLabel = branch
      ? branch.detached
        ? tr("Detached HEAD", "游离 HEAD")
        : branch.name || tr("HEAD", "HEAD")
      : tr("Repository", "仓库");
    const branchTooltip = [
      branch?.detached
        ? tr(`Detached HEAD at ${branch.headShort || "unknown"}`, `游离 HEAD：${branch.headShort || "未知"}`)
        : tr(`Branch: ${branchLabel}`, `当前分支：${branchLabel}`),
      branch?.headOid ? `HEAD: ${branch.headOid}` : tr("No commits yet", "尚无提交"),
      branch?.upstream
        ? tr(
            `Upstream: ${branch.upstream}${branch.upstreamGone ? " (gone)" : ""}`,
            `上游分支：${branch.upstream}${branch.upstreamGone ? "（已删除）" : ""}`,
          )
        : tr("No upstream tracking branch", "未关联上游分支"),
      branch?.ahead > 0 ? tr(`Ahead by ${branch.ahead} commit(s)`, `领先上游 ${branch.ahead} 个提交`) : "",
      branch?.behind > 0 ? tr(`Behind by ${branch.behind} commit(s)`, `落后上游 ${branch.behind} 个提交`) : "",
      this.stashCount > 0 ? tr(`Stashes: ${this.stashCount}`, `贮藏 (stash)：${this.stashCount} 项`) : "",
    ]
      .filter(Boolean)
      .join("\n");

    const statusPill = (label, color, bgAlpha = "1e") =>
      div()
        .px(5)
        .py(1)
        .rounded(4)
        .bg(withAlpha(color, bgAlpha) ?? t.background)
        .text_size(10)
        .font_medium()
        .text_color(color)
        .child(label);

    return v_flex()
      .id("chg-git-info")
      .flex_none()
      .px(12)
      .py(7)
      .gap(5)
      .bg(t.raised)
      .border_b(1)
      .border_color(t.border)
      .child(
        h_flex()
          .w_full()
          .gap(6)
          .items_center()
          .tooltip(branchTooltip)
          .child(svg("mods/workspace-chrome/git-branch.svg").size(12).flex_none().text_color(branch?.detached ? t.blocked : accent))
          .child(
            div()
              .min_w(0)
              .max_w(160)
              .truncate()
              .text_size(11.5)
              .font_semibold()
              .text_color(branch?.detached ? t.blocked : t.foreground)
              .child(branchLabel),
          )
          .when(Boolean(branch?.headShort), (e) =>
            e.child(
              div()
                .flex_none()
                .px(4)
                .py(1)
                .rounded(4)
                .bg(t.background)
                .border_1()
                .border_color(t.border)
                .font_family(mono)
                .text_size(10)
                .text_color(t.muted)
                .child(branch.headShort),
            ),
          )
          .when(Boolean(branch && !branch.hasHead), (e) =>
            e.child(statusPill(tr("unborn", "无提交"), t.dim, "20")),
          )
          .when(Boolean(branch?.upstream), (e) =>
            e.child(
              div()
                .min_w(0)
                .flex_1()
                .truncate()
                .text_size(10.5)
                .text_color(branch.upstreamGone ? t.ansi1 : t.dim)
                .child(`→ ${branch.upstream}${branch.upstreamGone ? tr(" (gone)", " (已失联)") : ""}`),
            ),
          )
          .when(!branch?.upstream, (e) => e.child(div().flex_1()))
          .when(Boolean(branch?.ahead > 0), (e) => e.child(statusPill(`↑${branch.ahead}`, t.ansi2)))
          .when(Boolean(branch?.behind > 0), (e) => e.child(statusPill(`↓${branch.behind}`, t.blocked)))
          .when(this.stashCount > 0, (e) =>
            e.child(statusPill(tr(`stash ${this.stashCount}`, `贮藏 ${this.stashCount}`), t.muted, "22")),
          )
          .child(
            div()
              .id("chg-git-refresh")
              .flex_none()
              .px(5)
              .py(1)
              .rounded(4)
              .text_size(11)
              .text_color(this.loading ? t.dim : t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .tooltip(tr("Refresh Git status & log", "刷新 Git 状态与提交记录"))
              .on_click((_e, clickCx) => this.refresh(clickCx))
              .child("↻"),
          ),
      )
      .when(Boolean(st), (el) =>
        el.child(
          h_flex()
            .id("chg-git-status-bar")
            .w_full()
            .gap(5)
            .items_center()
            .flex_wrap()
            .child(
              div()
                .flex_none()
                .font_family(mono)
                .text_size(10)
                .text_color(t.dim)
                .child("status"),
            )
            .when(st.clean, (e) =>
              e.child(
                h_flex()
                  .px(6)
                  .py(1)
                  .gap(4)
                  .items_center()
                  .rounded(4)
                  .bg(withAlpha(t.ansi2, "1c") ?? t.background)
                  .text_size(10)
                  .font_medium()
                  .text_color(t.ansi2)
                  .child(svg("mods/workspace-chrome/check.svg").size(10).flex_none().text_color(t.ansi2))
                  .child(tr("Clean working tree", "工作区干净")),
              ),
            )
            .when(!st.clean && st.conflicts > 0, (e) =>
              e.child(statusPill(tr(`${st.conflicts} conflict${st.conflicts === 1 ? "" : "s"}`, `冲突 ${st.conflicts}`), t.ansi1, "24")),
            )
            .when(!st.clean && st.staged > 0, (e) =>
              e.child(statusPill(tr(`${st.staged} staged`, `已暂存 ${st.staged}`), t.ansi2)),
            )
            .when(!st.clean && st.unstaged > 0, (e) =>
              e.child(statusPill(tr(`${st.unstaged} modified`, `未暂存 ${st.unstaged}`), t.blocked)),
            )
            .when(!st.clean && st.untracked > 0, (e) =>
              e.child(statusPill(tr(`${st.untracked} untracked`, `未跟踪 ${st.untracked}`), accent)),
            ),
        ),
      );
  }

  renderCommitBanner(t, diffMode) {
    const c = this.selectedCommit;
    if (!c) return null;
    const accent = t.accent ?? "#3b82f6";
    const mono = settings().font_family ?? "JetBrains Mono";
    return h_flex()
      .id("chg-commit-banner")
      .flex_none()
      .w_full()
      .min_w(0)
      .px(10)
      .py(6)
      .gap(6)
      .items_center()
      .bg(withAlpha(accent, "14") ?? t.raised)
      .border_b(1)
      .border_color(withAlpha(accent, "44") ?? t.border)
      .child(
        div()
          .flex_none()
          .px(5)
          .py(1)
          .rounded(4)
          .bg(withAlpha(accent, "24") ?? t.background)
          .font_family(mono)
          .text_size(10)
          .font_semibold()
          .text_color(accent)
          .child(c.shortHash),
      )
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(11.5)
          .font_medium()
          .text_color(t.foreground)
          .child(c.subject || tr("(no subject)", "(无提交说明)")),
      )
      .when(Boolean(c.author || c.relTime), (e) =>
        e.child(
          div()
            .flex_none()
            .max_w(120)
            .truncate()
            .text_size(10)
            .text_color(t.dim)
            .child([c.author, c.relTime].filter(Boolean).join(" · ")),
        ),
      )
      .child(this.renderDiffModeToggle(t, diffMode))
      .child(
        div()
          .id("chg-commit-close")
          .flex_none()
          .px(6)
          .py(2)
          .rounded(4)
          .bg(t.background)
          .border_1()
          .border_color(t.border)
          .text_size(10)
          .text_color(t.muted)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .on_click((_e, cx) => this.closeCommitPreview(cx))
          .child(tr("Back to changes", "返回变更")),
      );
  }

  renderExternalBanner(t, diffMode) {
    const ext = this.selectedExternal;
    if (!ext) return null;
    const isInProject = Boolean(ext.inProject);
    const badgeColor = isInProject ? (t.accent ?? "#3b82f6") : (t.blocked ?? "#f59e0b");
    const displayTitle = isInProject ? (ext.relPath || ext.displayPath || ext.path) : ext.path;
    const sourceLabel = ext.sourceKind === "git-diff"
      ? tr("Git diff", "Git 差异")
      : ext.sourceKind === "git-untracked"
        ? tr("Untracked", "未跟踪文件")
        : ext.sourceKind === "agent-diff"
          ? tr("Agent diff", "Agent 变更")
          : ext.sourceKind === "file-content"
            ? tr("File", "文件内容")
            : null;
    return h_flex()
      .id("chg-ext-banner")
      .flex_none()
      .w_full()
      .min_w(0)
      .px(10)
      .py(6)
      .gap(6)
      .items_center()
      .bg(withAlpha(badgeColor, "14") ?? t.raised)
      .border_b(1)
      .border_color(withAlpha(badgeColor, "44") ?? t.border)
      .child(
        div()
          .flex_none()
          .px(5)
          .py(1)
          .rounded(4)
          .bg(withAlpha(badgeColor, "24") ?? t.background)
          .text_size(10)
          .font_semibold()
          .text_color(badgeColor)
          .child(isInProject ? tr("In project", "项目内") : tr("External", "项目外")),
      )
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(11.5)
          .font_medium()
          .text_color(t.foreground)
          .tooltip(ext.path)
          .child(displayTitle),
      )
      .when(Boolean(sourceLabel), (e) =>
        e.child(
          div()
            .flex_none()
            .px(4)
            .py(1)
            .rounded(4)
            .bg(t.background)
            .border_1()
            .border_color(t.border)
            .text_size(9.5)
            .text_color(t.muted)
            .child(sourceLabel),
        ),
      )
      .when(ext.add > 0, (e) => e.child(div().flex_none().text_size(10.5).text_color(t.ansi2).child(`+${ext.add}`)))
      .when(ext.del > 0, (e) => e.child(div().flex_none().text_size(10.5).text_color(t.ansi1).child(`−${ext.del}`)))
      .child(
        div()
          .id("chg-ext-open-file")
          .flex_none()
          .px(6)
          .py(2)
          .rounded(4)
          .bg(t.background)
          .border_1()
          .border_color(t.border)
          .text_size(10)
          .text_color(t.muted)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .tooltip(tr("Open file in Files panel", "在文件面板中打开"))
          .on_click(() => {
            const args = { path: ext.path };
            if (ext.on) args.on = ext.on;
            run("files.open", args);
          })
          .child(tr("Open", "打开")),
      )
      .child(this.renderDiffModeToggle(t, diffMode))
      .child(
        div()
          .id("chg-ext-close")
          .flex_none()
          .px(6)
          .py(2)
          .rounded(4)
          .bg(t.background)
          .border_1()
          .border_color(t.border)
          .text_size(10)
          .text_color(t.muted)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .on_click((_e, cx) => this.closeExternalPreview(cx))
          .child(tr("Back", "返回")),
      );
  }

  renderGitLog(t) {
    if (!this.root) return null;
    const accent = t.accent ?? "#3b82f6";
    const mono = settings().font_family ?? "JetBrains Mono";
    const commits = this.gitLog ?? [];
    const latest = commits[0];
    const bodyHeight = commits.length ? Math.min(136, commits.length * 26 + 8) : 32;

    return v_flex()
      .id("chg-git-log")
      .flex_none()
      .w_full()
      .border_t(1)
      .border_color(t.border)
      .child(
        h_flex()
          .id("chg-git-log-toggle")
          .h(28)
          .flex_none()
          .px(12)
          .gap(6)
          .items_center()
          .bg(t.raised)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_click((_e, cx) => {
            this.gitLogOpen = !this.gitLogOpen;
            cx.notify();
          })
          .child(svg(`mods/workspace-chrome/chevron-${this.gitLogOpen ? "down" : "right"}.svg`).size(11).flex_none().text_color(t.dim))
          .child(svg("mods/workspace-chrome/clock.svg").size(11).flex_none().text_color(t.muted))
          .child(div().flex_none().text_size(11).font_medium().text_color(t.foreground).child(tr("Recent git log", "最近提交")))
          .child(
            div()
              .flex_none()
              .px(4)
              .py(1)
              .rounded(4)
              .bg(t.background)
              .text_size(9.5)
              .text_color(t.dim)
              .child(String(commits.length)),
          )
          .when(!this.gitLogOpen && Boolean(latest), (e) =>
            e
              .child(
                div()
                  .flex_none()
                  .font_family(mono)
                  .text_size(10)
                  .text_color(accent)
                  .child(latest.shortHash),
              )
              .child(
                div()
                  .flex_1()
                  .min_w(0)
                  .truncate()
                  .text_size(10.5)
                  .text_color(t.dim)
                  .child(latest.subject),
              )
              .child(
                div()
                  .flex_none()
                  .text_size(10)
                  .text_color(t.dim)
                  .child(tr("Expand", "展开查看")),
              ),
          )
          .when(this.gitLogOpen || !latest, (e) =>
            e
              .child(div().flex_1())
              .when(commits.length > 0, (x) =>
                x.child(div().flex_none().text_size(10).text_color(t.dim).child(tr("Click to inspect diff", "点击查看变更"))),
              ),
          ),
      )
      .when(this.gitLogOpen, (el) =>
        el.child(
          div()
            .w_full()
            .h(bodyHeight)
            .relative()
            .border_t(1)
            .border_color(t.border)
            .child(
              v_flex()
                .id("chg-git-log-list")
                .absolute()
                .inset_0()
                .overflow_y_scroll()
                .p(4)
                .gap(2)
                .when(!commits.length, (e) =>
                  e.child(div().px(8).py(4).text_size(11).text_color(t.dim).child(tr("No commits yet", "尚无提交记录"))),
                )
                .children(
                  commits.map((c) => {
                    const active = this.selectedCommit?.hash === c.hash;
                    const primaryRef = formatPrimaryRef(c.refs);
                    const tip = [
                      `${c.shortHash}  ${c.subject}`,
                      c.refs?.length ? `Refs: ${c.refs.join(", ")}` : "",
                      [c.author, c.relTime].filter(Boolean).join(" · "),
                      tr("Click to inspect commit diff", "点击查看本次提交的差异"),
                    ]
                      .filter(Boolean)
                      .join("\n");
                    return h_flex()
                      .id(`chg-commit-${c.shortHash}`)
                      .h(24)
                      .flex_none()
                      .px(8)
                      .gap(6)
                      .items_center()
                      .rounded(6)
                      .cursor_pointer()
                      .when(active, (e) => e.bg(t.selected))
                      .hover((s) => s.bg(t.hover))
                      .tooltip(tip)
                      .on_click((_e, cx) => this.pickCommit(c, cx))
                      .child(
                        div()
                          .flex_none()
                          .font_family(mono)
                          .text_size(10.5)
                          .text_color(active ? t.foreground : accent)
                          .child(c.shortHash),
                      )
                      .when(Boolean(primaryRef), (e) =>
                        e.child(
                          div()
                            .flex_none()
                            .max_w(90)
                            .truncate()
                            .px(4)
                            .py(1)
                            .rounded(4)
                            .bg(withAlpha(accent, "1c") ?? t.raised)
                            .text_size(9.5)
                            .font_medium()
                            .text_color(accent)
                            .child(primaryRef),
                        ),
                      )
                      .child(
                        div()
                          .flex_1()
                          .min_w(0)
                          .truncate()
                          .text_size(11.5)
                          .text_color(t.foreground)
                          .child(c.subject || tr("(no subject)", "(无提交说明)")),
                      )
                      .when(Boolean(c.author), (e) =>
                        e.child(
                          div()
                            .flex_none()
                            .max_w(72)
                            .truncate()
                            .text_size(10)
                            .text_color(t.dim)
                            .child(c.author),
                        ),
                      )
                      .when(Boolean(c.relTime), (e) =>
                        e.child(
                          div()
                            .flex_none()
                            .text_size(10)
                            .text_color(t.dim)
                            .child(c.relTime),
                        ),
                      );
                  }),
                ),
            )
            .when(commits.length > 5, (e) => e.child(Scrollbar.vertical("chg-git-log-list").absolute().inset_0())),
        ),
      );
  }

  render(cx) {
    if (this.viewKey() !== this.context) this.refresh(cx);
    this.consumeOpenRequest(cx);
    this.syncLastResult();
    const t = theme(), add = this.files.reduce((n, f) => n + f.add, 0), del = this.files.reduce((n, f) => n + f.del, 0);
    const enabled = Boolean(this.message.trim() && this.root && !this.busy && !this.loading);

    const allDrafts = this.drafts();
    const countByFile = new Map();
    for (const d of allDrafts) {
      countByFile.set(d.path, (countByFile.get(d.path) ?? 0) + 1);
    }
    const diffMode = this.diffMode();
    const activePath = !this.selectedCommit ? (this.selectedExternal?.path ?? this.open) : null;
    const openComments = activePath ? allDrafts.filter((c) => c.path === activePath) : [];
    const activeComposer = activePath && this.composer && this.composer.file === activePath ? this.composer : null;
    const { items } = diffMode === "split"
      ? buildSplitDisplayItems(this.annotatedRows, openComments, activeComposer)
      : buildDisplayItems(this.annotatedRows, openComments, activeComposer);
    const commentedSet = new Set();
    for (const c of openComments) {
      const anchor = matchCommentRowIndex(this.annotatedRows, c);
      if (anchor >= 0) commentedSet.add(anchor);
    }
    const accent = t.accent ?? "#3b82f6";
    const gitInfoEl = this.renderGitInfo(t, cx);
    const commitBannerEl = this.renderCommitBanner(t, diffMode);
    const externalBannerEl = !commitBannerEl ? this.renderExternalBanner(t, diffMode) : null;
    const gitLogEl = this.renderGitLog(t);
    const openFileRec = !this.selectedCommit && !this.selectedExternal && this.open ? this.files.find((f) => f.path === this.open) : null;
    const diffListId = diffMode === "split" ? "changes-diff-split" : "changes-diff";
    const externalList = Array.isArray(this.externalFiles) ? this.externalFiles : [];

    const fileListPane = v_flex()
      .id("changes-sidebar")
      .size_full()
      .min_w(0)
      .min_h(0)
      .border_r(1)
      .border_color(t.border)
      .child(
        div()
          .flex_1()
          .w_full()
          .min_w(0)
          .min_h(60)
          .relative()
          .child(
            v_flex()
              .id("changes-list")
              .absolute()
              .inset_0()
              .overflow_y_scroll()
              .p(6)
              .children(this.files.map((f) => {
                const parts = f.path.split("/"), name = parts.pop();
                const cCount = countByFile.get(f.path) ?? 0;
                return h_flex()
                  .id(`change-${f.path}`)
                  .h(28)
                  .flex_none()
                  .px(8)
                  .gap(6)
                  .items_center()
                  .rounded(7)
                  .cursor_pointer()
                  .tooltip(f.path)
                  .when(!this.selectedCommit && !this.selectedExternal && this.open === f.path, (e) => e.bg(t.selected))
                  .hover((s) => s.bg(t.hover))
                  .on_click((_e, cx) => this.pick(f.path, cx))
                  .child(div().w(14).flex_none().text_size(11).text_color(f.status === "A" ? t.ansi2 : f.status === "D" ? t.ansi1 : f.status === "U" ? t.ansi1 : t.blocked).child(f.status))
                  .child(
                    h_flex()
                      .flex_1()
                      .min_w(0)
                      .when(parts.length > 0, (e) => e.child(div().min_w(0).truncate().text_color(t.dim).child(parts.join("/") + "/")))
                      .child(div().min_w(0).truncate().child(name)),
                  )
                  .when(Boolean(f.staged), (e) =>
                    e.child(
                      div()
                        .flex_none()
                        .px(4)
                        .py(1)
                        .rounded(4)
                        .bg(withAlpha(t.ansi2, "1c") ?? t.raised)
                        .text_size(9.5)
                        .text_color(t.ansi2)
                        .tooltip(tr("Staged in index", "已暂存到暂存区"))
                        .child(tr("staged", "暂存")),
                    ),
                  )
                  .when(cCount > 0, (e) =>
                    e.child(
                      div()
                        .flex_none()
                        .px(5)
                        .py(1)
                        .rounded(4)
                        .bg(withAlpha(accent, "22") ?? t.raised)
                        .text_size(10)
                        .font_semibold()
                        .text_color(accent)
                        .child(String(cCount)),
                    ),
                  )
                  .when(f.add > 0, (e) => e.child(div().flex_none().text_size(11).text_color(t.ansi2).child(`+${f.add}`)))
                  .when(f.del > 0, (e) => e.child(div().flex_none().text_size(11).text_color(t.ansi1).child(`−${f.del}`)));
              }))
              .when(externalList.length > 0, (e) =>
                e.child(
                  v_flex()
                    .id("changes-external-section")
                    .w_full()
                    .min_w(0)
                    .when(this.files.length > 0, (x) => x.mt(8).pt(8).border_t(1).border_color(t.border))
                    .gap(2)
                    .child(
                      h_flex()
                        .w_full()
                        .px(8)
                        .py(2)
                        .gap(6)
                        .items_center()
                        .child(
                          div()
                            .flex_1()
                            .min_w(0)
                            .truncate()
                            .text_size(10.5)
                            .font_semibold()
                            .text_color(t.blocked ?? t.muted)
                            .child(tr(`External (${externalList.length})`, `项目外变更 (${externalList.length})`)),
                        )
                        .child(
                          div()
                            .id("chg-ext-clear-all")
                            .flex_none()
                            .px(4)
                            .py(1)
                            .rounded(4)
                            .text_size(10)
                            .text_color(t.dim)
                            .cursor_pointer()
                            .hover((s) => s.bg(t.hover).text_color(t.foreground))
                            .tooltip(tr("Clear external files", "清空项目外变更列表"))
                            .on_click((_e, cx) => this.closeExternalPreview(cx, true))
                            .child("×"),
                        ),
                    )
                    .children(externalList.map((ext) => {
                      const norm = String(ext.path ?? "").replace(/\\/g, "/");
                      const idx = norm.lastIndexOf("/");
                      const dir = idx >= 0 ? norm.slice(0, idx + 1) : "";
                      const name = idx >= 0 ? norm.slice(idx + 1) : norm;
                      const cCount = countByFile.get(ext.path) ?? 0;
                      const active = !this.selectedCommit && this.selectedExternal?.path === ext.path;
                      return h_flex()
                        .id(`change-ext-${ext.path}`)
                        .h(28)
                        .flex_none()
                        .px(8)
                        .gap(6)
                        .items_center()
                        .rounded(7)
                        .cursor_pointer()
                        .tooltip(ext.path)
                        .when(active, (x) => x.bg(t.selected))
                        .hover((s) => s.bg(t.hover))
                        .on_click((_e, cx) => this.pickExternal(ext, cx))
                        .child(div().w(14).flex_none().text_size(11).text_color(ext.status === "A" ? t.ansi2 : t.blocked).child(ext.status || "M"))
                        .child(
                          h_flex()
                            .flex_1()
                            .min_w(0)
                            .when(Boolean(dir), (x) => x.child(div().min_w(0).truncate().text_color(t.dim).child(dir)))
                            .child(div().min_w(0).truncate().child(name)),
                        )
                        .when(cCount > 0, (x) =>
                          x.child(
                            div()
                              .flex_none()
                              .px(5)
                              .py(1)
                              .rounded(4)
                              .bg(withAlpha(accent, "22") ?? t.raised)
                              .text_size(10)
                              .font_semibold()
                              .text_color(accent)
                              .child(String(cCount)),
                          ),
                        )
                        .when(ext.add > 0, (x) => x.child(div().flex_none().text_size(11).text_color(t.ansi2).child(`+${ext.add}`)))
                        .when(ext.del > 0, (x) => x.child(div().flex_none().text_size(11).text_color(t.ansi1).child(`−${ext.del}`)));
                    })),
                ),
              )
              .when(!this.files.length && !externalList.length, (e) => e.child(div().p(8).text_color(t.dim).whitespace_normal().child(this.error || (this.loading ? tr("Loading changes…", "正在加载变更…") : this.root ? tr("No uncommitted changes", "暂无未提交变更") : tr("No folder", "无工作目录"))))),
          )
          .child(Scrollbar.vertical("changes-list").absolute().inset_0()),
      )
      .when(this.limited, (el) => el.child(div().flex_none().px(10).py(4).text_size(11).text_color(t.dim).whitespace_normal().child(tr("Showing the first 1,000 changed files", "仅显示前 1,000 个变更文件"))));

    const diffDetailPane = v_flex()
      .id("changes-detail")
      .size_full()
      .min_w(0)
      .min_h(0)
      .when(Boolean(commitBannerEl), (el) => el.child(commitBannerEl))
      .when(Boolean(externalBannerEl), (el) => el.child(externalBannerEl))
      .when(!commitBannerEl && !externalBannerEl, (el) =>
        el.child(
          h_flex()
            .id("chg-diff-toolbar")
            .h(32)
            .flex_none()
            .w_full()
            .min_w(0)
            .px(10)
            .gap(6)
            .items_center()
            .bg(t.raised)
            .border_b(1)
            .border_color(t.border)
            .child(
              div()
                .flex_1()
                .min_w(0)
                .truncate()
                .text_size(11.5)
                .font_medium()
                .text_color(this.open ? t.foreground : t.dim)
                .when(Boolean(this.open), (e) => e.tooltip(this.open))
                .child(this.open || tr("Diff", "变更详情")),
            )
            .when(Boolean(openFileRec?.add > 0), (e) => e.child(div().flex_none().text_size(10.5).text_color(t.ansi2).child(`+${openFileRec.add}`)))
            .when(Boolean(openFileRec?.del > 0), (e) => e.child(div().flex_none().text_size(10.5).text_color(t.ansi1).child(`−${openFileRec.del}`)))
            .child(this.renderDiffModeToggle(t, diffMode)),
        ),
      )
      .when(diffMode === "split" && this.rows.length > 0, (el) =>
        el.child(
          h_flex()
            .id("chg-split-header")
            .h(22)
            .flex_none()
            .w_full()
            .min_w(0)
            .items_center()
            .bg(t.surface ?? t.raised)
            .border_b(1)
            .border_color(t.border)
            .child(div().flex_1().min_w(0).px(10).truncate().text_size(10.5).font_medium().text_color(t.muted).child(tr("Before", "修改前")))
            .child(div().w(1).h_full().flex_none().bg(t.border))
            .child(div().flex_1().min_w(0).px(10).truncate().text_size(10.5).font_medium().text_color(t.muted).child(tr("After", "修改后"))),
        ),
      )
      .child(
        div()
          .flex_1()
          .w_full()
          .min_w(0)
          .min_h(0)
          .relative()
          .overflow_hidden()
          .font_family(settings().font_family ?? "JetBrains Mono")
          .text_size(11)
          .on_mouse_down("left", (e) => { this.lastShift = Boolean(e?.modifiers?.shift); })
          .child(
            list(
              diffListId,
              items.length,
              (i) => items[i]?.key ?? String(i),
              (i) => {
                const item = items[i];
                if (!item) return div().h(0);
                if (item.type === "comment") return this.renderSavedComment(item, t);
                if (item.type === "split") return this.renderSplitRow(item.srow, t, activeComposer, commentedSet);
                return this.renderCodeRow(item.row, t, activeComposer, commentedSet);
              },
            ).on_item_click((itemKey, cx) => this.handleVirtualItemClick(itemKey, cx)).size_full(),
          )
          .child(Scrollbar.vertical(diffListId).viewport_from_layout().absolute().inset_0())
          .when(this.commitLoading, (e) => e.child(div().absolute().top(6).px(12).text_color(t.dim).child(tr("Loading commit diff…", "正在加载提交差异…"))))
          .when(this.externalLoading, (e) => e.child(div().absolute().top(6).px(12).text_color(t.dim).child(this.selectedExternal?.inProject ? tr("Loading diff…", "正在加载变更…") : tr("Loading external diff…", "正在加载项目外变更…"))))
          .when(!this.selectedCommit && !this.selectedExternal && !this.open && this.files.length > 0, (e) => e.child(div().absolute().top(6).px(12).text_color(t.dim).child(tr("Select a file to view its changes", "选择一个文件以查看变更详情"))))
          .when(!this.selectedCommit && !this.selectedExternal && !this.open && !this.files.length && this.gitLog.length > 0, (e) => e.child(div().absolute().top(6).px(12).text_color(t.dim).child(tr("Select a commit below to inspect its diff", "点击下方最近提交可查看历史变更详情")))),
      )
      .when(Boolean(activeComposer), (e) => e.child(this.renderDockedComposer(activeComposer, t)))
      .when(allDrafts.length > 0 || Boolean(this.reviewError), (e) => e.child(this.renderReviewBar(t, allDrafts)));

    return v_flex().id("changes").size_full().min_h(0).rounded(10).overflow_hidden().bg(t.background).border_1().border_color(t.border).text_color(t.foreground).text_size(13).track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") {
          if (this.targetPopupOpen) {
            this.targetPopupOpen = false;
            cx.stop_propagation();
            cx.notify();
          } else if (this.composer) {
            this.closeComposer(cx);
            cx.stop_propagation();
          } else if (this.selectedCommit) {
            this.closeCommitPreview(cx);
            cx.stop_propagation();
          } else if (this.selectedExternal) {
            this.closeExternalPreview(cx);
            cx.stop_propagation();
          } else if (this.typing) {
            this.typing = false;
            this.handle.focus();
            cx.stop_propagation();
            cx.notify();
          }
        }
      })
      .child(
        h_flex().h(42).flex_none().px(12).gap(8).items_center().border_b(1).border_color(t.border)
          .child(div().font_semibold().child(tr("Changes", "变更")))
          .child(div().px(5).py(1).rounded(4).border_1().border_color(t.border).text_size(10).text_color(t.muted).child(tr(`${this.files.length} files`, `${this.files.length} 个文件`)))
          .when(externalList.length > 0, (el) =>
            el.child(
              div()
                .px(5)
                .py(1)
                .rounded(4)
                .bg(withAlpha(t.blocked ?? "#f59e0b", "20") ?? t.raised)
                .text_size(10)
                .font_semibold()
                .text_color(t.blocked ?? "#f59e0b")
                .child(tr(`${externalList.length} external`, `${externalList.length} 项目外`)),
            ),
          )
          .when(allDrafts.length > 0, (el) =>
            el.child(
              div()
                .px(5)
                .py(1)
                .rounded(4)
                .bg(withAlpha(accent, "20") ?? t.raised)
                .text_size(10)
                .font_semibold()
                .text_color(accent)
                .child(tr(`${allDrafts.length} note${allDrafts.length === 1 ? "" : "s"}`, `${allDrafts.length} 条批注`)),
            ),
          )
          .child(div().flex_1())
          .child(div().text_size(11).text_color(t.ansi2).child(`+${add}`))
          .child(div().text_size(11).text_color(t.ansi1).child(`−${del}`)),
      )
      .when(Boolean(gitInfoEl), (el) => el.child(gitInfoEl))
      .child(
        h_resizable("changes-split")
          .flex_1()
          .min_h(0)
          .w_full()
          .child(
            resizable_panel()
              .size(220)
              .size_range(120, 480)
              .child(fileListPane),
          )
          .child(
            resizable_panel()
              .size_range(160)
              .child(diffDetailPane),
          ),
      )
      .when(Boolean(gitLogEl), (el) => el.child(gitLogEl))
      .when(Boolean(this.note), (e) => e.child(div().px(10).pb(8).text_size(11).text_color(t.ansi1).whitespace_normal().child(this.note)))
      .child(h_flex().flex_none().p(10).gap(6).items_center().border_t(1).border_color(t.border)
        .child(
          h_flex()
            .id("commit-message")
            .flex_1()
            .min_w(0)
            .h(32)
            .px(10)
            .gap(6)
            .items_center()
            .rounded(8)
            .border_1()
            .border_color(this.typing ? t.accent : t.border)
            .cursor_text()
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .child(Input.new(this.input).flex_1().min_w(0).h_full().text_size(12).text_color(t.foreground))
            .when(this.aiCommitUsedAi && Boolean(this.message.trim()), (el) =>
              el.child(
                div()
                  .flex_none()
                  .px(4)
                  .py(1)
                  .rounded(4)
                  .bg(withAlpha("#a855f7", "22") ?? t.raised)
                  .text_size(9.5)
                  .font_semibold()
                  .text_color("#a855f7")
                  .child("✨ AI"),
              ),
            ),
        )
        .child((() => {
          const canAi = Boolean(this.root && this.files.length > 0 && !this.busy && !this.loading && !this.aiCommitLoading);
          return h_flex()
            .id("chg-ai-commit")
            .h(32)
            .px(9)
            .flex_none()
            .items_center()
            .rounded(8)
            .border_1()
            .border_color(canAi ? (withAlpha("#a855f7", "66") ?? t.border) : t.border)
            .bg(canAi ? (withAlpha("#a855f7", "18") ?? t.raised) : t.raised)
            .text_size(11.5)
            .font_semibold()
            .text_color(canAi ? "#a855f7" : t.dim)
            .when(canAi, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha("#a855f7", "2c") ?? t.hover)))
            .tooltip(tr("Generate commit message with Core AI from current changes", "由 Core AI 根据当前代码变更生成 Git 提交信息"))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              this.suggestCommitMessage(cx);
            })
            .child(this.aiCommitLoading ? tr("✨ …", "✨ 生成中…") : tr("✨ AI", "✨ AI 生成"));
        })())
        .child(div().id("commit").h(32).px(14).flex().items_center().rounded(8).bg(enabled ? t.solid : t.raised).text_color(enabled ? t.solid_foreground : t.dim).text_size(12).when(enabled, (e) => e.cursor_pointer()).on_click((_e, cx) => this.commit(cx)).child(this.busy ? tr("Committing…", "提交中…") : tr("Commit", "提交"))));
  }
}
