// HERDR: built-in pure-Rust syntax highlighting for script editors, adapted from Base's showcase.
use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, LazyLock, Mutex},
};

use gpui::{Context, FontStyle, FontWeight, HighlightStyle, SharedString, Task, Window};
use gpui_base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, Rope,
};
use syntect::{
    parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

/// Loading the default syntax definitions deserializes a few megabytes, so share
/// one set across every highlighter instance.
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

const SYNC_HIGHLIGHT_BYTES: usize = 16 * 1024;
const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;
const MAX_HIGHLIGHT_LINE_BYTES: usize = 2048;

pub(crate) fn warm_syntax() {
    let _ = &*SYNTAX_SET;
}

#[derive(Default)]
struct HighlightState {
    revision: u64,
    highlights: Vec<(Range<usize>, &'static str)>,
    fold_ranges: Vec<FoldRange>,
}

/// A small, WASM-compatible example adapter for Base's parser-independent
/// highlighting API. Syntect identifies scopes; the application-owned resolver
/// supplies all colors and font styles.
pub(crate) struct SyntectHighlighter {
    language: SharedString,
    revision: u64,
    state: Arc<Mutex<HighlightState>>,
    /// Scope ids are cheap to compare but expensive to stringify, so remember
    /// the semantic name each one maps to.
    semantic_names: HashMap<Scope, Option<&'static str>>,
    pending_task: Option<Task<()>>,
}

impl SyntectHighlighter {
    pub(crate) fn new(language: &str) -> Option<Self> {
        find_syntax(language)?;

        Some(Self {
            language: language.to_owned().into(),
            revision: 0,
            state: Arc::new(Mutex::new(HighlightState::default())),
            semantic_names: HashMap::new(),
            pending_task: None,
        })
    }
}

fn slice_prefix_at_line_end(text: &str, max_bytes: usize, round_up: bool) -> usize {
    if max_bytes >= text.len() {
        return text.len();
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    if round_up {
        if let Some(rel) = text[boundary..].find('\n') {
            boundary + rel + 1
        } else {
            text.len()
        }
    } else {
        text[..boundary].rfind('\n').map_or(boundary, |i| i + 1)
    }
}

fn syntax_alias(language: &str) -> Option<&'static str> {
    match language {
        "ts" | "tsx" | "mts" | "cts" | "jsx" | "mjs" | "cjs" | "typescript" | "javascript"
        | "graphql" | "gql" => Some("js"),
        "toml" | "ini" | "cfg" | "conf" | "env" | "dotenv" | "properties" => Some("yaml"),
        "jsonc" | "jsonl" | "geojson" => Some("json"),
        "mdx" | "markdown" | "mdown" | "mkd" => Some("md"),
        "vue" | "svelte" | "astro" | "svg" | "xhtml" => Some("html"),
        "scss" | "sass" | "less" | "pcss" => Some("css"),
        "kt" | "kts" | "kotlin" => Some("scala"),
        "swift" | "zig" | "wgsl" => Some("rs"),
        "proto" | "protobuf" | "ino" | "cuda" | "cu" => Some("cpp"),
        "ps1" | "psm1" | "psd1" | "zsh" | "bash" | "fish" | "dockerfile" | "containerfile"
        | "justfile" => Some("sh"),
        "cmake" | "mk" | "gnumakefile" => Some("makefile"),
        _ => None,
    }
}

fn find_syntax(language: &str) -> Option<&'static SyntaxReference> {
    let trimmed = language.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    SYNTAX_SET
        .find_syntax_by_token(trimmed)
        .or_else(|| SYNTAX_SET.find_syntax_by_extension(trimmed))
        .or_else(|| SYNTAX_SET.find_syntax_by_token(&lower))
        .or_else(|| SYNTAX_SET.find_syntax_by_extension(&lower))
        .or_else(|| {
            syntax_alias(&lower).and_then(|alias| {
                SYNTAX_SET
                    .find_syntax_by_token(alias)
                    .or_else(|| SYNTAX_SET.find_syntax_by_extension(alias))
            })
        })
}

fn collect_highlights(
    syntax: &SyntaxReference,
    text: &str,
    semantic_names: &mut HashMap<Scope, Option<&'static str>>,
) -> Vec<(Range<usize>, &'static str)> {
    let mut parser = ParseState::new(syntax);
    let mut scopes = ScopeStack::new();
    let mut offset = 0;
    let mut highlights = Vec::new();

    for line in LinesWithEndings::from(text) {
        if line.len() > MAX_HIGHLIGHT_LINE_BYTES {
            offset += line.len();
            continue;
        }
        if let Ok(operations) = parser.parse_line(line, &SYNTAX_SET) {
            let mut cursor = 0;
            for (index, operation) in operations {
                if cursor < index {
                    let name = scopes.scopes.iter().rev().find_map(|scope| {
                        *semantic_names
                            .entry(*scope)
                            .or_insert_with(|| semantic_name(*scope))
                    });
                    if let Some(name) = name {
                        highlights.push((offset + cursor..offset + index, name));
                    }
                }
                let _ = scopes.apply(&operation);
                cursor = index;
            }
            if cursor < line.len() {
                let name = scopes.scopes.iter().rev().find_map(|scope| {
                    *semantic_names
                        .entry(*scope)
                        .or_insert_with(|| semantic_name(*scope))
                });
                if let Some(name) = name {
                    highlights.push((offset + cursor..offset + line.len(), name));
                }
            }
        }
        offset += line.len();
    }

    highlights
}

pub(crate) fn highlight_code(
    language: Option<&str>,
    text: &str,
    styles: &SyntaxStyles,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(lang) = language else {
        return Vec::new();
    };
    let Some(syntax) = find_syntax(lang) else {
        return Vec::new();
    };
    let mut semantic_names = HashMap::new();
    let raw = collect_highlights(syntax, text, &mut semantic_names);
    raw.into_iter()
        .filter_map(|(range, name)| styles.style(name).map(|style| (range, style)))
        .collect()
}

impl InputHighlighter for SyntectHighlighter {
    fn language(&self) -> SharedString {
        self.language.clone()
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        folding: bool,
        _window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        self.revision = self.revision.wrapping_add(1);
        let rev = self.revision;
        self.pending_task = None;

        // `syntect` has no incremental mode; highlight the initial viewport synchronously
        // and offload larger documents so file switching never blocks the UI thread.
        let full_text = text.to_string();
        let sync_limit = if edit.is_some() { 32 * 1024 } else { SYNC_HIGHLIGHT_BYTES };
        let syntax = find_syntax(self.language.as_ref())
            .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

        if full_text.len() <= sync_limit {
            let highlights = collect_highlights(syntax, &full_text, &mut self.semantic_names);
            let fold_ranges = if folding {
                brace_fold_ranges(&full_text)
            } else {
                Vec::new()
            };
            let mut guard = self.state.lock().unwrap();
            guard.revision = rev;
            guard.highlights = highlights;
            guard.fold_ranges = fold_ranges;
            return;
        }

        let head_end = slice_prefix_at_line_end(&full_text, SYNC_HIGHLIGHT_BYTES, true);
        let head_highlights = collect_highlights(syntax, &full_text[..head_end], &mut self.semantic_names);
        {
            let mut guard = self.state.lock().unwrap();
            guard.revision = rev;
            guard.highlights = head_highlights;
            guard.fold_ranges.clear();
        }

        let state_arc = self.state.clone();
        let lang = self.language.clone();
        let bg = cx.background_executor().spawn(async move {
            let syntax = find_syntax(lang.as_ref())
                .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
            let mut semantic_names = HashMap::new();
            let cap = slice_prefix_at_line_end(&full_text, MAX_HIGHLIGHT_BYTES, false);
            let highlights = collect_highlights(syntax, &full_text[..cap], &mut semantic_names);
            let fold_ranges = if folding {
                brace_fold_ranges(&full_text[..cap])
            } else {
                Vec::new()
            };
            (highlights, fold_ranges)
        });
        self.pending_task = Some(cx.spawn(async move |editor, cx| {
            let (highlights, fold_ranges) = bg.await;
            let _ = editor.update(cx, |_, cx| {
                let mut guard = state_arc.lock().unwrap();
                if guard.revision == rev {
                    guard.highlights = highlights;
                    guard.fold_ranges = fold_ranges;
                    drop(guard);
                    cx.notify();
                }
            });
        }));
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let guard = self.state.lock().unwrap();
        resolve_styles(&guard.highlights, range, resolver)
    }

    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        self.state.lock().unwrap().fold_ranges.clone()
    }

    fn fold_ranges_for_edit(&self, _: Range<usize>, _: &Rope) -> Vec<FoldRange> {
        self.state.lock().unwrap().fold_ranges.clone()
    }
}

/// Turn the highlights overlapping `range` into gap-free style runs.
///
/// `highlights` is ordered and non-overlapping, so the first candidate is found
/// by binary search instead of scanning the whole document on every frame.
fn resolve_styles(
    highlights: &[(Range<usize>, &'static str)],
    range: &Range<usize>,
    resolver: &dyn HighlightStyleResolver,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let first = highlights.partition_point(|(highlight, _)| highlight.end <= range.start);
    let mut runs = Vec::new();
    let mut cursor = range.start;

    for (highlight_range, name) in &highlights[first..] {
        if highlight_range.start >= range.end {
            break;
        }

        let start = highlight_range.start.max(range.start);
        let end = highlight_range.end.min(range.end);
        if start >= end || end <= cursor {
            continue;
        }
        if cursor < start {
            runs.push((cursor..start, HighlightStyle::default()));
        }
        runs.push((start..end, resolver.style(name).unwrap_or_default()));
        cursor = end;
    }

    if cursor < range.end {
        runs.push((cursor..range.end, HighlightStyle::default()));
    }
    runs
}

fn semantic_name(scope: Scope) -> Option<&'static str> {
    let scope = scope.build_string();
    let name = if scope.starts_with("comment") {
        "comment"
    } else if scope.starts_with("constant.character.escape") {
        "string.escape"
    } else if scope.starts_with("string.regexp") {
        "regexp"
    } else if scope.starts_with("support.type.property-name")
        || scope.starts_with("meta.object-literal.key")
        || scope.starts_with("entity.name.tag.yaml")
        || scope.starts_with("variable.other.member")
        || scope.starts_with("variable.other.property")
        || scope.starts_with("variable.other.readwrite.member")
    {
        "property"
    } else if scope.starts_with("string") {
        "string"
    } else if scope.starts_with("constant.numeric") {
        "number"
    } else if scope.starts_with("constant.language.boolean") {
        "boolean"
    } else if scope.starts_with("constant") || scope.starts_with("support.constant") {
        "constant"
    } else if scope.starts_with("entity.name.tag") {
        "tag"
    } else if scope.starts_with("entity.other.attribute-name") {
        "attribute"
    } else if scope.starts_with("entity.name.function.macro")
        || scope.starts_with("support.macro")
        || scope.starts_with("meta.preprocessor")
        || scope.starts_with("meta.annotation")
        || scope.starts_with("storage.type.annotation")
    {
        "macro"
    } else if scope.starts_with("entity.name.function") || scope.starts_with("support.function") {
        "function"
    } else if scope.starts_with("entity.name.type")
        || scope.starts_with("entity.name.class")
        || scope.starts_with("entity.name.struct")
        || scope.starts_with("entity.name.enum")
        || scope.starts_with("entity.name.trait")
        || scope.starts_with("entity.name.interface")
        || scope.starts_with("support.type")
        || scope.starts_with("support.class")
        || scope.starts_with("storage.type.primitive")
    {
        "type"
    } else if scope.starts_with("entity.name.namespace") || scope.starts_with("entity.name.module")
    {
        "namespace"
    } else if scope.starts_with("variable.parameter") {
        "parameter"
    } else if scope.starts_with("keyword.operator") || scope.starts_with("punctuation.accessor") {
        "operator"
    } else if scope.starts_with("keyword") || scope.starts_with("storage") {
        "keyword"
    } else if scope.starts_with("variable") {
        "variable"
    } else if scope.starts_with("markup.heading") || scope.starts_with("entity.name.section") {
        "heading"
    } else if scope.starts_with("punctuation") {
        "punctuation"
    } else {
        return None;
    };
    Some(name)
}

fn brace_fold_ranges(text: &str) -> Vec<FoldRange> {
    let mut starts = Vec::new();
    let mut ranges = Vec::new();

    for (line_number, line) in text.lines().enumerate() {
        let mut chars = line.chars().peekable();
        let mut quoted = false;
        let mut escaped = false;
        while let Some(character) = chars.next() {
            if !quoted && character == '/' && chars.peek() == Some(&'/') {
                break;
            }
            if character == '"' && !escaped {
                quoted = !quoted;
            } else if !quoted && character == '{' {
                starts.push(line_number);
            } else if !quoted && character == '}' {
                if let Some(start_line) = starts.pop() {
                    if start_line < line_number {
                        ranges.push(FoldRange::new(start_line, line_number));
                    }
                }
            }
            escaped = quoted && character == '\\' && !escaped;
            if character != '\\' {
                escaped = false;
            }
        }
    }
    ranges
}

fn fallback_semantic_key(name: &str) -> &[&'static str] {
    match name {
        "regexp" => &["string"],
        "property" => &["variable", "function"],
        "tag" => &["keyword"],
        "attribute" => &["type"],
        "macro" => &["function"],
        "namespace" => &["type"],
        "parameter" => &["variable"],
        "heading" => &["keyword"],
        _ => &[],
    }
}

/// HERDR: semantic overrides fall back to readable, dark-editor defaults.
#[derive(Default)]
pub(crate) struct SyntaxStyles(pub std::collections::HashMap<String, gpui::Hsla>);

impl HighlightStyleResolver for SyntaxStyles {
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        let root = name.split('.').next()?;
        let mut color = self.0.get(name).or_else(|| self.0.get(root)).copied();
        if color.is_none() {
            for fallback in fallback_semantic_key(root) {
                if let Some(found) = self.0.get(*fallback).copied() {
                    color = Some(found);
                    break;
                }
            }
        }
        let color = color.or_else(|| {
            let rgb = match root {
                "comment" => 0x7f848e,
                "string" | "regexp" => 0x98c379,
                "number" => 0xd19a66,
                "keyword" | "tag" | "heading" => 0xc678dd,
                "boolean" | "constant" => 0xe5c07b,
                "function" | "macro" => 0x61afef,
                "type" | "namespace" | "attribute" => 0xe5c07b,
                "operator" => 0x56b6c2,
                // Unclassified identifiers and punctuation follow the theme foreground.
                _ => return None,
            };
            Some(gpui::rgb(rgb).into())
        });
        color.map(|color| HighlightStyle {
            color: Some(color),
            font_style: (root == "comment").then_some(FontStyle::Italic),
            font_weight: (root == "heading").then_some(FontWeight::BOLD),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_styles_cover_the_requested_range_without_gaps() {
        let runs = resolve_styles(
            &[(2..4, "keyword"), (6..8, "string")],
            &(0..10),
            &SyntaxStyles::default(),
        );
        let ranges: Vec<_> = runs.into_iter().map(|(range, _)| range).collect();
        assert_eq!(ranges, vec![0..2, 2..4, 4..6, 6..8, 8..10]);
    }

    #[test]
    fn semantic_overrides_and_nested_names_resolve() {
        let color = gpui::rgb(0xc678dd).into();
        let var_color = gpui::rgb(0xe06c75).into();
        let styles = SyntaxStyles(HashMap::from([
            ("string".into(), color),
            ("variable".into(), var_color),
        ]));
        assert_eq!(styles.style("string.escape").unwrap().color, Some(color));
        assert_eq!(styles.style("property").unwrap().color, Some(var_color));
        assert_eq!(styles.style("comment").unwrap().font_style, Some(FontStyle::Italic));
        assert_eq!(styles.style("heading").unwrap().font_weight, Some(FontWeight::BOLD));
        assert!(styles.style("keyword").is_some());
        assert!(find_syntax("rust").is_some());
        assert!(find_syntax("rs").is_some());
        assert!(find_syntax("tsx").is_some());
        assert!(find_syntax("toml").is_some());
        assert!(find_syntax("dockerfile").is_some());
        assert!(find_syntax("unknown-language").is_none());
    }

    #[test]
    fn highlight_code_produces_non_empty_runs_for_supported_languages() {
        let styles = SyntaxStyles::default();
        let runs = highlight_code(Some("rs"), "fn main() { let x = \"hi\"; }", &styles);
        assert!(!runs.is_empty());
        assert!(highlight_code(Some("tsx"), "const n: number = 1;", &styles).len() > 1);
        assert!(highlight_code(None, "plain", &styles).is_empty());
        assert!(highlight_code(Some("unknown-language"), "plain", &styles).is_empty());
    }

    #[test]
    fn skips_long_lines_and_slices_on_utf8_boundaries() {
        let long_line = format!("let x = \"{}\";\nfn after_long() {{}}\n", "a".repeat(MAX_HIGHLIGHT_LINE_BYTES + 128));
        let mut names = HashMap::new();
        let syntax = find_syntax("rs").unwrap();
        let highlights = collect_highlights(syntax, &long_line, &mut names);
        // Only the second line should be highlighted; the oversized first line is skipped cleanly.
        assert!(highlights.iter().all(|(r, _)| r.start > MAX_HIGHLIGHT_LINE_BYTES));
        assert!(!highlights.is_empty());

        let utf8_text = format!("{}中文内容\nnext line\n", "a".repeat(SYNC_HIGHLIGHT_BYTES - 1));
        let split_up = slice_prefix_at_line_end(&utf8_text, SYNC_HIGHLIGHT_BYTES, true);
        let split_down = slice_prefix_at_line_end(&utf8_text, SYNC_HIGHLIGHT_BYTES, false);
        assert!(utf8_text.is_char_boundary(split_up));
        assert!(utf8_text.is_char_boundary(split_down));
        assert!(utf8_text[..split_up].ends_with("中文内容\n"));
    }
}
