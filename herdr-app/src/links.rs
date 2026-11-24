//! Links in terminal text: Ghostty's URL and file path pattern, what a match
//! points to, and handing it to the system to open.

use fancy_regex::Regex;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Ghostty's default link pattern (`src/config/url.zig`), with ASCII word
/// boundaries (so CJK text adjacent to paths still matches) and Windows drive paths.
static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let schemes = r"https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:";
    let ipv6 = r"(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?)";
    let scheme_chars = r"[\w\-.~:/?#@!$&*+,;=%]";
    let path_chars = r"[\w\-.~:\/?#@!$&*+;=%]";
    let win_path_chars = r"[\w\-.~:\\/?#@!$&*+;=%]";
    let bracketed = r"(?:[\(\[]\w*[\)\]])?";
    let no_trailing_punctuation = r"(?<![,.])";
    let no_trailing_colon = r"(?<!:)";
    let dotted = r"(?=[\w\-.~:\/?#@!$&*+;=%]*\.)";
    let undotted = r"(?![\w\-.~:\/?#@!$&*+;=%]*\.)";
    let dotted_spaces = r"(?:(?<!:) (?!\w+:\/\/)(?!\.{0,2}\/)(?!~\/)[\w\-.~:\/?#@!$&*+;=%]*[\/.])*";
    let any_spaces = r"(?:(?<!:) (?!\w+:\/\/)(?!\.{0,2}\/)(?!~\/)[\w\-.~:\/?#@!$&*+;=%]+)*";
    let rooted = r"(?:\.\.\/|\.\/|(?<![A-Za-z0-9_])~\/|(?:[\w][\w\-.]*\/)*(?<![A-Za-z0-9_])\$[A-Za-z_]\w*\/|\.[\w][\w\-.]*\/|(?<![A-Za-z0-9_~\/])\/(?!\/))";
    let scheme_url = format!("(?:{schemes})(?:{ipv6}|{scheme_chars}+{bracketed})+{no_trailing_punctuation}");
    let windows_path = format!(r"(?<![A-Za-z0-9_])[A-Za-z]:[\\/]{win_path_chars}+{no_trailing_colon}");
    let rooted_path = format!(
        "{rooted}(?:{dotted}{path_chars}+{dotted_spaces}{no_trailing_colon}|{undotted}{path_chars}+{any_spaces}{no_trailing_colon})"
    );
    let bare_path = format!(r"{dotted}(?<!\$\d*)(?<![A-Za-z0-9_])[\w][\w\-.]*\/{path_chars}+{no_trailing_colon}");
    Regex::new(&format!("{scheme_url}|{windows_path}|{rooted_path}|{bare_path}")).expect("link pattern")
});

/// The link in `text` that covers byte `at`.
pub fn find(text: &str, at: usize) -> Option<Range<usize>> {
    for m in PATTERN.find_iter(text) {
        let m = m.ok()?;
        if m.start() > at {
            return None;
        }
        if at < m.end() {
            return Some(m.range());
        }
    }
    None
}

/// The address-shaped word under the pointer, excluding surrounding punctuation.
pub fn word(text: &str, at: usize) -> Option<Range<usize>> {
    let allowed = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '/' | '.');
    if !text.get(at..)?.chars().next().is_some_and(allowed) {
        return None;
    }
    let start = text[..at].char_indices().rev().find(|(_, c)| !allowed(*c)).map_or(0, |(i, c)| i + c.len_utf8());
    let end = text[at..].char_indices().find(|(_, c)| !allowed(*c)).map_or(text.len(), |(i, _)| at + i);
    let trimmed = text[start..end].trim_matches('.');
    let start = start + text[start..end].len() - text[start..end].trim_start_matches('.').len();
    let end = start + trimmed.len();
    (start <= at && at < end).then_some(start..end)
}

/// Bare addresses refer to the hovered terminal's machine, never its names.
pub fn terminal_address<'a>(word: &str, source: &str, live: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let address = if word.contains('/') {
        word.to_owned()
    } else {
        crate::mirror::qualify(word, source.split_once('/').map(|(machine, _)| machine))
    };
    live.into_iter().any(|a| a == address).then_some(address)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Url(String),
    File(PathBuf),
}

/// What a link opens. File paths count only when they exist here; relative
/// ones need the terminal's folder.
pub fn resolve(link: &str, cwd: Option<&Path>) -> Option<Target> {
    resolve_with_location(link, cwd).map(|(target, _, _)| target)
}

/// Parses `#L10-L20`, `#L10`, `:L10-L20`, `:L10`, `:10-20`, `:10:5`, `:10`,
/// `(10)`, or `(10,5)` suffix into `(base, start_line, end_line)`.
fn split_line_suffix(raw: &str) -> (&str, Option<u32>, Option<u32>) {
    let bare = raw.trim_end_matches(['.', ':', ',', ';']);
    if let Some(head) = bare.strip_suffix(')')
        && let Some((path, inner)) = head.rsplit_once('(')
        && !path.is_empty()
    {
        let first = inner.split(',').next().unwrap_or("").trim();
        if !first.is_empty()
            && first.bytes().all(|c| c.is_ascii_digit())
            && let Ok(line) = first.parse::<u32>()
            && line > 0
        {
            return (path, Some(line), None);
        }
    }
    if let Some((head, frag)) = bare.rsplit_once('#')
        && let Some(rest) = frag.strip_prefix('L').or_else(|| frag.strip_prefix('l'))
    {
        if let Some((a, b)) = rest.split_once('-') {
            let b = b.strip_prefix('L').or_else(|| b.strip_prefix('l')).unwrap_or(b);
            if let (Ok(s), Ok(e)) = (a.parse::<u32>(), b.parse::<u32>())
                && s > 0
            {
                return (head, Some(s), Some(e.max(s)));
            }
        } else if let Ok(s) = rest.parse::<u32>()
            && s > 0
        {
            return (head, Some(s), None);
        }
    }
    if let Some((head, tail)) = bare.rsplit_once(':') {
        // `:L10-L20` or `:L10`
        if let Some(rest) = tail.strip_prefix('L').or_else(|| tail.strip_prefix('l')) {
            if let Some((a, b)) = rest.split_once('-') {
                let b = b.strip_prefix('L').or_else(|| b.strip_prefix('l')).unwrap_or(b);
                if let (Ok(s), Ok(e)) = (a.parse::<u32>(), b.parse::<u32>())
                    && s > 0
                {
                    return (head, Some(s), Some(e.max(s)));
                }
            } else if let Ok(s) = rest.parse::<u32>()
                && s > 0
            {
                return (head, Some(s), None);
            }
        }
        // `:10-20`
        if let Some((a, b)) = tail.split_once('-')
            && !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
            && let (Ok(s), Ok(e)) = (a.parse::<u32>(), b.parse::<u32>())
            && s > 0
        {
            return (head, Some(s), Some(e.max(s)));
        }
        // `:42` or `:42:7`
        if !tail.is_empty() && tail.bytes().all(|c| c.is_ascii_digit())
            && let Ok(n1) = tail.parse::<u32>()
        {
            if let Some((head2, line_s)) = head.rsplit_once(':')
                && !line_s.is_empty()
                && line_s.bytes().all(|c| c.is_ascii_digit())
                && let Ok(line) = line_s.parse::<u32>()
                && line > 0
            {
                return (head2, Some(line), None);
            }
            if n1 > 0 {
                return (head, Some(n1), None);
            }
        }
    }
    (bare, None, None)
}

fn is_url_scheme(link: &str) -> bool {
    link.split_once(':').is_some_and(|(s, _)| {
        s.len() > 1 && s.starts_with(|c: char| c.is_ascii_alphabetic()) && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    })
}

fn local_hostname() -> Option<&'static str> {
    static HOST: LazyLock<Option<String>> = LazyLock::new(|| {
        for var in ["HOSTNAME", "COMPUTERNAME"] {
            if let Ok(v) = std::env::var(var) {
                let t = v.trim();
                if !t.is_empty() {
                    return Some(t.to_owned());
                }
            }
        }
        #[cfg(unix)]
        if let Ok(raw) = std::fs::read_to_string("/etc/hostname") {
            let t = raw.trim();
            if !t.is_empty() {
                return Some(t.to_owned());
            }
        }
        None
    });
    HOST.as_deref()
}

fn resolve_local_candidate(full: &Path) -> Option<(Target, Option<u32>, Option<u32>)> {
    // Windows ignores a trailing period when checking whether a path exists.
    if !(cfg!(windows) && full.extension().is_some_and(|ext| ext.is_empty())) && full.exists() {
        return Some((Target::File(full.to_path_buf()), None, None));
    }
    // The period ending a sentence, or `:42` / `:42:7` / `#L10-L20` / `(42,7)` naming a place in the file.
    let full_str = full.to_str()?;
    let (bare, line, end_line) = split_line_suffix(full_str);
    if bare.is_empty() {
        return None;
    }
    Path::new(bare)
        .exists()
        .then(|| (Target::File(PathBuf::from(bare)), line, end_line))
}

/// Resolves a terminal link into a target plus optional 1-based `(start_line, end_line)`.
pub fn resolve_with_location(link: &str, cwd: Option<&Path>) -> Option<(Target, Option<u32>, Option<u32>)> {
    match cwd {
        Some(dir) => resolve_with_roots(link, &[dir]),
        None => resolve_with_roots(link, &[]),
    }
}

/// Resolves a terminal link against an ordered list of candidate base directories
/// (e.g. terminal `cwd`, git repo root, space folder).
pub fn resolve_with_roots(link: &str, roots: &[&Path]) -> Option<(Target, Option<u32>, Option<u32>)> {
    if is_url_scheme(link) {
        // Windows can't open a file URL whose path is percent-encoded, the way programs write them.
        if let Some((path, line, end_line)) = file_url_path_with_location(link, false)
            && path.exists()
        {
            return Some((Target::File(path), line, end_line));
        }
        return Some((Target::Url(link.to_owned()), None, None));
    }
    let home = std::env::home_dir();
    if let Some(rest) = link.strip_prefix("~/") {
        let full = home?.join(rest);
        return resolve_local_candidate(&full);
    }
    if let Some(var) = link.strip_prefix('$') {
        let (name, rest) = var.split_once('/').unwrap_or((var, ""));
        if name == "PWD" {
            for root in roots {
                if let Some(found) = resolve_local_candidate(&root.join(rest)) {
                    return Some(found);
                }
            }
            return None;
        }
        let base = match name {
            "HOME" => home?,
            _ => PathBuf::from(std::env::var_os(name)?),
        };
        return resolve_local_candidate(&base.join(rest));
    }
    let expanded = PathBuf::from(link);
    if expanded.is_absolute() {
        return resolve_local_candidate(&expanded);
    }
    for root in roots {
        if let Some(found) = resolve_local_candidate(&root.join(&expanded)) {
            return Some(found);
        }
    }
    // Git diff paths (`a/src/foo.rs`, `b/src/foo.rs`) when no literal `a/` or `b/` folder exists.
    if let Some(stripped) = link.strip_prefix("a/").or_else(|| link.strip_prefix("b/")) {
        let rel = Path::new(stripped);
        for root in roots {
            if let Some(found) = resolve_local_candidate(&root.join(rel)) {
                return Some(found);
            }
        }
    }
    None
}

/// Resolves a link printed inside a remote SSH terminal (cannot check local `Path::exists()`).
pub fn resolve_remote_with_location(link: &str, cwd: Option<&str>) -> Option<(Target, Option<u32>, Option<u32>)> {
    if is_url_scheme(link) {
        if let Some((path, line, end_line)) = file_url_path_with_location(link, true) {
            return Some((Target::File(path), line, end_line));
        }
        return Some((Target::Url(link.to_owned()), None, None));
    }
    let (bare, line, end_line) = split_line_suffix(link);
    if bare.is_empty() || bare.starts_with('$') {
        return None;
    }
    let is_win_abs = bare.len() >= 3
        && bare.as_bytes()[0].is_ascii_alphabetic()
        && bare.as_bytes()[1] == b':'
        && matches!(bare.as_bytes()[2], b'/' | b'\\');
    let full = if bare.starts_with('/') || bare.starts_with("~/") || is_win_abs {
        bare.to_owned()
    } else if let Some(dir) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        format!("{}/{}", dir.trim_end_matches('/'), bare.trim_start_matches("./"))
    } else {
        return None;
    };
    Some((Target::File(PathBuf::from(full)), line, end_line))
}

fn is_path_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '_' | '/' | '\\' | '.' | ':' | '#' | '~' | '@' | '+' | '%' | '$' | '(' | ')' | ','
        )
        || (!c.is_ascii() && c.is_alphanumeric())
}

/// Extracts a path-like token under byte `at` (including single filenames like `Cargo.toml`,
/// `src/justfile`, `src/main.rs(42,7)`, or Windows `src\main.rs:10`), trimming outer quotes/brackets.
fn path_token(text: &str, at: usize) -> Option<Range<usize>> {
    if !text.get(at..)?.chars().next().is_some_and(is_path_token_char) {
        return None;
    }
    let mut start = text[..at]
        .char_indices()
        .rev()
        .find(|(_, c)| !is_path_token_char(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let mut end = text[at..]
        .char_indices()
        .find(|(_, c)| !is_path_token_char(*c))
        .map_or(text.len(), |(i, _)| at + i);

    // Strip leading non-path delimiters like `(` or `,` or `:`.
    while start < end {
        let ch = text[start..end].chars().next()?;
        if matches!(ch, '(' | ')' | ',' | ':' | ';') {
            start += ch.len_utf8();
        } else {
            break;
        }
    }
    // Strip trailing punctuation, while preserving `file(42)` / `file(42,10)`.
    while start < end {
        let slice = &text[start..end];
        let ch = slice.chars().next_back()?;
        if matches!(ch, ',' | ';' | ':') {
            end -= ch.len_utf8();
            continue;
        }
        if ch == ')' {
            let open = slice.matches('(').count();
            let close = slice.matches(')').count();
            if close > open || !split_line_suffix(slice).1.is_some() {
                end -= 1;
                continue;
            }
        }
        if ch == '(' {
            end -= 1;
            continue;
        }
        break;
    }
    (start <= at && at < end).then_some(start..end)
}

/// Trims trailing non-ASCII characters (e.g. Chinese text glued onto an ASCII path like `src/app.rs中的函数`
/// or `src/app.rs:42行`) while keeping `at` inside the range.
fn trim_trailing_non_ascii(text: &str, range: Range<usize>, at: usize) -> Option<Range<usize>> {
    let mut end = range.end;
    while end > range.start {
        let ch = text[range.start..end].chars().next_back()?;
        if !ch.is_ascii() {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    (end < range.end && range.start <= at && at < end).then_some(range.start..end)
}

/// Trims leading non-ASCII characters (e.g. `查看Cargo.toml` -> `Cargo.toml`) while keeping `at` inside.
fn trim_leading_non_ascii(text: &str, range: Range<usize>, at: usize) -> Option<Range<usize>> {
    let mut start = range.start;
    while start < range.end {
        let ch = text[start..range.end].chars().next()?;
        if !ch.is_ascii() {
            start += ch.len_utf8();
        } else {
            break;
        }
    }
    (start > range.start && start <= at && at < range.end).then_some(start..range.end)
}

/// Extends a matched file path range to absorb an immediately following `(line)` or `(line,col)` suffix.
fn extend_paren_location(text: &str, range: Range<usize>) -> Option<(Range<usize>, Option<u32>, Option<u32>)> {
    let rest = text.get(range.end..)?;
    if !rest.starts_with('(') {
        return None;
    }
    let close = rest.find(')')?;
    let extended = range.start..(range.end + close + 1);
    let (_, line, end_line) = split_line_suffix(&text[extended.clone()]);
    line.map(|_| (extended, line, end_line))
}

fn try_resolve_range_local(
    text: &str,
    range: Range<usize>,
    at: usize,
    roots: &[&Path],
    joined: bool,
) -> Option<(Range<usize>, Target, Option<u32>, Option<u32>)> {
    let slice = &text[range.clone()];
    if let Some((target, mut line, mut end_line)) = resolve_with_roots(slice, roots)
        && (!joined || matches!(target, Target::File(_)))
    {
        let mut final_range = range;
        if line.is_none()
            && matches!(target, Target::File(_))
            && let Some((ext_range, ext_line, ext_end)) = extend_paren_location(text, final_range.clone())
        {
            final_range = ext_range;
            line = ext_line;
            end_line = ext_end;
        }
        return Some((final_range, target, line, end_line));
    }
    // If trailing non-ASCII prose was glued to an ASCII path (e.g. `src/app.rs:42行`), retry trimmed.
    if let Some(trimmed) = trim_trailing_non_ascii(text, range.clone(), at)
        && let Some((target, line, end_line)) = resolve_with_roots(&text[trimmed.clone()], roots)
        && (!joined || matches!(target, Target::File(_)))
    {
        return Some((trimmed, target, line, end_line));
    }
    // If leading non-ASCII prose was glued (e.g. `查看Cargo.toml`), retry trimmed.
    if let Some(trimmed) = trim_leading_non_ascii(text, range.clone(), at) {
        if let Some((target, line, end_line)) = resolve_with_roots(&text[trimmed.clone()], roots)
            && (!joined || matches!(target, Target::File(_)))
        {
            return Some((trimmed, target, line, end_line));
        }
        if let Some(both) = trim_trailing_non_ascii(text, trimmed, at)
            && let Some((target, line, end_line)) = resolve_with_roots(&text[both.clone()], roots)
            && (!joined || matches!(target, Target::File(_)))
        {
            return Some((both, target, line, end_line));
        }
    }
    None
}

/// Finds and resolves the link under byte `at` for a local terminal.
/// Handles Ghostty URLs/paths, space-separated paths, CJK-adjacent paths,
/// Windows paths, single filenames in `roots`, and extensionless relative paths.
pub fn find_and_resolve_local(
    text: &str,
    at: usize,
    roots: &[&Path],
    joined: bool,
) -> Option<(Range<usize>, Target, Option<u32>, Option<u32>)> {
    if let Some(range) = find(text, at) {
        if let Some(found) = try_resolve_range_local(text, range.clone(), at, roots, joined) {
            return Some(found);
        }
        // `rooted_path` allows spaces for paths like `/tmp/test folder/file.txt`, which can
        // gobble adjacent words or a second path (`/tmp/a.rs src/b.rs`, `/tmp/Makefile for`).
        let matched = &text[range.clone()];
        if matched.contains(' ') {
            // Try trimming from the right at each space while keeping `at` covered.
            let mut end = range.end;
            while let Some(rel_sp) = text[range.start..end].rfind(' ') {
                end = range.start + rel_sp;
                if at >= end {
                    break;
                }
                if let Some(found) = try_resolve_range_local(text, range.start..end, at, roots, joined) {
                    return Some(found);
                }
            }
            // If `at` was inside a later space-separated word (e.g. `src/b.rs` in `/tmp/a.rs src/b.rs`),
            // retry finding/resolving starting after the preceding space.
            if let Some(rel_sp) = text[range.start..at].rfind(' ') {
                let sub_start = range.start + rel_sp + 1;
                if sub_start <= at && sub_start < text.len() {
                    if let Some(sub) = find(&text[sub_start..], at - sub_start) {
                        let abs_range = (sub_start + sub.start)..(sub_start + sub.end);
                        if let Some(found) = try_resolve_range_local(text, abs_range, at, roots, joined) {
                            return Some(found);
                        }
                    }
                }
            }
        }
    }
    if joined {
        return None;
    }
    let token = path_token(text, at)?;
    try_resolve_range_local(text, token, at, roots, false)
}

fn is_known_remote_filename(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "makefile"
            | "dockerfile"
            | "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "tsconfig.json"
            | "readme.md"
            | "claude.md"
            | "agents.md"
            | "skill.md"
            | "justfile"
            | ".gitignore"
    )
}

fn looks_like_remote_file_token(raw: &str) -> bool {
    let (bare, line, _) = split_line_suffix(raw);
    if bare.is_empty() || bare.contains(' ') || bare.starts_with("//") {
        return false;
    }
    if bare.starts_with('/') || bare.starts_with("~/") || bare.starts_with("./") || bare.starts_with("../") {
        return bare.len() > 2;
    }
    if bare.len() >= 3 && bare.as_bytes()[0].is_ascii_alphabetic() && bare.as_bytes()[1] == b':' && matches!(bare.as_bytes()[2], b'/' | b'\\') {
        return true;
    }
    if let Some((_, last)) = bare.rsplit_once(['/', '\\']) {
        return !last.is_empty() && (last.contains('.') || is_known_remote_filename(last));
    }
    is_known_remote_filename(bare) || (line.is_some() && bare.contains('.'))
}

/// Finds and resolves the link under byte `at` for a remote SSH/WSL terminal.
pub fn find_and_resolve_remote(
    text: &str,
    at: usize,
    cwd: Option<&str>,
    joined: bool,
) -> Option<(Range<usize>, Target, Option<u32>, Option<u32>)> {
    if let Some(mut range) = find(text, at) {
        let matched = &text[range.clone()];
        // On joined hard-wrapped rows, only stitch URLs or space-free remote file paths
        // so unrelated prose on adjacent rows is never fused into a bogus path.
        if joined && !is_url_scheme(matched) && (matched.contains(' ') || !looks_like_remote_file_token(matched)) {
            return None;
        }
        // Avoid sending space-gobbled prose (e.g. `/srv/Makefile for details`) to remote `cat`.
        if !is_url_scheme(matched) && matched.contains(' ') {
            let word_start = text[range.start..at].rfind(' ').map_or(range.start, |i| range.start + i + 1);
            let word_end = text[at..range.end].find(' ').map_or(range.end, |i| at + i);
            if word_start <= at && at < word_end && looks_like_remote_file_token(&text[word_start..word_end]) {
                range = word_start..word_end;
            }
        }
        if !is_url_scheme(&text[range.clone()]) {
            if let Some(trimmed) = trim_leading_non_ascii(text, range.clone(), at)
                && !text[trimmed.clone()].starts_with('/')
                && looks_like_remote_file_token(&text[trimmed.clone()])
            {
                range = trimmed;
            }
            if let Some(trimmed) = trim_trailing_non_ascii(text, range.clone(), at)
                && looks_like_remote_file_token(&text[trimmed.clone()])
            {
                range = trimmed;
            }
        }
        if let Some((target, mut line, mut end_line)) = resolve_remote_with_location(&text[range.clone()], cwd) {
            if line.is_none()
                && matches!(target, Target::File(_))
                && let Some((ext_range, ext_line, ext_end)) = extend_paren_location(text, range.clone())
            {
                range = ext_range;
                line = ext_line;
                end_line = ext_end;
            }
            return Some((range, target, line, end_line));
        }
    }
    if joined {
        return None;
    }
    let mut token = path_token(text, at)?;
    if let Some(trimmed) = trim_leading_non_ascii(text, token.clone(), at) {
        token = trimmed;
    }
    if let Some(trimmed) = trim_trailing_non_ascii(text, token.clone(), at) {
        token = trimmed;
    }
    if !looks_like_remote_file_token(&text[token.clone()]) {
        return None;
    }
    let (target, line, end_line) = resolve_remote_with_location(&text[token.clone()], cwd)?;
    Some((token, target, line, end_line))
}

/// The path a `file:` URL names on this computer, percent-decoded, with optional `#L10-L20` / `:10`.
fn file_url_path_with_location(link: &str, allow_any_host: bool) -> Option<(PathBuf, Option<u32>, Option<u32>)> {
    let rest = link.strip_prefix("file://")?;
    let rest = if rest.starts_with('/') {
        rest
    } else if let Some((host, _)) = rest.split_once('/') {
        let local_ok = host.eq_ignore_ascii_case("localhost")
            || local_hostname().is_some_and(|h| host.eq_ignore_ascii_case(h));
        if allow_any_host || local_ok {
            &rest[host.len()..]
        } else {
            return None;
        }
    } else {
        return None;
    };
    if !rest.starts_with('/') {
        return None;
    }
    let (path_and_query, frag) = match rest.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (rest, None),
    };
    let path_part = path_and_query.split('?').next()?;
    let mut bytes = Vec::with_capacity(path_part.len());
    let mut i = 0;
    while i < path_part.len() {
        if path_part.as_bytes()[i] == b'%' {
            bytes.push(u8::from_str_radix(path_part.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            bytes.push(path_part.as_bytes()[i]);
            i += 1;
        }
    }
    let mut path = String::from_utf8(bytes).ok()?;
    // `/C:/Users/...` names `C:\Users\...`.
    if cfg!(windows) && path.as_bytes().get(2) == Some(&b':') && path.as_bytes()[1].is_ascii_alphabetic() {
        path = path[1..].replace('/', "\\");
    }
    let mut line = None;
    let mut end_line = None;
    if let Some(f) = frag {
        let probe = format!("x#{f}");
        let (_, l, e) = split_line_suffix(&probe);
        line = l;
        end_line = e;
    }
    if line.is_none() && (allow_any_host || !Path::new(&path).exists()) {
        let (stripped, l, e) = split_line_suffix(&path);
        if l.is_some() {
            path = stripped.to_owned();
            line = l;
            end_line = e;
        }
    }
    Some((PathBuf::from(path), line, end_line))
}

/// Hands the target to the system's default handler.
pub fn open(target: &Target) {
    let arg = match target {
        Target::Url(u) => std::ffi::OsStr::new(u),
        Target::File(p) => p.as_os_str(),
    };
    open_system(arg);
}

#[cfg(windows)]
fn open_system(arg: &std::ffi::OsStr) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };

    let mut wide: Vec<u16> = arg.encode_wide().collect();
    if wide.contains(&0) {
        tracing::warn!("cannot open a link containing a NUL");
        return;
    }
    wide.push(0);
    // ShellExecuteW can pump messages; never reenter GPUI while it handles a click.
    std::thread::spawn(move || {
        let initialized = unsafe { CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
        // Explorer interprets '=' as its own arguments; the handler gets a literal target.
        let result = unsafe {
            ShellExecuteW(std::ptr::null_mut(), std::ptr::null(), wide.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL)
        } as isize;
        if result <= 32 {
            tracing::warn!("cannot open link: ShellExecuteW returned {result}");
        }
        if initialized >= 0 {
            unsafe { CoUninitialize() };
        }
    });
}

#[cfg(not(windows))]
fn open_system(arg: &std::ffi::OsStr) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let spawned = std::process::Command::new(opener)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        // Reaped off the UI thread so it never lingers as a zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => tracing::warn!("cannot run {opener}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(text: &str) -> Option<&str> {
        PATTERN.find(text).unwrap().map(|m| m.as_str())
    }

    /// Ghostty's own cases (`src/config/url.zig`).
    #[test]
    fn matches_like_ghostty() {
        let cases = [
            ("hello https://example.com world", "https://example.com"),
            ("https://example.com/foo(bar) more", "https://example.com/foo(bar)"),
            ("https://example.com/foo(bar)baz more", "https://example.com/foo(bar)baz"),
            ("Link inside (https://example.com) parens", "https://example.com"),
            ("Link period https://example.com. More text.", "https://example.com"),
            ("Link trailing comma https://example.com, more text.", "https://example.com"),
            ("Link in double quotes \"https://example.com\" and more", "https://example.com"),
            ("Link in single quotes 'https://example.com' and more", "https://example.com"),
            ("some file with https://google.com https://duckduckgo.com links.", "https://google.com"),
            ("and links in it. links https://yahoo.com mailto:test@example.com ssh://1.2.3.4", "https://yahoo.com"),
            ("also match http://example.com non-secure links", "http://example.com"),
            ("match tel://+12123456789 phone numbers", "tel://+12123456789"),
            ("match with query url https://example.com?query=1&other=2 and more text.", "https://example.com?query=1&other=2"),
            (
                "url with dashes [mode 2027](https://github.com/contour-terminal/terminal-unicode-core) for better unicode support",
                "https://github.com/contour-terminal/terminal-unicode-core",
            ),
            ("dot.http://example.com", "http://example.com"),
            ("weird characters https://example.com/~user/?query=1&other=2#hash and more", "https://example.com/~user/?query=1&other=2#hash"),
            ("square brackets https://example.com/[foo] and more", "https://example.com/[foo]"),
            (
                "[13]:TooManyStatements: TempFile#assign_temp_file_to_entity has approx 7 statements [https://example.com/docs/Too-Many-Statements.md]",
                "https://example.com/docs/Too-Many-Statements.md",
            ),
            ("match ftp://example.com ftp links", "ftp://example.com"),
            ("match file://example.com file links", "file://example.com"),
            ("match ssh://example.com ssh links", "ssh://example.com"),
            ("match git://example.com git links", "git://example.com"),
            ("/tmp/test.txt http://www.google.com", "/tmp/test.txt"),
            ("match tel:+18005551234 tel links", "tel:+18005551234"),
            ("match magnet:?xt=urn:btih:1234567890 magnet links", "magnet:?xt=urn:btih:1234567890"),
            ("match ipfs://QmSomeHashValue ipfs links", "ipfs://QmSomeHashValue"),
            ("match ipns://QmSomeHashValue ipns links", "ipns://QmSomeHashValue"),
            ("match gemini://example.com gemini links", "gemini://example.com"),
            ("match gopher://example.com gopher links", "gopher://example.com"),
            ("match news:comp.infosystems.www.servers.unix news links", "news:comp.infosystems.www.servers.unix"),
            ("/Users/ghostty.user/code/example.py", "/Users/ghostty.user/code/example.py"),
            ("/Users/ghostty.user/code/../example.py", "/Users/ghostty.user/code/../example.py"),
            ("/Users/ghostty.user/code/../example.py hello world", "/Users/ghostty.user/code/../example.py"),
            ("../example.py", "../example.py"),
            ("../example.py ", "../example.py"),
            ("first time ../example.py contributor ", "../example.py"),
            ("[link](/home/user/ghostty.user/example)", "/home/user/ghostty.user/example"),
            ("Serving HTTP on :: port 8000 (http://[::]:8000/)", "http://[::]:8000/"),
            ("IPv6 address https://[2001:db8::1]:8080/path", "https://[2001:db8::1]:8080/path"),
            ("IPv6 localhost http://[::1]:3000", "http://[::1]:3000"),
            ("Complex IPv6 https://[2001:db8:85a3:8d3:1319:8a2e:370:7348]:443/", "https://[2001:db8:85a3:8d3:1319:8a2e:370:7348]:443/"),
            ("IPv6 with path https://[2001:db8::1]/path/to/resource", "https://[2001:db8::1]/path/to/resource"),
            ("IPv6 with query https://[2001:db8::1]:8080/api?param=value&other=123", "https://[2001:db8::1]:8080/api?param=value&other=123"),
            ("IPv6 compressed http://[2001:db8::]:80/", "http://[2001:db8::]:80/"),
            ("IPv6 multiple zeros http://[2001:0:0:0:0:0:0:1]", "http://[2001:0:0:0:0:0:0:1]"),
            ("IPv6 link-local https://[fe80::1234:5678:9abc]", "https://[fe80::1234:5678:9abc]"),
            ("IPv6 multicast http://[ff02::1]/stream", "http://[ff02::1]/stream"),
            ("IPv6 in markdown [link](http://[2001:db8::1]/docs)", "http://[2001:db8::1]/docs"),
            ("./spaces-end.   ", "./spaces-end."),
            ("./space middle", "./space middle"),
            ("../test folder/file.txt", "../test folder/file.txt"),
            ("/tmp/test folder/file.txt", "/tmp/test folder/file.txt"),
            ("/tmp/test  folder/file.txt", "/tmp/test"),
            ("diff --git a/src/font/shaper/harfbuzz.zig b/src/font/shaper/harfbuzz.zig", "a/src/font/shaper/harfbuzz.zig"),
            ("/tmp/foo /tmp/bar", "/tmp/foo"),
            ("/tmp/foo.txt /tmp/bar.txt", "/tmp/foo.txt"),
            ("src/config/url.zig", "src/config/url.zig"),
            ("app/folder/file.rb:1", "app/folder/file.rb:1"),
            ("modified:   src/config/url.zig", "src/config/url.zig"),
            ("lib/ghostty/terminal.zig:42:10", "lib/ghostty/terminal.zig:42:10"),
            ("some-pkg/src/file.txt more text", "some-pkg/src/file.txt"),
            ("src/foo.c,baz.txt", "src/foo.c"),
            ("~/foo/bar.txt", "~/foo/bar.txt"),
            ("open ~/Documents/notes.md please", "~/Documents/notes.md"),
            ("~/.config/ghostty/config", "~/.config/ghostty/config"),
            ("directory: ~/src/ghostty-org/ghostty", "~/src/ghostty-org/ghostty"),
            ("$HOME/src/config/url.zig", "$HOME/src/config/url.zig"),
            ("space dir: $PWD/src/ghostty/main.zig", "$PWD/src/ghostty/main.zig"),
            ("foo/$BAR/baz", "foo/$BAR/baz"),
            (".foo/bar/$VAR", ".foo/bar/$VAR"),
            (".config/ghostty/config", ".config/ghostty/config"),
            ("loaded from .local/share/ghostty/state.db now", ".local/share/ghostty/state.db"),
            ("../some/where", "../some/where"),
            ("  - shared/src/foo/SomeItem.m:12, shared/src/", "shared/src/foo/SomeItem.m:12"),
            ("foo.local/share", "foo.local/share"),
            ("2024/report.txt", "2024/report.txt"),
            ("./foo bar,baz", "./foo bar"),
            ("/tmp/foo bar,baz", "/tmp/foo bar"),
            ("./.config/ghostty: Needs upstream (main)", "./.config/ghostty"),
            ("./Downloads: Operation not permitted", "./Downloads"),
        ];
        for (input, expect) in cases {
            assert_eq!(first(input), Some(expect), "{input:?}");
        }
        for input in ["input/output", "foo/bar", "$10/bar", "$10/$20", "$10/bar.txt", "foo/bar,baz.txt", "foo$BAR/baz.txt", "foo~/bar.txt", "// foo bar", "//foo"] {
            assert_eq!(first(input), None, "{input:?}");
        }
    }

    #[test]
    fn query_links_keep_their_full_target() {
        for url in ["https://example.com/?a=1&b=2", "https://localhost:17480/login?t=abc%3D%3D", "file:///C:/Windows/System32/winver.exe"] {
            let text = format!("Dashboard: {url}");
            let range = find(&text, text.find(url).unwrap()).unwrap();
            assert_eq!(&text[range.clone()], url);
            // winver.exe exists on Windows, where it opens as the file.
            let expected = match url.strip_prefix("file:///") {
                Some(path) if cfg!(windows) => Target::File(path.replace('/', "\\").into()),
                _ => Target::Url(url.into()),
            };
            assert_eq!(resolve(&text[range], None), Some(expected));
        }
    }

    #[test]
    fn finds_the_link_under_the_pointer() {
        let text = "see https://a.com and /tmp/x.txt.";
        assert_eq!(find(text, 0), None);
        assert_eq!(find(text, 4).map(|r| &text[r]), Some("https://a.com"));
        assert_eq!(find(text, 16).map(|r| &text[r]), Some("https://a.com"));
        assert_eq!(find(text, 17), None);
        assert_eq!(find(text, 25).map(|r| &text[r]), Some("/tmp/x.txt."));
    }

    #[test]
    fn extracts_address_words_at_the_pointer() {
        for (text, expected) in [
            ("<t1hg>", "t1hg"),
            ("[\"macbook/t2w\"]", "macbook/t2w"),
            ("terminal=t1hg,", "t1hg"),
            ("'my-machine_2/t2w'.", "my-machine_2/t2w"),
            ("before t1hg after", "t1hg"),
            ("é t1hg", "t1hg"),
            ("<build.box/t1>.", "build.box/t1"),
            ("1.2.3.4/t1", "1.2.3.4/t1"),
            ("...t1hg.", "t1hg"),
        ] {
            let at = text.find(expected).unwrap();
            for (i, _) in expected.char_indices() {
                assert_eq!(word(text, at + i).map(|r| &text[r]), Some(expected), "{text}");
            }
        }
        for (text, at) in [("<t1>", 0), ("t1 ", 2), ("", 0), ("t1", 2), ("é", 1)] {
            assert_eq!(word(text, at), None);
        }
    }

    #[test]
    fn matches_only_live_addresses_relative_to_the_source_machine() {
        let live = ["t1", "t2", "macbook/t1", "macbook/t2w", "my-machine_2/t3", "build.box/t1", "box/t1"];
        for (word, source, expected) in [
            ("t1", "t2", Some("t1")),
            ("t1", "macbook/t2w", Some("macbook/t1")),
            ("t1", "build.box/t2", Some("build.box/t1")),
            ("t2", "macbook/t1", None),
            ("macbook/t2w", "t1", Some("macbook/t2w")),
            ("my-machine_2/t3", "macbook/t1", Some("my-machine_2/t3")),
            ("t99", "t1", None),
            ("Build", "t1", None),
            ("t", "t1", None),
            ("other/t1", "t1", None),
            ("macbook/t1/extra", "t1", None),
        ] {
            assert_eq!(terminal_address(word, source, live), expected.map(str::to_owned), "{word} in {source}");
        }
    }

    #[test]
    fn dotted_machines_never_match_a_different_suffix_machine() {
        let text = "<build.box/t1>.";
        let range = word(text, text.find("t1").unwrap()).unwrap();
        assert_eq!(&text[range.clone()], "build.box/t1");
        assert_eq!(terminal_address(&text[range.clone()], "t2", ["box/t1"]), None);
        assert_eq!(terminal_address(&text[range], "t2", ["box/t1", "build.box/t1"]), Some("build.box/t1".into()));
    }

    #[test]
    fn files_resolve_only_when_they_exist() {
        let dir = std::env::temp_dir().join(format!("herdr-links-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        let file = Target::File(dir.join("src/main.rs"));
        assert_eq!(resolve("src/main.rs", Some(&dir)), Some(file.clone()));
        assert_eq!(resolve("src/main.rs:42:7", Some(&dir)), Some(file.clone()));
        assert_eq!(resolve("src/main.rs.", Some(&dir)), Some(file.clone()));
        assert_eq!(resolve("src/.", Some(&dir)), Some(Target::File(dir.join("src/."))));
        assert_eq!(resolve("src/..", Some(&dir)), Some(Target::File(dir.join("src/.."))));
        assert_eq!(resolve("$PWD/src/main.rs", Some(&dir)), Some(file.clone()));
        assert_eq!(resolve(dir.join("src/main.rs").to_str().unwrap(), None), Some(file));
        assert_eq!(resolve("src/gone.rs", Some(&dir)), None);
        assert_eq!(resolve("src/main.rs", None), None);
        assert_eq!(resolve("https://a.com/x", None), Some(Target::Url("https://a.com/x".into())));
        #[cfg(unix)]
        {
            let dotted = dir.join("src/main.rs.");
            std::fs::write(&dotted, "").unwrap();
            assert_eq!(resolve("src/main.rs.", Some(&dir)), Some(Target::File(dotted)));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_urls_open_as_the_file_they_name() {
        let dir = std::env::temp_dir().join(format!("herdr-file-urls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("测试 #1.txt");
        std::fs::write(&path, "").unwrap();
        // As programs write them: every byte outside the unreserved set encoded.
        let slashed = path.to_str().unwrap().replace('\\', "/");
        let encoded: String = slashed
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        let root = if cfg!(windows) { "/" } else { "" };
        let file = Some(Target::File(path.clone()));
        assert_eq!(resolve(&format!("file://{root}{encoded}"), None), file);
        assert_eq!(resolve(&format!("file://localhost{root}{encoded}"), None), file);
        assert_eq!(resolve(&format!("file://{root}{}", slashed.replace('#', "%23")), None), file);
        for url in [format!("file://{root}{encoded}.gone"), format!("file://{root}{encoded}%ZZ"), format!("file://box{root}{encoded}")] {
            assert_eq!(resolve(&url, None), Some(Target::Url(url.clone())), "{url}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolve_with_location_preserves_line_ranges_and_remote_paths() {
        let dir = std::env::temp_dir().join(format!("herdr-links-loc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let path = dir.join("src/app.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        assert_eq!(resolve_with_location("src/app.rs:42", Some(&dir)), Some((Target::File(path.clone()), Some(42), None)));
        assert_eq!(resolve_with_location("src/app.rs:42:7", Some(&dir)), Some((Target::File(path.clone()), Some(42), None)));
        assert_eq!(resolve_with_location("src/app.rs:10-25", Some(&dir)), Some((Target::File(path.clone()), Some(10), Some(25))));
        assert_eq!(resolve_with_location("src/app.rs#L12-L20", Some(&dir)), Some((Target::File(path.clone()), Some(12), Some(20))));
        let root = if cfg!(windows) { "/" } else { "" };
        let url = format!("file://{root}{}#L5-L9", path.to_str().unwrap().replace('\\', "/"));
        assert_eq!(resolve_with_location(&url, None), Some((Target::File(path.clone()), Some(5), Some(9))));
        assert_eq!(
            resolve_remote_with_location("src/foo.rs:18:2", Some("/srv/repo")),
            Some((Target::File(PathBuf::from("/srv/repo/src/foo.rs")), Some(18), None)),
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cli_agent_file_links_handle_cjk_bare_files_spaces_roots_and_remote_urls() {
        let dir = std::env::temp_dir().join(format!("herdr-links-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/nested")).unwrap();
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::create_dir_all(dir.join("test folder")).unwrap();
        let app_rs = dir.join("sub/app.rs");
        let cargo_toml = dir.join("Cargo.toml");
        let extless = dir.join("scripts/install-linux");
        let spaced = dir.join("test folder/file.txt");
        let makefile = dir.join("Makefile");
        std::fs::write(&app_rs, "fn main() {}\n").unwrap();
        std::fs::write(&cargo_toml, "[package]\n").unwrap();
        std::fs::write(&extless, "#!/bin/sh\n").unwrap();
        std::fs::write(&spaced, "ok\n").unwrap();
        std::fs::write(&makefile, "all:\n").unwrap();

        let roots = [dir.join("sub/nested"), dir.clone()];
        let root_refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();

        // 1. CJK immediately before and after paths (very common in Chinese CLI agent output).
        let cjk_rel = "请查看sub/app.rs中的改动";
        let (r, t, l, e) = find_and_resolve_local(cjk_rel, cjk_rel.find("app.rs").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&cjk_rel[r], "sub/app.rs");
        assert_eq!((t, l, e), (Target::File(app_rs.clone()), None, None));

        let cjk_line = "修改了sub/app.rs:42行";
        let (r, t, l, e) = find_and_resolve_local(cjk_line, cjk_line.find("sub/").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&cjk_line[r], "sub/app.rs:42");
        assert_eq!((t, l, e), (Target::File(app_rs.clone()), Some(42), None));

        let abs_cjk = format!("打开{}即可", app_rs.to_str().unwrap());
        let (r, t, _, _) = find_and_resolve_local(&abs_cjk, abs_cjk.find("app.rs").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&abs_cjk[r], app_rs.to_str().unwrap());
        assert_eq!(t, Target::File(app_rs.clone()));

        // 2. Single-component filename, extensionless relative path, git diff prefix, parenthesized line.
        let single = "查看Cargo.toml配置";
        let (r, t, _, _) = find_and_resolve_local(single, single.find("Cargo").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&single[r], "Cargo.toml");
        assert_eq!(t, Target::File(cargo_toml.clone()));

        let no_ext = "Run scripts/install-linux now";
        let (r, t, _, _) = find_and_resolve_local(no_ext, no_ext.find("scripts").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&no_ext[r], "scripts/install-linux");
        assert_eq!(t, Target::File(extless.clone()));

        let diff_line = "+++ b/sub/app.rs";
        let (_, t, _, _) = find_and_resolve_local(diff_line, diff_line.find("app.rs").unwrap(), &root_refs, false).unwrap();
        assert_eq!(t, Target::File(app_rs.clone()));

        let parens = "error at sub/app.rs(17,4)";
        let (_, t, l, _) = find_and_resolve_local(parens, parens.find("app.rs").unwrap(), &root_refs, false).unwrap();
        assert_eq!((t, l), (Target::File(app_rs.clone()), Some(17)));

        // 3. Rooted paths followed by prose or a second path must not gobble the rest of the line,
        // while real paths with spaces still resolve.
        let two_paths = format!("{} sub/app.rs", makefile.to_str().unwrap());
        let (r1, t1, _, _) = find_and_resolve_local(&two_paths, two_paths.find("Makefile").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&two_paths[r1], makefile.to_str().unwrap());
        assert_eq!(t1, Target::File(makefile.clone()));
        let (r2, t2, _, _) = find_and_resolve_local(&two_paths, two_paths.find("sub/app.rs").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&two_paths[r2], "sub/app.rs");
        assert_eq!(t2, Target::File(app_rs.clone()));

        let with_prose = format!("Check {} for details", makefile.to_str().unwrap());
        let (rp, tp, _, _) = find_and_resolve_local(&with_prose, with_prose.find("Makefile").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&with_prose[rp], makefile.to_str().unwrap());
        assert_eq!(tp, Target::File(makefile.clone()));

        let spaced_line = format!("See {} ok", spaced.to_str().unwrap());
        let (rs, ts, _, _) = find_and_resolve_local(&spaced_line, spaced_line.find("file.txt").unwrap(), &root_refs, false).unwrap();
        assert_eq!(&spaced_line[rs], spaced.to_str().unwrap());
        assert_eq!(ts, Target::File(spaced.clone()));

        // 4. Remote SSH/WSL paths: strip space-gobbled prose, trailing CJK, and remote file:// hostnames.
        let remote_prose = "Check /srv/repo/Makefile for details";
        let (rr, tr, _, _) = find_and_resolve_remote(remote_prose, remote_prose.find("Makefile").unwrap(), Some("/srv/repo"), false).unwrap();
        assert_eq!(&remote_prose[rr], "/srv/repo/Makefile");
        assert_eq!(tr, Target::File(PathBuf::from("/srv/repo/Makefile")));

        let remote_cjk = "修改了src/foo.rs:18行";
        let (rc, tc, lc, _) = find_and_resolve_remote(remote_cjk, remote_cjk.find("foo.rs").unwrap(), Some("/srv/repo"), false).unwrap();
        assert_eq!(&remote_cjk[rc], "src/foo.rs:18");
        assert_eq!((tc, lc), (Target::File(PathBuf::from("/srv/repo/src/foo.rs")), Some(18)));

        let remote_url = "file://dev-box/srv/repo/src/foo.rs#L10-L20";
        assert_eq!(
            resolve_remote_with_location(remote_url, Some("/srv/repo")),
            Some((Target::File(PathBuf::from("/srv/repo/src/foo.rs")), Some(10), Some(20))),
        );

        let wrapped_remote = "/home/user/repo/herdr-app/defaults/mods/files/main.js:42";
        let (rw, tw, lw, _) =
            find_and_resolve_remote(wrapped_remote, wrapped_remote.find("main.js").unwrap(), Some("/home/user/repo"), true).unwrap();
        assert_eq!(&wrapped_remote[rw], wrapped_remote);
        assert_eq!((tw, lw), (Target::File(PathBuf::from("/home/user/repo/herdr-app/defaults/mods/files/main.js")), Some(42)));

        std::fs::remove_dir_all(dir).unwrap();
    }
}
