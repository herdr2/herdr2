//! How results look to a person: rows, tables, one-line summaries.

use super::*;

/// The agent's status word (or how it ended) and its dot.
pub(super) fn status(a: &Value) -> (String, &'static str) {
    const HOLLOW: &str = "\x1b[2m\u{25cb}\x1b[22m";
    if a["state"] != "live" {
        let word = match (a["exit_code"].as_i64(), a["signal"].as_i64()) {
            (Some(code), _) => format!("exit {code}"),
            (_, Some(sig)) => format!("signal {sig}"),
            _ => a["reason"].as_str().unwrap_or("ended").into(),
        };
        return (word, HOLLOW);
    }
    let Some(word) = a["status"]["state"].as_str() else { return ("running".into(), HOLLOW) };
    let dot = match word {
        "working" => "\x1b[33m\u{25cf}\x1b[39m",
        "blocked" | "error" => "\x1b[31m\u{25cf}\x1b[39m",
        "done" => "\x1b[32m\u{25cf}\x1b[39m",
        _ => "\u{25cf}",
    };
    (word.into(), dot)
}

/// Which agent binary/preset runs here (or foreground app / command for terminals).
pub(super) fn agent_label(a: &Value) -> String {
    if let Some(preset) = a["agent"].as_str().filter(|s| !s.is_empty()) {
        return preset.into();
    }
    if let Some(info) = a["agent_info"].as_str().filter(|s| !s.is_empty()) {
        return info.into();
    }
    if let Some(app) = a["status"]["app"].as_str().filter(|s| !s.is_empty()) {
        return app.into();
    }
    let program = a["argv"][0].as_str().unwrap_or_default();
    program.rsplit('/').next().unwrap_or_default().into()
}

fn cwd(a: &Value) -> Option<&str> {
    a["foreground"]["cwd"].as_str().or_else(|| a["cwd"].as_str())
}

/// The last folder of an agent's cwd, `~` for home.
pub(super) fn folder(a: &Value) -> String {
    let cwd = cwd(a).unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && cwd.trim_end_matches('/') == home.trim_end_matches('/') {
        return "~".into();
    }
    cwd.trim_end_matches('/').rsplit('/').next().filter(|f| !f.is_empty()).unwrap_or(cwd).into()
}

pub(super) fn detail(a: &Value) -> String {
    if let Some(perm) = a["pending_permission"]["title"].as_str().filter(|s| !s.is_empty()) {
        return format!("approve: {perm}");
    }
    if let Some(msg) = a["status"]["msg"].as_str().filter(|s| !s.is_empty()) {
        return msg.lines().next().unwrap_or_default().chars().take(60).collect();
    }
    let base = a["labels"]["drove.display"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| a["template"].as_str().filter(|s| !s.is_empty()))
        .map(String::from)
        .unwrap_or_else(|| folder(a));
    match a["inbox_len"].as_u64() {
        Some(n) if n > 0 => format!("{base} (inbox: {n})"),
        _ => base,
    }
}

/// The facts an agent acts on; `h2 get` has the full record.
pub(super) fn short(a: &Value) -> Value {
    let keep = [
        "address",
        "name",
        "template",
        "parent",
        "state",
        "turn",
        "inbox_len",
        "pending_permission",
        "exit_code",
        "signal",
        "reason",
        "cwd",
    ];
    let mut out: serde_json::Map<String, Value> =
        keep.iter().filter_map(|k| a.get(*k).filter(|v| !v.is_null()).map(|v| (k.to_string(), v.clone()))).collect();
    if let Some(word) = a["status"]["state"].as_str() {
        out.insert("status".into(), word.into());
    }
    out.insert("agent".into(), agent_label(a).into());
    Value::Object(out)
}

pub(super) fn describe_wait(r: &Value) -> String {
    let a = r["agent"].as_str().unwrap_or("?");
    let mut lines: Vec<String> = r["children"].as_array().into_iter().flatten().map(describe_wait).collect();
    if r["reason"] == "message" || !r["from"].is_null() {
        lines.insert(0, format!("a message from {} (queued in inbox)", r["from"].as_str().unwrap_or("a caller")));
        if r["ended"] == false && lines.len() == 1 {
            let word = r["status"].as_str().unwrap_or("no status");
            lines.push(format!("{a} still running: {word}, turn {} (wait again to keep waiting)", r["turn"]));
        }
    }
    if r["reason"] == "children" && lines.is_empty() {
        return "no children are working".into();
    }
    if !lines.is_empty() {
        return lines.join("\n");
    }
    let word = r["status"].as_str().unwrap_or("no status");
    match r["reason"].as_str() {
        Some("exit") => format!("{a} exited (exit {}, turn {})", r["exit_code"], r["turn"]),
        Some("timeout") => format!("{a} still running: {word}, turn {} (wait again to keep waiting)", r["turn"]),
        Some("output") => format!("{a} matched output ({word}, turn {})", r["turn"]),
        Some("not_started") => format!("{a} hasn't started on it ({word}): h2 read {a}, then h2 wait {a}"),
        Some("running") => format!("{a} still running: {word}, turn {}", r["turn"]),
        _ if word == "blocked" => format!("{a} is blocked (turn {}): h2 approve {a} or h2 deny {a}", r["turn"]),
        _ => format!("{a} {word}, turn {}. Read: h2 read {a}", r["turn"]),
    }
}

pub(super) fn summary(a: &Value) -> String {
    match a["state"].as_str() {
        Some("live") => match a["status"]["state"].as_str() {
            Some(word) => format!("live, {word} (turn {})", a["turn"]),
            None => "live".into(),
        },
        _ => {
            let reason = a["reason"].as_str().unwrap_or("finished");
            match (a["exit_code"].as_i64(), a["signal"].as_i64()) {
                (Some(code), _) => format!("finished ({reason}, exit {code})"),
                (_, Some(sig)) => format!("finished ({reason}, signal {sig})"),
                _ => format!("finished ({reason})"),
            }
        }
    }
}

pub(super) fn command(a: &Value) -> String {
    let argv: Vec<&str> = a["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    argv.join(" ")
}

fn order_tree(agents: &[Value]) -> Vec<(usize, &Value)> {
    let present: std::collections::HashSet<&str> = agents.iter().filter_map(|a| a["address"].as_str()).collect();
    let mut children: std::collections::HashMap<&str, Vec<&Value>> = std::collections::HashMap::new();
    let mut roots = Vec::new();
    for a in agents {
        if let Some(p) = a["parent"].as_str().filter(|p| present.contains(*p)) {
            children.entry(p).or_default().push(a);
        } else {
            roots.push(a);
        }
    }
    let mut ordered = Vec::with_capacity(agents.len());
    fn push<'a>(
        node: &'a Value,
        depth: usize,
        children: &std::collections::HashMap<&str, Vec<&'a Value>>,
        out: &mut Vec<(usize, &'a Value)>,
    ) {
        out.push((depth, node));
        if let Some(addr) = node["address"].as_str()
            && let Some(kids) = children.get(addr)
        {
            for kid in kids {
                push(kid, depth + 1, children, out);
            }
        }
    }
    for root in roots {
        push(root, 0, &children, &mut ordered);
    }
    ordered
}

pub(super) fn table(r: &Value) -> String {
    let mut records: Vec<Value> = r["terminals"].as_array().cloned().unwrap_or_default();
    records.extend(r["agents"].as_array().cloned().unwrap_or_default());
    let mut out = String::new();
    if records.is_empty() {
        out.push_str("no live agents\n");
    } else {
        let rows: Vec<(&'static str, [String; 6])> = order_tree(&records)
            .into_iter()
            .map(|(depth, a)| {
                let (word, dot) = status(a);
                let indent = "  ".repeat(depth);
                let addr = format!("{indent}{}", a["address"].as_str().unwrap_or("?"));
                let name = a["name"].as_str().unwrap_or_default().to_string();
                let agent = agent_label(a);
                let turn = a["turn"].as_u64().unwrap_or(0).to_string();
                let det = detail(a);
                (dot, [addr, name, agent, word, turn, det])
            })
            .collect();
        const HEADERS: [&str; 6] = ["ADDRESS", "NAME", "AGENT", "STATUS", "TURN", "DETAIL"];
        let mut widths = [0usize; 6];
        for i in 0..6 {
            widths[i] = HEADERS[i].len().max(rows.iter().map(|(_, r)| r[i].chars().count()).max().unwrap_or(0));
        }
        let format_row = |lead: &str, cols: &[String; 6], dim: bool| {
            let mut s = format!("{lead} ");
            for (i, col) in cols.iter().enumerate() {
                if i > 0 {
                    s.push_str("  ");
                }
                if i + 1 < cols.len() {
                    let pad = widths[i].saturating_sub(col.chars().count());
                    s.push_str(col);
                    s.extend(std::iter::repeat_n(' ', pad));
                } else {
                    s.push_str(col);
                }
            }
            if dim { format!("\x1b[2m{s}\x1b[22m") } else { s }
        };
        out.push_str(&format_row(" ", &HEADERS.map(String::from), true));
        out.push('\n');
        for (dot, cols) in &rows {
            out.push_str(&format_row(dot, cols, false));
            out.push('\n');
        }
    }
    for m in r["machines"].as_array().into_iter().flatten().filter(|m| m["state"] != "connected") {
        out.push_str(&format!("{}: {}\n", m["name"].as_str().unwrap_or("?"), line_state(m)));
    }
    match r["hidden_finished"].as_u64() {
        Some(n) if n > 0 => out.push_str(&format!("{n} finished hidden (--all)")),
        _ => {
            out.pop();
        }
    }
    out
}

/// How a machine's line is: "connected", "unreachable for 5m (why)", ...
pub(super) fn line_state(m: &Value) -> String {
    let since = m["since_ms"].as_u64().map(|ms| {
        let ago = crate::runtime::now_ms().saturating_sub(ms) / 1000;
        match ago {
            0..60 => format!(" for {ago}s"),
            60..3600 => format!(" for {}m", ago / 60),
            _ => format!(" for {}h", ago / 3600),
        }
    });
    let why = m["why"].as_str().map(|w| format!(" ({w})")).unwrap_or_default();
    match m["state"].as_str() {
        Some("needs_login") => {
            let keys = if cfg!(windows) { "set up its SSH keys, then " } else { "" };
            format!(
                "needs login{}: {keys}h2 machine login {}",
                since.unwrap_or_default(),
                m["name"].as_str().unwrap_or("?")
            )
        }
        Some("needs_update") => format!("needs an h2 update: h2 machine update {}", m["name"].as_str().unwrap_or("?")),
        Some(state) => format!("{state}{}{why}", since.unwrap_or_default()),
        None => "?".into(),
    }
}

pub(super) fn describe(a: &Value) -> String {
    let labels: Vec<String> = a["labels"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
        .collect();
    let mut description = format!(
        "{} {}\n  name: {}\n  agent: {}\n  parent: {}\n  command: {}\n  cwd: {}\n  pid: {}\n  permissions: {}\n  labels: {}",
        a["address"].as_str().unwrap_or("?"),
        summary(a),
        a["name"].as_str().unwrap_or("-"),
        agent_label(a),
        a["parent"].as_str().unwrap_or("-"),
        command(a),
        cwd(a).unwrap_or("-"),
        a["pid"],
        a["permission_mode"].as_str().unwrap_or("approve-reads"),
        if labels.is_empty() { "-".into() } else { labels.join(" ") },
    );
    if let Some(sid) = a["acp_session_id"].as_str() {
        description.push_str(&format!("\n  acp_session: {sid}"));
    }
    if let Some(n) = a["inbox_len"].as_u64().filter(|&n| n > 0) {
        description.push_str(&format!("\n  inbox: {n} queued"));
    }
    if let Some(perm) = a.get("pending_permission").filter(|p| !p.is_null()) {
        description.push_str(&format!("\n  pending_permission: {}", perm["title"].as_str().unwrap_or("?")));
    }
    if let Some(first) = a.get("first_message").filter(|f| f["sent"] == false) {
        description.push_str(&format!("\n  first message NOT sent: {}", first["reason"].as_str().unwrap_or("?")));
    }
    if let Some(lin) = a.get("lineage").filter(|l| l.is_object()) {
        let kind = lin["kind"].as_str().unwrap_or("handoff");
        let parent = lin["parent"].as_str().unwrap_or("?");
        let mut desc = format!("{kind} from {parent}");
        if let Some(prov) = lin["parent_provider"].as_str() {
            desc.push_str(&format!(" ({prov})"));
        }
        if let (Some(f), Some(t)) = (lin["from_turn"].as_u64(), lin["to_turn"].as_u64()) {
            desc.push_str(&format!(" turns {f}-{t}"));
        }
        description.push_str(&format!("\n  lineage: {desc}"));
    }
    description
}

/// An exec's output as the program wrote it, then its exit code as ours.
pub(super) fn exec_out(r: &Value) -> ! {
    use std::io::Write;
    let bytes = |key: &str| crate::put::decode(r[key].as_str().unwrap_or_default()).unwrap_or_default();
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let _ = out.write_all(&bytes("stdout")).and_then(|_| out.flush());
    let _ = err.write_all(&bytes("stderr"));
    if r["truncated"] == true {
        let _ = writeln!(err, "[h2] output cut: kept the first 8 MB of stdout, 1 MB of stderr");
    }
    let code = match (r["timed_out"].as_bool(), r["code"].as_i64(), r["signal"].as_i64()) {
        (Some(true), _, _) => {
            let _ = writeln!(err, "[h2] timed out after {} ms: ended", r["ms"]);
            124
        }
        (_, Some(code), _) => code as i32,
        (_, _, Some(signal)) => 128 + signal as i32,
        _ => 1,
    };
    let _ = err.flush();
    std::process::exit(code)
}
