//! `h2 run …`: flags after a template's task, and what a run that waited prints.

use super::*;

/// A template's task is plain text, so `--name x` after it is h2's, not part
/// of the task. Flags given before stay; flags after the task are merged.
pub(super) fn flags_after_task(paths: &Paths, p: RunParams) -> Result<(RunParams, bool), RpcError> {
    if p.agent.is_some() {
        return Ok((p, false));
    }
    let Some((q, json)) = trailing_flags(paths, &p.argv)? else { return Ok((p, false)) };
    let merged = RunParams {
        agent: q.agent.or(p.agent),
        prompt: q.prompt.or(p.prompt),
        system_prompt: q.system_prompt.or(p.system_prompt),
        model: q.model.or(p.model),
        mode: q.mode.or(p.mode),
        permissions: q.permissions.or(p.permissions),
        no_mcp: p.no_mcp || q.no_mcp,
        argv: q.argv,
        name: q.name.or(p.name),
        cwd: q.cwd.or(p.cwd),
        on: q.on.or(p.on),
        labels: [p.labels, q.labels].concat(),
        parent: q.parent.or(p.parent),
        no_parent: p.no_parent || q.no_parent,
        no_notify: p.no_notify || q.no_notify,
        outlive_parent: p.outlive_parent || q.outlive_parent,
        rm: p.rm || q.rm,
        keep: p.keep || q.keep,
        bg: p.bg || q.bg,
        async_start: p.async_start || q.async_start,
        timeout: q.timeout.or(p.timeout),
        ..p
    };
    if merged.parent.is_some() && merged.no_parent {
        return Err(RpcError::new("usage", "--parent and --no-parent together", "see `h2 run --help`"));
    }
    Ok((merged, json))
}

fn trailing_flags(paths: &Paths, argv: &[String]) -> Result<Option<(RunParams, bool)>, RpcError> {
    use clap::CommandFactory;
    if argv.len() < 2 || !matches!(crate::templates::load(paths, &argv[0]), Ok(Some(_))) {
        return Ok(None);
    }
    let cli = Cli::command();
    let Some(run) = cli.find_subcommand("run") else { return Ok(None) };
    let (mut flags, mut task) = (Vec::new(), Vec::new());
    let mut rest = argv[1..].iter();
    while let Some(a) = rest.next() {
        let long = a.strip_prefix("--").map(|l| l.split('=').next().unwrap_or(l));
        match long.and_then(|l| run.get_arguments().chain(cli.get_arguments()).find(|arg| arg.get_long() == Some(l))) {
            Some(arg) => {
                flags.push(a.clone());
                if arg.get_action().takes_values() && !a.contains('=') {
                    flags.extend(rest.next().cloned());
                }
            }
            None => task.push(a.clone()),
        }
    }
    if flags.is_empty() {
        return Ok(None);
    }
    let args = ["h2".to_string(), "run".into()].into_iter().chain(flags).chain(["--".into(), argv[0].clone()]).chain(task);
    match Cli::try_parse_from(args) {
        Ok(Cli { cmd: Some(Cmd::Run(q)), json, .. }) => Ok(Some((q, json))),
        Ok(_) => Ok(None),
        Err(e) => Err(RpcError::new("usage", usage_message(&e), "see `h2 run --help`")),
    }
}

/// A run that waited: its transcript output, a status footer, and exit code as h2's (124: still running).
pub(super) fn ran(r: &Value) -> Result<Option<Out>, RpcError> {
    use std::io::Write;
    let a = r["address"].as_str().unwrap_or("?");
    if r.get("wait").is_none() {
        let fix = a.split_once('/').map(|(m, _)| format!(": h2 machine update {m}")).unwrap_or_default();
        println!("[h2] started as {a}, not waited on: that h2 is older{fix}");
        std::process::exit(0);
    }
    let w = &r["wait"];
    let status = w["status"].as_str().unwrap_or("no status");
    let (note, code) = match w["reason"].as_str() {
        Some("timeout") => (Some(format!("still running as {a}: h2 wait {a}, h2 read {a}")), 124),
        Some("exit") => match (w["exit_code"].as_i64(), w["signal"].as_i64()) {
            (Some(0), _) => (None, 0),
            (Some(c), _) => (Some(format!("{a} exited (exit {c})")), c as i32),
            (_, Some(sig)) => (Some(format!("{a} ended by signal {sig}")), 128 + sig as i32),
            _ => (Some(format!("{a} ended")), 1),
        },
        _ if status == "blocked" => (Some(format!("{a} is blocked: approve with h2 approve {a} or h2 deny {a}")), 0),
        _ => (Some(format!("{a} is {status}: h2 send {a} <text>")), 0),
    };
    let mut text = r["output"].as_str().unwrap_or_default().trim_end().to_string();
    if let Some(first) = r.get("first_message").filter(|f| f["sent"] != true) {
        text.push_str(&format!("\n[h2] first message NOT sent: {}", first["reason"].as_str().unwrap_or("?")));
    }
    if let Some(note) = note {
        text.push_str(&format!("\n[h2] {note}"));
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}", text.trim_start_matches('\n')).and_then(|_| out.flush());
    std::process::exit(code);
}
