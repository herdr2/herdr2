//! Input text compaction and volatile-token normalization for high cache hit rates
//! and lower LLM input token consumption.

/// Strips ANSI CSI/OSC escape sequences and non-whitespace control characters.
pub fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                b'[' => {
                    // CSI: ESC [ ... final byte 0x40..=0x7E
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
                b']' => {
                    // OSC: ESC ] ... BEL or ESC \
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 1;
                }
            }
            continue;
        }
        let ch = raw[i..].chars().next().unwrap();
        let len = ch.len_utf8();
        if !ch.is_control() || ch == '\n' || ch == '\t' {
            out.push(ch);
        }
        i += len;
    }
    out
}

fn is_box_or_rule_only(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return true;
    }
    t.chars().all(|c| {
        matches!(
            c,
            '─' | '━'
                | '═'
                | '│'
                | '┃'
                | '║'
                | '╭'
                | '╮'
                | '╰'
                | '╯'
                | '┌'
                | '┐'
                | '└'
                | '┘'
                | '├'
                | '┤'
                | '┬'
                | '┴'
                | '┼'
                | '-'
                | '='
                | '_'
                | '~'
                | '*'
                | '+'
                | '|'
                | ' '
                | '\t'
        )
    })
}

/// Cleans terminal/log text: strips ANSI, drops pure box-drawing/blank lines, collapses consecutive
/// identical lines (`[xN]`), keeps the trailing `max_lines`, and bounds total char count.
pub fn strip_and_dedup_lines(raw: &str, max_lines: usize, max_line_chars: usize, max_total_chars: usize) -> String {
    let clean = strip_ansi(raw);
    let mut collapsed: Vec<(String, usize)> = Vec::new();
    for line in clean.lines() {
        let trimmed = line.trim();
        if is_box_or_rule_only(trimmed) {
            continue;
        }
        let bounded: String = if trimmed.chars().count() > max_line_chars {
            let mut s: String = trimmed.chars().take(max_line_chars.saturating_sub(1)).collect();
            s.push('…');
            s
        } else {
            trimmed.to_string()
        };
        if let Some((last, count)) = collapsed.last_mut()
            && *last == bounded
        {
            *count += 1;
        } else {
            collapsed.push((bounded, 1));
        }
    }

    let skip = collapsed.len().saturating_sub(max_lines);
    let formatted: Vec<String> =
        collapsed.into_iter().skip(skip).map(|(l, n)| if n > 1 { format!("{l} [x{n}]") } else { l }).collect();
    let joined = formatted.join("\n");
    if max_total_chars == 0 || joined.chars().count() <= max_total_chars {
        return joined;
    }
    let half = (max_total_chars / 2).max(40);
    let total = joined.chars().count();
    let head: String = joined.chars().take(half).collect();
    let tail: String = joined.chars().skip(total.saturating_sub(half)).collect();
    format!("{head}\n…\n{tail}")
}

fn is_spinner_glyph(c: char) -> bool {
    matches!(
        c,
        '⠋' | '⠙'
            | '⠹'
            | '⠸'
            | '⠼'
            | '⠴'
            | '⠦'
            | '⠧'
            | '⠇'
            | '⠏'
            | '⣾'
            | '⣽'
            | '⣻'
            | '⢿'
            | '⡿'
            | '⣟'
            | '⣯'
            | '⣷'
            | '✦'
            | '❖'
            | '✳'
            | '◐'
            | '◓'
            | '◑'
            | '◒'
    )
}

/// Normalizes volatile runtime tokens (timestamps, elapsed timers, percentages, spinners, ANSI, box rules)
/// so semantically identical prompts hash to the same local cache key (0-token hit).
pub fn normalize_for_fingerprint(raw: &str) -> String {
    let clean = strip_ansi(raw);
    let mut out = String::with_capacity(clean.len());
    for line in clean.lines() {
        if is_box_or_rule_only(line) {
            continue;
        }
        for tok in line.split_whitespace() {
            let trimmed: String = tok.chars().filter(|&c| !is_spinner_glyph(c)).collect();
            if trimmed.is_empty() {
                continue;
            }
            if is_volatile_token(&trimmed) {
                out.push_str("<#>");
            } else {
                out.push_str(&trimmed.to_ascii_lowercase());
            }
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

fn is_volatile_token(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | ',' | ';' | ':'));
    if t.is_empty() {
        return false;
    }
    // Clock timestamps like 14:32:05 or 14:32
    let colon_parts: Vec<&str> = t.split(':').collect();
    if (colon_parts.len() == 2 || colon_parts.len() == 3)
        && colon_parts.iter().all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    // ISO dates like 2026-11-21
    let dash_parts: Vec<&str> = t.split('-').collect();
    if dash_parts.len() == 3
        && dash_parts[0].len() == 4
        && dash_parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    // Elapsed timers or percentages: 12s, 450ms, 3m, 87%, 1.4s
    for suffix in ["ms", "s", "m", "h", "%"] {
        if let Some(num) = t.strip_suffix(suffix)
            && !num.is_empty()
            && num.chars().all(|c| c.is_ascii_digit() || c == '.')
            && num.chars().any(|c| c.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

/// Redacts high-risk credentials (Bearer tokens, API keys, PEM private keys, URL credentials,
/// and sensitive env assignments) before sending untrusted terminal/log/diff text to an external LLM.
pub fn redact_secrets(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mut lines_out: Vec<String> = Vec::new();
    let mut in_pem_block = false;
    for line in raw.lines() {
        if in_pem_block {
            if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                in_pem_block = false;
                lines_out.push("<REDACTED_PRIVATE_KEY>".into());
            }
            continue;
        }
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            if line.contains("-----END ") {
                lines_out.push("<REDACTED_PRIVATE_KEY>".into());
            } else {
                in_pem_block = true;
            }
            continue;
        }
        lines_out.push(redact_line(line));
    }
    if in_pem_block {
        lines_out.push("<REDACTED_PRIVATE_KEY>".into());
    }
    let mut out = lines_out.join("\n");
    if raw.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn is_secret_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+' | b'/' | b'=')
}

fn redact_line(line: &str) -> String {
    let mut cur = redact_sensitive_env_assignment(line);
    cur = redact_url_credentials(&cur);
    cur = redact_bearer_tokens(&cur);
    redact_prefixed_tokens(&cur)
}

fn redact_sensitive_env_assignment(line: &str) -> String {
    let bytes = line.as_bytes();
    let Some(eq_idx) = bytes.iter().position(|&b| b == b'=' || b == b':') else {
        return line.to_string();
    };
    let lhs = line[..eq_idx].trim();
    let key_token = lhs
        .rsplit(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '{' | ','))
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`'));
    let upper = key_token.to_ascii_uppercase();
    let is_sensitive_key = [
        "API_KEY",
        "APIKEY",
        "SECRET_KEY",
        "ACCESS_KEY",
        "ACCESS_TOKEN",
        "AUTH_TOKEN",
        "REFRESH_TOKEN",
        "PRIVATE_KEY",
        "CLIENT_SECRET",
        "PASSWORD",
        "PASSWD",
        "BEARER_TOKEN",
    ]
    .iter()
    .any(|kw| upper == *kw || upper.ends_with(&format!("_{kw}")));
    if !is_sensitive_key {
        return line.to_string();
    }
    let rhs_raw = &line[eq_idx + 1..];
    let rhs_trimmed = rhs_raw.trim();
    let unquoted = rhs_trimmed.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';'));
    if unquoted.len() < 8
        || unquoted.contains("<REDACTED")
        || unquoted.starts_with('$')
        || unquoted.starts_with("env:")
        || matches!(
            unquoted.to_ascii_lowercase().as_str(),
            "your_api_key" | "changeme" | "placeholder" | "example" | "redacted" | "xxxxxxxx"
        )
    {
        return line.to_string();
    }
    // Avoid redacting code expressions like `api_key: Option<String>` or `password = req.body.password`.
    if unquoted.contains(' ') || unquoted.contains('(') || unquoted.contains('<') || unquoted.contains("::") {
        return line.to_string();
    }
    let sep = &line[eq_idx..=eq_idx];
    let leading_ws: String = rhs_raw.chars().take_while(|c| c.is_whitespace()).collect();
    let quote = rhs_trimmed.chars().next().filter(|&c| c == '"' || c == '\'');
    let trailing_punct = rhs_trimmed.chars().last().filter(|&c| c == ',' || c == ';');
    let mut out = format!("{}{sep}{leading_ws}", &line[..eq_idx]);
    if let Some(q) = quote {
        out.push(q);
        out.push_str("<REDACTED>");
        out.push(q);
    } else {
        out.push_str("<REDACTED>");
    }
    if let Some(p) = trailing_punct {
        out.push(p);
    }
    out
}

fn redact_url_credentials(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find("://") {
        let after = &rest[pos + 3..];
        let auth_end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | ')' | '>' | ']'))
            .unwrap_or(after.len());
        let authority = &after[..auth_end];
        if let Some(at_pos) = authority.rfind('@') {
            let userinfo = &authority[..at_pos];
            if userinfo.contains(':') && !userinfo.contains("<REDACTED") {
                out.push_str(&rest[..pos + 3]);
                if let Some((user, _)) = userinfo.split_once(':')
                    && !user.is_empty()
                {
                    out.push_str(user);
                    out.push_str(":<REDACTED>@");
                } else {
                    out.push_str("<REDACTED>@");
                }
                rest = &after[at_pos + 1..];
                continue;
            }
        }
        out.push_str(&rest[..pos + 3]);
        rest = after;
    }
    out.push_str(rest);
    out
}

fn redact_bearer_tokens(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 7 <= bytes.len()
            && bytes[i..i + 7].eq_ignore_ascii_case(b"bearer ")
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            out.push_str(&line[i..i + 7]);
            i += 7;
            while i < bytes.len() && bytes[i] == b' ' {
                out.push(' ');
                i += 1;
            }
            let start = i;
            while i < bytes.len() && is_secret_token_char(bytes[i]) {
                i += 1;
            }
            let tok = &line[start..i];
            if tok.len() >= 12 && !tok.contains("<REDACTED") {
                out.push_str("<REDACTED>");
            } else {
                out.push_str(tok);
            }
            continue;
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn redact_prefixed_tokens(line: &str) -> String {
    const PREFIXES: &[(&str, usize)] = &[
        ("sk-proj-", 20),
        ("sk-ant-", 20),
        ("sk-", 20),
        ("github_pat_", 20),
        ("ghp_", 16),
        ("gho_", 16),
        ("ghu_", 16),
        ("ghs_", 16),
        ("glpat-", 16),
        ("xoxb-", 16),
        ("xoxp-", 16),
        ("xoxa-", 16),
        ("AKIA", 16),
        ("AIza", 24),
    ];

    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let mut matched = false;
        if boundary {
            for &(prefix, min_tail) in PREFIXES {
                if line[i..].starts_with(prefix) {
                    let mut end = i + prefix.len();
                    while end < bytes.len() && is_secret_token_char(bytes[end]) {
                        end += 1;
                    }
                    let tail_len = end - (i + prefix.len());
                    let tok = &line[i..end];
                    // Avoid false positives on ordinary hyphenated words lacking digits/entropy.
                    let has_digit_or_upper =
                        tok[prefix.len()..].bytes().any(|b| b.is_ascii_digit() || b.is_ascii_uppercase());
                    if tail_len >= min_tail && (prefix != "sk-" || has_digit_or_upper) {
                        out.push_str("<REDACTED>");
                        i = end;
                        matched = true;
                        break;
                    }
                }
            }
        }
        if matched {
            continue;
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 64-bit FNV-1a hash for fast deterministic content fingerprinting.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub fn fingerprint_hex(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &b in part.as_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0x1f;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_box_rules_and_dedups_repeated_lines() {
        let raw = "\x1b[31merror[E0433]\x1b[0m: missing crate\n───────────\nretrying\nretrying\nretrying\n";
        let out = strip_and_dedup_lines(raw, 10, 120, 1000);
        assert_eq!(out, "error[E0433]: missing crate\nretrying [x3]");
    }

    #[test]
    fn volatile_normalization_matches_across_timers_and_spinners() {
        let a = normalize_for_fingerprint("⠋ Thinking (12s) 14:30:01 45%\nDo you want to continue?");
        let b = normalize_for_fingerprint("⠙ Thinking (38s) 14:30:27 82%\nDo you want to continue?");
        assert_eq!(a, b);
    }

    #[test]
    fn redacts_secrets_without_corrupting_normal_code() {
        let sample = "\
Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.abc123
OPENAI_API_KEY=sk-proj-1234567890ABCDEFGHIJKLMNOP
GITHUB_TOKEN=\"ghp_1234567890abcdefghijklmnop\"
DATABASE_URL=postgres://admin:s3cr3tPass@db.internal:5432/app
-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEAtestkeymaterial
-----END RSA PRIVATE KEY-----
let token_count = 42;
let api_key: Option<String> = None;
";
        let out = redact_secrets(sample);
        assert!(!out.contains("eyJhbGci"));
        assert!(!out.contains("sk-proj-1234"));
        assert!(!out.contains("ghp_1234"));
        assert!(!out.contains("s3cr3tPass"));
        assert!(!out.contains("MIIEpAIBAAKCAQEA"));
        assert!(out.contains("Authorization: Bearer <REDACTED>"));
        assert!(out.contains("OPENAI_API_KEY=<REDACTED>"));
        assert!(out.contains("postgres://admin:<REDACTED>@db.internal:5432/app"));
        assert!(out.contains("<REDACTED_PRIVATE_KEY>"));
        assert!(out.contains("let token_count = 42;"));
        assert!(out.contains("let api_key: Option<String> = None;"));
    }
}
