//! Self-contained YAML subset + JSON parser (no external yaml crate in Cargo.lock).

use serde_json::{Map, Number, Value};

/// Parses either JSON or a standard YAML plan/config document into `serde_json::Value`.
pub fn parse_yaml_or_json(input: &str) -> Result<Value, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(val) = serde_json::from_str::<Value>(trimmed)
    {
        return Ok(val);
    }
    parse_yaml(input)
}

#[derive(Clone, Debug)]
struct Line {
    line_no: usize,
    indent: usize,
    text: String,
}

fn preprocess_lines(input: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for (idx, raw_line) in input.replace("\r\n", "\n").split('\n').enumerate() {
        let stripped = strip_trailing_comment(raw_line);
        let trimmed_end = stripped.trim_end();
        let indent = trimmed_end.chars().take_while(|c| *c == ' ').count();
        let text = trimmed_end[indent..].to_string();
        if text.is_empty() || text == "---" || text == "..." || text.starts_with('#') {
            continue;
        }
        out.push(Line { line_no: idx + 1, indent, text });
    }
    out
}

fn strip_trailing_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_double {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else {
            match b {
                b'\'' => in_single = true,
                b'"' => in_double = true,
                b'#' if i == 0 || bytes[i - 1].is_ascii_whitespace() => return &line[..i],
                _ => {}
            }
        }
    }
    line
}

fn parse_yaml(input: &str) -> Result<Value, String> {
    // Keep raw lines for literal/folded block scalars too.
    let raw_lines: Vec<String> = input.replace("\r\n", "\n").split('\n').map(String::from).collect();
    let lines = preprocess_lines(input);
    if lines.is_empty() {
        return Ok(Value::Null);
    }
    let mut parser = Parser { lines: &lines, raw_lines: &raw_lines, idx: 0 };
    parser.parse_node(lines[0].indent)
}

struct Parser<'a> {
    lines: &'a [Line],
    raw_lines: &'a [String],
    idx: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Line> {
        self.lines.get(self.idx)
    }

    fn err(&self, line_no: usize, msg: impl Into<String>) -> String {
        format!("YAML line {line_no}: {}", msg.into())
    }

    fn parse_node(&mut self, min_indent: usize) -> Result<Value, String> {
        let Some(first) = self.peek().cloned() else {
            return Ok(Value::Null);
        };
        if first.indent < min_indent {
            return Ok(Value::Null);
        }
        if first.text == "-" || first.text.starts_with("- ") {
            self.parse_seq(first.indent)
        } else if split_key_val(&first.text).is_some() {
            self.parse_map(first.indent)
        } else {
            self.idx += 1;
            parse_inline_value(&first.text).map_err(|e| self.err(first.line_no, e))
        }
    }

    fn parse_seq(&mut self, seq_indent: usize) -> Result<Value, String> {
        let mut items = Vec::new();
        while let Some(line) = self.peek().cloned() {
            if line.indent != seq_indent || (!line.text.starts_with("- ") && line.text != "-") {
                break;
            }
            self.idx += 1;
            let rest = if line.text == "-" { "" } else { line.text[2..].trim_start() };
            if rest.is_empty() {
                if let Some(next) = self.peek().cloned()
                    && next.indent > seq_indent
                {
                    items.push(self.parse_node(next.indent)?);
                } else {
                    items.push(Value::Null);
                }
            } else if let Some((k, v)) = split_key_val(rest) {
                // Inline map entry starting on the `- ` line (e.g. `- name: foo`).
                let item_indent = line.indent + 2;
                let mut map = Map::new();
                let val = self.parse_after_colon(item_indent, line.line_no, v)?;
                map.insert(unquote_key(k)?, val);
                if let Some(next) = self.peek().cloned()
                    && next.indent >= item_indent
                    && !next.text.starts_with("- ")
                    && next.text != "-"
                    && split_key_val(&next.text).is_some()
                {
                    if let Value::Object(more) = self.parse_map(next.indent)? {
                        for (mk, mv) in more {
                            map.insert(mk, mv);
                        }
                    }
                }
                items.push(Value::Object(map));
            } else if let Some(style) = BlockStyle::parse(rest) {
                items.push(self.parse_block_scalar(seq_indent, line.line_no, style));
            } else {
                items.push(parse_inline_value(rest).map_err(|e| self.err(line.line_no, e))?);
            }
        }
        Ok(Value::Array(items))
    }

    fn parse_map(&mut self, map_indent: usize) -> Result<Value, String> {
        let mut map = Map::new();
        while let Some(line) = self.peek().cloned() {
            if line.indent != map_indent {
                break;
            }
            if line.text == "-" || line.text.starts_with("- ") {
                break;
            }
            let Some((raw_key, rest)) = split_key_val(&line.text) else {
                return Err(self.err(line.line_no, format!("expected key: value, found {:?}", line.text)));
            };
            self.idx += 1;
            let key = unquote_key(raw_key)?;
            let val = self.parse_after_colon(map_indent, line.line_no, rest)?;
            map.insert(key, val);
        }
        Ok(Value::Object(map))
    }

    fn parse_after_colon(&mut self, parent_indent: usize, line_no: usize, rest: &str) -> Result<Value, String> {
        let trimmed = rest.trim_start();
        if let Some(style) = BlockStyle::parse(trimmed) {
            return Ok(self.parse_block_scalar(parent_indent, line_no, style));
        }
        if trimmed.is_empty() {
            if let Some(next) = self.peek().cloned() {
                if next.indent > parent_indent {
                    return self.parse_node(next.indent);
                }
                if next.indent == parent_indent && (next.text == "-" || next.text.starts_with("- ")) {
                    return self.parse_seq(next.indent);
                }
            }
            return Ok(Value::Null);
        }
        // Handle multi-line flow JSON/YAML brackets/braces if unclosed on this line.
        if (trimmed.starts_with('[') && !is_flow_balanced(trimmed))
            || (trimmed.starts_with('{') && !is_flow_balanced(trimmed))
        {
            let mut joined = trimmed.to_string();
            while let Some(next) = self.peek().cloned() {
                if next.indent <= parent_indent {
                    break;
                }
                self.idx += 1;
                joined.push(' ');
                joined.push_str(next.text.trim());
                if is_flow_balanced(&joined) {
                    break;
                }
            }
            return parse_inline_value(&joined).map_err(|e| self.err(line_no, e));
        }
        parse_inline_value(trimmed).map_err(|e| self.err(line_no, e))
    }

    fn parse_block_scalar(&mut self, parent_indent: usize, header_line_no: usize, style: BlockStyle) -> Value {
        let mut collected = Vec::<(usize, &str)>::new();
        let mut content_indent: Option<usize> = None;
        let start_raw = header_line_no; // 1-based line_no means index header_line_no is the next raw line.
        let mut last_consumed_raw = header_line_no;

        for (offset, raw) in self.raw_lines.iter().enumerate().skip(start_raw) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                collected.push((0, ""));
                last_consumed_raw = offset + 1;
                continue;
            }
            let indent = raw.chars().take_while(|c| *c == ' ').count();
            if indent <= parent_indent {
                break;
            }
            if content_indent.is_none() {
                content_indent = Some(indent);
            }
            let base = content_indent.unwrap();
            let slice_from = indent.min(base);
            collected.push((indent, &raw[slice_from..]));
            last_consumed_raw = offset + 1;
        }

        while let Some(next) = self.peek() {
            if next.line_no <= last_consumed_raw {
                self.idx += 1;
            } else {
                break;
            }
        }

        // Trim trailing blank lines for strip/clip, keep one trailing newline for clip.
        while collected.last().is_some_and(|(_, s)| s.is_empty()) {
            collected.pop();
        }
        let mut text = if style.folded {
            fold_lines(&collected)
        } else {
            collected.iter().map(|(_, s)| *s).collect::<Vec<_>>().join("\n")
        };
        if !text.is_empty() && !style.strip {
            text.push('\n');
        }
        Value::String(text)
    }
}

#[derive(Clone, Copy)]
struct BlockStyle {
    folded: bool,
    strip: bool,
}

impl BlockStyle {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "|" | "|+" => Some(Self { folded: false, strip: false }),
            "|-" => Some(Self { folded: false, strip: true }),
            ">" | ">+" => Some(Self { folded: true, strip: false }),
            ">-" => Some(Self { folded: true, strip: true }),
            _ => None,
        }
    }
}

fn fold_lines(lines: &[(usize, &str)]) -> String {
    let mut out = String::new();
    for (i, (_, line)) in lines.iter().enumerate() {
        if i > 0 {
            if line.is_empty() || lines[i - 1].1.is_empty() {
                out.push('\n');
            } else {
                out.push(' ');
            }
        }
        out.push_str(line);
    }
    out
}

fn split_key_val(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if in_double {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            continue;
        }
        match b {
            b'"' => in_double = true,
            b'\'' => in_single = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && (i + 1 == bytes.len() || bytes[i + 1].is_ascii_whitespace()) => {
                let key = text[..i].trim();
                let val = text[i + 1..].trim_start();
                if !key.is_empty() {
                    return Some((key, val));
                }
            }
            _ => {}
        }
    }
    None
}

fn unquote_key(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        match parse_quoted_string(trimmed)? {
            Value::String(s) => Ok(s),
            other => Ok(other.to_string()),
        }
    } else {
        Ok(trimmed.to_string())
    }
}

fn parse_inline_value(raw: &str) -> Result<Value, String> {
    let s = raw.trim();
    if s.is_empty() || matches!(s, "null" | "~" | "Null" | "NULL") {
        return Ok(Value::Null);
    }
    if matches!(s, "true" | "True" | "TRUE") {
        return Ok(Value::Bool(true));
    }
    if matches!(s, "false" | "False" | "FALSE") {
        return Ok(Value::Bool(false));
    }
    if s.starts_with('"') || s.starts_with('\'') {
        return parse_quoted_string(s);
    }
    if s.starts_with('[') && s.ends_with(']') {
        return parse_flow_array(&s[1..s.len() - 1]);
    }
    if s.starts_with('{') && s.ends_with('}') {
        return parse_flow_object(&s[1..s.len() - 1]);
    }
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Value::Number(i.into()));
    }
    if let Ok(u) = s.parse::<u64>() {
        return Ok(Value::Number(u.into()));
    }
    if (s.contains('.') || s.contains('e') || s.contains('E'))
        && let Ok(f) = s.parse::<f64>()
        && let Some(n) = Number::from_f64(f)
    {
        return Ok(Value::Number(n));
    }
    Ok(Value::String(s.to_string()))
}

fn parse_quoted_string(s: &str) -> Result<Value, String> {
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        return Ok(Value::String(s[1..s.len() - 1].replace("''", "'")));
    }
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        return serde_json::from_str::<Value>(s).map_err(|e| format!("invalid double-quoted string: {e}"));
    }
    Err(format!("unterminated quoted string: {s}"))
}

fn parse_flow_array(inner: &str) -> Result<Value, String> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let parts = split_flow_items(trimmed)?;
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        out.push(parse_inline_value(part.trim())?);
    }
    Ok(Value::Array(out))
}

fn parse_flow_object(inner: &str) -> Result<Value, String> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let parts = split_flow_items(trimmed)?;
    let mut map = Map::new();
    for part in parts {
        let Some((k, v)) = split_key_val(part.trim()) else {
            return Err(format!("expected key: value in flow object, got {:?}", part.trim()));
        };
        map.insert(unquote_key(k)?, parse_inline_value(v)?);
    }
    Ok(Value::Object(map))
}

fn split_flow_items(s: &str) -> Result<Vec<&str>, String> {
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (i, b) in s.bytes().enumerate() {
        if in_double {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else {
            match b {
                b'"' => in_double = true,
                b'\'' => in_single = true,
                b'[' | b'{' => depth += 1,
                b']' | b'}' => {
                    if depth == 0 {
                        return Err("unbalanced closing bracket".into());
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => {
                    let piece = s[start..i].trim();
                    if !piece.is_empty() {
                        items.push(piece);
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        items.push(tail);
    }
    Ok(items)
}

fn is_flow_balanced(s: &str) -> bool {
    let mut depth = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for b in s.bytes() {
        if in_double {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else {
            match b {
                b'"' => in_double = true,
                b'\'' => in_single = true,
                b'[' | b'{' => depth += 1,
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    depth == 0 && !in_single && !in_double
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_plan_yaml_with_nested_maps_lists_and_block_strings() {
        let doc = r#"
objective: Build Drove
defaults:
  assign:
    role: coder
    effort: high
  isolation: worktree
tasks:
  scan:
    title: Scan repo # inline comment
    kind: exec
    exec:
      cmd: |
        git status
        echo "done"
    deps: []
  impl:
    title: Implement core
    deps: [scan]
    spec:
      target: "src/drove"
      change: Implement store and http
      acceptance: cargo test passes
      verify:
        cmd: cargo test --locked
        timeoutSec: 120
    files:
      - src/drove/mod.rs
      - src/drove/store.rs
"#;
        let val = parse_yaml_or_json(doc).unwrap();
        assert_eq!(val["objective"], "Build Drove");
        assert_eq!(val["defaults"]["assign"]["role"], "coder");
        assert_eq!(val["defaults"]["isolation"], "worktree");
        assert_eq!(val["tasks"]["scan"]["title"], "Scan repo");
        assert_eq!(val["tasks"]["scan"]["exec"]["cmd"], "git status\necho \"done\"\n");
        assert_eq!(val["tasks"]["impl"]["deps"], json!(["scan"]));
        assert_eq!(val["tasks"]["impl"]["spec"]["verify"]["timeoutSec"], 120);
        assert_eq!(val["tasks"]["impl"]["files"], json!(["src/drove/mod.rs", "src/drove/store.rs"]));
    }

    #[test]
    fn parses_sequence_of_maps_and_json_passthrough() {
        let y = r#"
items:
  - name: first
    enabled: true
  - name: second
    enabled: false
"#;
        let v = parse_yaml_or_json(y).unwrap();
        assert_eq!(v["items"][0]["name"], "first");
        assert_eq!(v["items"][0]["enabled"], true);
        assert_eq!(v["items"][1]["enabled"], false);

        let j = parse_yaml_or_json(r#"{"tasks":{"a":{"title":"A"}}}"#).unwrap();
        assert_eq!(j["tasks"]["a"]["title"], "A");
    }
}
