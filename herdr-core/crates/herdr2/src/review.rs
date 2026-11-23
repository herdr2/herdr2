//! Deterministic compiler for line-anchored file/diff notes & messages delivered to ACP or PTY agents.

use crate::api::ReviewComment;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Maximum inline Markdown payload sent directly into an ACP `session/prompt` turn
/// before staging a `line-notes.md` file beside the agent.
pub const DEFAULT_REVIEW_INLINE_MAX_BYTES: usize = 24 * 1024;

/// Maximum number of inline review comments accepted in one `agent.review` call.
pub const MAX_REVIEW_COMMENTS: usize = 200;

/// Maximum UTF-8 byte length kept for each comment's quoted diff snippet.
const MAX_SNIPPET_BYTES: usize = 2048;

pub struct ReviewBuildInput<'a> {
    pub source: Option<&'a str>,
    pub checkpoint_turn: Option<u64>,
    pub repo_root: Option<&'a str>,
    pub note: Option<&'a str>,
    pub comments: &'a [ReviewComment],
    pub is_zh: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewPackage {
    pub markdown: String,
    pub files: Vec<String>,
    pub file_count: usize,
    pub comment_count: usize,
    pub bytes: usize,
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("\n… (truncated)");
    out
}

fn code_fence_for(text: &str) -> String {
    let mut longest = 2usize;
    let mut run = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            run += 1;
            if run > longest {
                longest = run;
            }
        } else {
            run = 0;
        }
    }
    "`".repeat(longest + 1)
}

fn format_line_anchor(c: &ReviewComment, is_zh: bool) -> String {
    let start = c.start_line;
    let end = c.end_line.unwrap_or(start).max(start);
    let range = if end > start { format!("L{start}-L{end}") } else { format!("L{start}") };
    match c.side.trim().to_ascii_lowercase().as_str() {
        "old" | "deleted" | "-" => {
            if is_zh {
                format!("{range}（删除侧）")
            } else {
                format!("{range} (old/deleted side)")
            }
        }
        "context" | " " => {
            if is_zh {
                format!("{range}（上下文）")
            } else {
                format!("{range} (context)")
            }
        }
        _ => range,
    }
}

/// Compiles structured inline diff comments into a deterministic Markdown review package.
pub fn build_review_markdown(input: &ReviewBuildInput<'_>) -> ReviewPackage {
    let mut by_file: BTreeMap<String, Vec<&ReviewComment>> = BTreeMap::new();
    for c in input.comments {
        let path = c.path.trim().to_string();
        by_file.entry(path).or_default().push(c);
    }
    for group in by_file.values_mut() {
        group.sort_by_key(|c| (c.start_line, c.end_line.unwrap_or(c.start_line)));
    }

    let files: Vec<String> = by_file.keys().cloned().collect();
    let file_count = files.len();
    let comment_count = input.comments.len();
    let is_zh = input.is_zh;

    let mut md = String::with_capacity(1024);
    if is_zh {
        let _ = writeln!(md, "## 文件行级消息（{comment_count} 条，涉及 {file_count} 个文件）");
    } else {
        let n_word = if comment_count == 1 { "note" } else { "notes" };
        let f_word = if file_count == 1 { "file" } else { "files" };
        let _ = writeln!(md, "## File Line Notes ({comment_count} {n_word} across {file_count} {f_word})");
    }

    let mut meta_parts: Vec<String> = Vec::new();
    if let Some(turn) = input.checkpoint_turn {
        meta_parts.push(if is_zh { format!("来源：快照 T{turn}") } else { format!("Source: Checkpoint T{turn}") });
    } else if let Some(src) = input.source.map(str::trim).filter(|s| !s.is_empty()) {
        let label = match src {
            "changes" => {
                if is_zh {
                    "来源：未提交工作区变更 (Changes)"
                } else {
                    "Source: Uncommitted workspace changes"
                }
            }
            "checkpoints" => {
                if is_zh {
                    "来源：快照差异 (Checkpoints)"
                } else {
                    "Source: Checkpoint diff"
                }
            }
            "files" => {
                if is_zh {
                    "来源：工作区文本文件 (Files)"
                } else {
                    "Source: Workspace files"
                }
            }
            other => other,
        };
        meta_parts.push(label.to_string());
    }
    if let Some(root) = input.repo_root.map(str::trim).filter(|s| !s.is_empty()) {
        meta_parts.push(if is_zh { format!("仓库：`{root}`") } else { format!("Repo: `{root}`") });
    }
    if !meta_parts.is_empty() {
        let _ = writeln!(md, "\n{}", meta_parts.join(" · "));
    }

    if let Some(note) = input.note.map(str::trim).filter(|s| !s.is_empty()) {
        if is_zh {
            let _ = writeln!(md, "\n### 总体说明\n{note}");
        } else {
            let _ = writeln!(md, "\n### Overall Note\n{note}");
        }
    }

    let mut idx = 0usize;
    for (file, group) in &by_file {
        let _ = writeln!(md, "\n### `{file}`");
        for c in group {
            idx += 1;
            let anchor = format_line_anchor(c, is_zh);
            let hunk = c
                .hunk_header
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|h| format!(" `{h}`"))
                .unwrap_or_default();
            let _ = writeln!(md, "\n#### {idx}. `{file}:{anchor}`{hunk}");

            if let Some(raw_snip) =
                c.snippet.as_deref().map(|s| s.trim_end_matches('\n')).filter(|s| !s.trim().is_empty())
            {
                let bounded = truncate_utf8(raw_snip, MAX_SNIPPET_BYTES);
                let fence = code_fence_for(&bounded);
                let is_file_snip = c.side.eq_ignore_ascii_case("file")
                    || input.source.is_some_and(|s| s.eq_ignore_ascii_case("files"));
                let lang = if is_file_snip { "" } else { "diff" };
                let _ = writeln!(md, "{fence}{lang}\n{bounded}\n{fence}");
            }

            let body = c.body.trim();
            if is_zh {
                let _ = writeln!(md, "**用户消息：** {body}");
            } else {
                let _ = writeln!(md, "**Message:** {body}");
            }
        }
    }

    if is_zh {
        let _ = writeln!(
            md,
            "\n---\n请结合上述文件行级上下文，按每条用户消息的实际诉求（如解答疑问、分析讨论、修改代码或审查建议等）逐条回应或处理。"
        );
    } else {
        let _ = writeln!(
            md,
            "\n---\nPlease respond to or act on each message above based on the attached file line context (e.g. answer questions, explain, discuss, or edit code as requested)."
        );
    }

    let bytes = md.len();
    ReviewPackage { markdown: md, files, file_count, comment_count, bytes }
}

fn summarize_file_list(files: &[String], max_shown: usize) -> String {
    if files.len() <= max_shown {
        return files.join(", ");
    }
    let head = files[..max_shown].join(", ");
    format!("{head}, +{}", files.len() - max_shown)
}

/// Builds a compact, TTY-safe pointer prompt for PTY terminals (or oversized ACP payloads)
/// pointing the agent to the staged `line-notes*.md` file.
pub fn build_staged_pointer_message(
    pkg: &ReviewPackage,
    staged_path: &str,
    note: Option<&str>,
    checkpoint_turn: Option<u64>,
    is_zh: bool,
) -> String {
    let files_summary = summarize_file_list(&pkg.files, 3);
    let turn_tag = checkpoint_turn
        .map(|t| if is_zh { format!("（快照 T{t}）") } else { format!(" (Checkpoint T{t})") })
        .unwrap_or_default();
    let mut msg = if is_zh {
        format!(
            "[文件行级消息{turn_tag}：{} 条，涉及 {} 个文件（{files_summary}）]\n完整内容已保存至：{staged_path}",
            pkg.comment_count, pkg.file_count
        )
    } else {
        let n_word = if pkg.comment_count == 1 { "note" } else { "notes" };
        let f_word = if pkg.file_count == 1 { "file" } else { "files" };
        format!(
            "[File Line Notes{turn_tag}: {} {n_word} across {} {f_word} ({files_summary})]\nFull context saved at: {staged_path}",
            pkg.comment_count, pkg.file_count
        )
    };
    if let Some(n) = note.map(str::trim).filter(|s| !s.is_empty()) {
        // Keep PTY pointer single-line safe by collapsing newlines in the preview note.
        let one_line: String = n.split_whitespace().collect::<Vec<_>>().join(" ");
        let bounded = truncate_utf8(&one_line, 240);
        if is_zh {
            let _ = write!(msg, "\n补充说明：{bounded}");
        } else {
            let _ = write!(msg, "\nNote: {bounded}");
        }
    }
    if is_zh {
        msg.push_str("\n请先读取该文件，再按其中的用户消息逐条回应或处理。");
    } else {
        msg.push_str("\nPlease read that file first and respond to or act on each message accordingly.");
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_comment(path: &str, start: u32, end: Option<u32>, side: &str, body: &str) -> ReviewComment {
        ReviewComment {
            path: path.into(),
            side: side.into(),
            start_line: start,
            end_line: end,
            hunk_header: Some("@@ -10,4 +10,6 @@".into()),
            snippet: Some("-old()\n+new()".into()),
            body: body.into(),
        }
    }

    #[test]
    fn groups_by_file_sorts_by_line_and_formats_en() {
        let comments = vec![
            sample_comment("src/b.rs", 20, Some(25), "new", "Avoid cloning here"),
            sample_comment("src/a.rs", 12, None, "old", "Why was this guard removed?"),
            sample_comment("src/b.rs", 5, None, "new", "Check error path"),
        ];
        let pkg = build_review_markdown(&ReviewBuildInput {
            source: Some("changes"),
            checkpoint_turn: None,
            repo_root: Some("/repo"),
            note: Some("Focus on error handling"),
            comments: &comments,
            is_zh: false,
        });
        assert_eq!(pkg.file_count, 2);
        assert_eq!(pkg.comment_count, 3);
        assert_eq!(pkg.files, vec!["src/a.rs", "src/b.rs"]);
        assert!(pkg.markdown.contains("## File Line Notes (3 notes across 2 files)"));
        assert!(pkg.markdown.contains("#### 1. `src/a.rs:L12 (old/deleted side)`"));
        assert!(pkg.markdown.contains("#### 2. `src/b.rs:L5`"));
        assert!(pkg.markdown.contains("#### 3. `src/b.rs:L20-L25`"));
        assert!(pkg.markdown.contains("Focus on error handling"));
    }

    #[test]
    fn escapes_nested_backticks_and_formats_zh_checkpoint() {
        let mut c = sample_comment("docs/readme.md", 8, Some(10), "new", "修正代码块语法");
        c.snippet = Some("+```rust\n+fn main() {}\n+```".into());
        let comments = vec![c];
        let pkg = build_review_markdown(&ReviewBuildInput {
            source: Some("checkpoints"),
            checkpoint_turn: Some(3),
            repo_root: None,
            note: None,
            comments: &comments,
            is_zh: true,
        });
        assert!(pkg.markdown.contains("## 文件行级消息（1 条，涉及 1 个文件）"));
        assert!(pkg.markdown.contains("来源：快照 T3"));
        assert!(pkg.markdown.contains("````diff"));
        assert!(pkg.markdown.contains("**用户消息：** 修正代码块语法"));

        let ptr = build_staged_pointer_message(&pkg, "/tmp/files/t1/line-notes.md", None, Some(3), true);
        assert!(ptr.contains("快照 T3"));
        assert!(ptr.contains("/tmp/files/t1/line-notes.md"));
        assert!(ptr.lines().count() <= 4);
    }

    #[test]
    fn truncates_multibyte_snippet_safely() {
        let long = "中".repeat(1200);
        let truncated = truncate_utf8(&long, 100);
        assert!(truncated.ends_with("… (truncated)"));
        assert!(truncated.len() <= 120);
    }

    #[test]
    fn formats_plain_file_review_without_diff_fence() {
        let comments = vec![ReviewComment {
            path: "docs/plan.md".into(),
            side: "file".into(),
            start_line: 12,
            end_line: Some(15),
            hunk_header: None,
            snippet: Some("12 | ## Architecture\n13 | Keep modules small.".into()),
            body: "补充容错策略说明".into(),
        }];
        let pkg = build_review_markdown(&ReviewBuildInput {
            source: Some("files"),
            checkpoint_turn: None,
            repo_root: Some("/repo"),
            note: None,
            comments: &comments,
            is_zh: true,
        });
        assert!(pkg.markdown.contains("来源：工作区文本文件 (Files)"));
        assert!(pkg.markdown.contains("#### 1. `docs/plan.md:L12-L15`"));
        assert!(pkg.markdown.contains("```\n12 | ## Architecture"));
        assert!(!pkg.markdown.contains("```diff"));
    }
}
