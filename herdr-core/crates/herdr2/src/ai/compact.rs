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
pub fn strip_and_dedup_lines(
    raw: &str,
    max_lines: usize,
    max_line_chars: usize,
    max_total_chars: usize,
) -> String {
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
    let formatted: Vec<String> = collapsed
        .into_iter()
        .skip(skip)
        .map(|(l, n)| if n > 1 { format!("{l} [x{n}]") } else { l })
        .collect();
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
}
