// Pure Git metadata parsers for the Changes mod.
// Zero GPUI / host imports so unit tests can import directly.

function afterNthSpace(str, n) {
  let idx = -1;
  for (let count = 0; count < n; count++) {
    idx = str.indexOf(" ", idx + 1);
    if (idx < 0) return "";
  }
  return str.slice(idx + 1);
}

/**
 * Parse `git status --porcelain=v2 --branch -z --no-renames --untracked-files=all`.
 */
export function parseGitStatusV2(stdout, { limit = 1000 } = {}) {
  const tokens = String(stdout ?? "").split("\0");
  let branch = "";
  let detached = false;
  let hasHead = false;
  let headOid = "";
  let headShort = "";
  let upstream = "";
  let hasAb = false;
  let ahead = 0;
  let behind = 0;

  let staged = 0;
  let unstaged = 0;
  let untracked = 0;
  let conflicts = 0;
  let total = 0;
  const files = [];

  for (let i = 0; i < tokens.length; i++) {
    const entry = tokens[i];
    if (!entry) continue;

    if (entry.startsWith("# branch.oid ")) {
      const val = entry.slice("# branch.oid ".length).trim();
      if (val && val !== "(initial)") {
        hasHead = true;
        headOid = val;
        headShort = val.slice(0, 7);
      } else {
        hasHead = false;
        headOid = "";
        headShort = "";
      }
      continue;
    }

    if (entry.startsWith("# branch.head ")) {
      const val = entry.slice("# branch.head ".length).trim();
      if (val === "(detached)") {
        detached = true;
        branch = "";
      } else {
        detached = false;
        branch = val;
      }
      continue;
    }

    if (entry.startsWith("# branch.upstream ")) {
      upstream = entry.slice("# branch.upstream ".length).trim();
      continue;
    }

    if (entry.startsWith("# branch.ab ")) {
      const m = /\+(\d+)\s+-(\d+)/.exec(entry);
      if (m) {
        hasAb = true;
        ahead = Number(m[1]) || 0;
        behind = Number(m[2]) || 0;
      }
      continue;
    }

    if (entry.startsWith("# ")) continue;

    if (entry.startsWith("1 ")) {
      const xy = entry.slice(2, 4);
      const path = afterNthSpace(entry, 8);
      if (!path) continue;
      const x = xy[0] || ".";
      const y = xy[1] || ".";
      const isStaged = x !== ".";
      const isUnstaged = y !== ".";
      if (isStaged) staged += 1;
      if (isUnstaged) unstaged += 1;
      total += 1;
      if (files.length < limit) {
        const status = xy.includes("D") ? "D" : xy.includes("A") ? "A" : "M";
        files.push({ path, status, xy, staged: isStaged, unstaged: isUnstaged, fresh: false, conflict: false });
      }
      continue;
    }

    if (entry.startsWith("2 ")) {
      const xy = entry.slice(2, 4);
      const path = afterNthSpace(entry, 9);
      // Rename/copy entry in -z mode is followed by NUL-delimited origPath.
      i += 1;
      if (!path) continue;
      const x = xy[0] || ".";
      const y = xy[1] || ".";
      const isStaged = x !== ".";
      const isUnstaged = y !== ".";
      if (isStaged) staged += 1;
      if (isUnstaged) unstaged += 1;
      total += 1;
      if (files.length < limit) {
        const status = xy.includes("D") ? "D" : xy.includes("A") ? "A" : "M";
        files.push({ path, status, xy, staged: isStaged, unstaged: isUnstaged, fresh: false, conflict: false });
      }
      continue;
    }

    if (entry.startsWith("u ")) {
      const xy = entry.slice(2, 4);
      const path = afterNthSpace(entry, 10);
      if (!path) continue;
      conflicts += 1;
      total += 1;
      if (files.length < limit) {
        files.push({ path, status: "U", xy, staged: false, unstaged: true, fresh: false, conflict: true });
      }
      continue;
    }

    if (entry.startsWith("? ")) {
      const path = entry.slice(2);
      if (!path) continue;
      untracked += 1;
      total += 1;
      if (files.length < limit) {
        files.push({ path, status: "A", xy: "??", staged: false, unstaged: false, fresh: true, conflict: false });
      }
    }
  }

  return {
    branch: {
      name: branch,
      detached,
      hasHead,
      headOid,
      headShort,
      upstream,
      upstreamGone: Boolean(upstream && !hasAb && hasHead),
      ahead,
      behind,
    },
    status: {
      staged,
      unstaged,
      untracked,
      conflicts,
      total,
      clean: total === 0,
    },
    files,
    limited: total > limit,
  };
}

/**
 * Parse `git log -n N --format=%H%x1f%h%x1f%s%x1f%an%x1f%ar%x1f%D%x1e`.
 */
export function parseGitLog(stdout) {
  const out = [];
  for (const raw of String(stdout ?? "").split("\x1e")) {
    const rec = raw.replace(/^[\r\n]+/, "");
    if (!rec.trim()) continue;
    const [hash = "", shortHash = "", subject = "", author = "", relTime = "", rawRefs = ""] = rec.split("\x1f");
    if (!hash) continue;
    const refs = rawRefs
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    out.push({
      hash: hash.trim(),
      shortHash: (shortHash || hash.slice(0, 7)).trim(),
      subject: subject.trim(),
      author: author.trim(),
      relTime: relTime.trim(),
      refs,
    });
  }
  return out;
}

/**
 * Parse `git stash list` output to stash count.
 */
export function parseStashCount(stdout) {
  let count = 0;
  for (const line of String(stdout ?? "").split("\n")) {
    if (line.trim()) count += 1;
  }
  return count;
}

/**
 * Compact primary ref badge for a git log row (e.g. "HEAD -> master" -> "HEAD", "tag: v1.0" -> "v1.0").
 */
export function formatPrimaryRef(refs) {
  if (!Array.isArray(refs) || !refs.length) return "";
  const headRef = refs.find((r) => r.startsWith("HEAD -> "));
  if (headRef) return headRef.slice("HEAD -> ".length).trim();
  if (refs.includes("HEAD")) return "HEAD";
  const tagRef = refs.find((r) => r.startsWith("tag: "));
  if (tagRef) return tagRef.slice("tag: ".length).trim();
  return refs[0];
}
