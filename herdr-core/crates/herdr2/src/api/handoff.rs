//! `agent.handoff`: cross-provider context handoff, fork, side-chat, and merge-back across ACP and PTY sessions.

use super::*;
use crate::handoff::{
    DEFAULT_HANDOFF_MAX_BYTES, HandoffBuildInput, Lineage, LineageKind, build_handoff_package, default_pty_argv,
    format_turn_ranges, wrap_handoff_envelope,
};
use crate::pty_adapters::{PtyCliKind, build_rollback_argv, fork_claude_jsonl, fork_codex_jsonl, h2_exe_string};
use crate::pty_transcript::{
    extract_transcript, find_claude_session_file, find_codex_rollout_by_sid, snapshot_from_terminal,
};
use crate::runtime::{TargetKey, now_ms};
use crate::transcript::Entry;

struct SourceSnapshot {
    address: String,
    cwd: String,
    provider: Option<String>,
    session_id: Option<String>,
    model: Option<String>,
    argv: Vec<String>,
    is_agent: bool,
    cols: u16,
    rows: u16,
    entries: Vec<Entry>,
}

pub(super) async fn agent_handoff(core: Ctx<'_>, p: HandoffParams) -> Result<HandoffResult, RpcError> {
    not_stopping(&core.lock())?;

    if let (Some(from), Some(to)) = (p.from_turn, p.to_turn)
        && from > to
    {
        return Err(RpcError::invalid_params("from_turn must be <= to_turn"));
    }

    let src = collect_source(core, &p.source).await?;
    let cwd = p
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&src.cwd)
        .to_string();

    let cwd_for_pkg = cwd.clone();
    let entries_for_pkg = src.entries.clone();
    let kind = p.kind;
    let src_addr_pkg = src.address.clone();
    let src_prov_pkg = src.provider.clone();
    let src_sid_pkg = src.session_id.clone();
    let src_model_pkg = src.model.clone();
    let from_turn = p.from_turn;
    let to_turn = p.to_turn;
    let note_pkg = p.note.clone();

    let pkg = blocking(move || {
        Ok(build_handoff_package(&HandoffBuildInput {
            kind,
            source_address: &src_addr_pkg,
            source_provider: src_prov_pkg.as_deref(),
            source_session_id: src_sid_pkg.as_deref(),
            source_model: src_model_pkg.as_deref(),
            cwd: (!cwd_for_pkg.is_empty()).then_some(cwd_for_pkg.as_str()),
            entries: &entries_for_pkg,
            from_turn,
            to_turn,
            max_bytes: DEFAULT_HANDOFF_MAX_BYTES,
            note: note_pkg.as_deref(),
        }))
    })
    .await?;

    let turns_label = (!pkg.included_turns.is_empty()).then(|| format_turn_ranges(&pkg.included_turns));
    let mut lineage = Lineage {
        kind: p.kind,
        parent: src.address.clone(),
        parent_provider: src.provider.clone(),
        parent_session_id: src.session_id.clone(),
        from_turn: p.from_turn.or_else(|| pkg.included_turns.first().copied()),
        to_turn: p.to_turn.or_else(|| pkg.included_turns.last().copied()),
        artifact_path: None,
    };

    if let Some(into_raw) = p.into.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return handoff_into_existing(core, &p, &src, &pkg, &lineage, turns_label, into_raw).await;
    }

    if p.pty {
        return handoff_to_new_pty(core, &p, &src, &cwd, &pkg, &mut lineage, turns_label).await;
    }

    handoff_to_new_acp(core, &p, &src, &cwd, &pkg, &lineage, turns_label).await
}

async fn collect_source(core: &Arc<Core>, source: &str) -> Result<SourceSnapshot, RpcError> {
    match resolve_any(core, source)? {
        TargetKey::Agent(id) => {
            let (address, cwd, provider, session_id, model, argv) = {
                let st = core.lock();
                let a = st.reg.agents.get(&id).ok_or_else(|| {
                    RpcError::new("not_found", format!("{} not found", crate::agents::address(id)), "")
                })?;
                (
                    a.address.clone(),
                    a.cwd.clone(),
                    a.agent.clone(),
                    a.acp_session_id.clone(),
                    a.model.clone(),
                    a.argv.clone(),
                )
            };
            let entries = core.read_transcript_entries(id)?;
            Ok(SourceSnapshot {
                address,
                cwd,
                provider,
                session_id,
                model,
                argv,
                is_agent: true,
                cols: 120,
                rows: 36,
                entries,
            })
        }
        TargetKey::Terminal(id) => {
            let (address, cwd, cols, rows, argv, snap, fallback_kind, fallback_sid) = {
                let mut st = core.lock();
                let Some(t) = st.pty_reg.terminals.get(&id).cloned() else {
                    return Err(RpcError::new(
                        "not_found",
                        format!("{} not found", crate::terminals::address(id)),
                        "",
                    ));
                };
                let snap = snapshot_from_terminal(&t, st.pty_prompts.get(&id));
                let tc = core.target_checkpoints(&mut st, &t.address);
                (
                    t.address,
                    t.cwd,
                    t.cols,
                    t.rows,
                    t.argv,
                    snap,
                    tc.cli_kind,
                    tc.cli_session_id,
                )
            };
            let (entries, session_meta) = blocking(move || Ok(extract_transcript(&snap))).await?;
            let provider = session_meta
                .as_ref()
                .and_then(|m| m.provider.clone())
                .or(fallback_kind);
            let session_id = session_meta
                .as_ref()
                .and_then(|m| m.session_id.clone())
                .or(fallback_sid);
            let model = session_meta.and_then(|m| m.model);
            Ok(SourceSnapshot {
                address,
                cwd,
                provider,
                session_id,
                model,
                argv,
                is_agent: false,
                cols,
                rows,
                entries,
            })
        }
    }
}

async fn handoff_into_existing(
    core: Ctx<'_>,
    p: &HandoffParams,
    src: &SourceSnapshot,
    pkg: &crate::handoff::HandoffPackage,
    lineage: &Lineage,
    turns_label: Option<String>,
    into_raw: &str,
) -> Result<HandoffResult, RpcError> {
    let clean_prompt = p.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    match resolve_any(core, into_raw)? {
        TargetKey::Agent(dst_id) => {
            let dst_addr = crate::agents::address(dst_id);
            let wrapped = wrap_handoff_envelope(p.kind, &src.address, &pkg.markdown);
            {
                let mut st = core.lock();
                let Some(turn) = st.reg.agents.get(&dst_id).map(|a| a.turn) else {
                    return Err(RpcError::new("not_found", format!("{dst_addr} was removed"), ""));
                };
                let mut tc = core.target_checkpoints(&mut st, &dst_addr);
                tc.pending_handoff = Some(wrapped);
                core.save_target_checkpoints(&mut st, tc);
                core.record_transcript(
                    &mut st,
                    dst_id,
                    Entry::Handoff {
                        turn,
                        handoff_kind: p.kind.as_str().into(),
                        from_address: src.address.clone(),
                        from_provider: src.provider.clone(),
                        from_session_id: src.session_id.clone(),
                        from_turns: turns_label,
                        artifact_path: None,
                        ts_ms: now_ms(),
                    },
                );
                core.save(&st);
            }
            core.flush_saved().await?;

            let prompt_text = clean_prompt.unwrap_or_else(|| {
                format!(
                    "[{} from {}] Please review and incorporate the incoming handoff context above, then summarize key takeaways or next steps.",
                    p.kind.display_label(),
                    src.address
                )
            });
            let sent = agent_send(
                core,
                SendParams {
                    agent: dst_addr.clone(),
                    text: prompt_text,
                    steer: false,
                    only_if: None,
                    wait: p.wait,
                    timeout: p.timeout,
                    from: None,
                    caller: p.caller.clone(),
                },
            )
            .await?;
            let prompt_sent = sent.started != Some(false);
            let wait_res = sent.wait;

            Ok(HandoffResult {
                target: dst_addr,
                target_kind: "agent".into(),
                kind: p.kind,
                source: src.address.clone(),
                lineage: lineage.clone(),
                included_turns: pkg.included_turns.clone(),
                omitted_turns: pkg.omitted_turns.clone(),
                bytes: pkg.bytes,
                artifact_path: None,
                native_fork: false,
                prompt_sent,
                wait: wait_res,
            })
        }
        TargetKey::Terminal(dst_id) => {
            let dst_addr = crate::terminals::address(dst_id);
            let filename = format!("handoff-from-{}.md", src.address.replace('/', "_"));
            let staged_path =
                crate::put::stage_terminal(&core.paths.state, dst_id, &filename, pkg.markdown.as_bytes()).map_err(
                    |e| RpcError::internal(format!("failed to stage handoff markdown for {dst_addr}: {e}")),
                )?;
            let path_str = staged_path.display().to_string();

            let mut pointer = format!(
                "[{} from {}] Context handoff saved at: {}",
                p.kind.display_label(),
                src.address,
                path_str
            );
            if let Some(prompt_text) = clean_prompt {
                pointer.push('\n');
                pointer.push_str(&prompt_text);
            } else {
                pointer.push_str("\nRead that file for the incoming context package before continuing.");
            }

            let sent = core.pty_send(dst_id, &pointer, false, None).await?;
            core.clear_manual_status_key(TargetKey::Terminal(dst_id));
            core.clear_manual_status_caller(p.caller.as_deref());

            let wait_res = if p.wait {
                if sent.started == Some(false) {
                    Some(core.wait_result(TargetKey::Terminal(dst_id), false, "not_started", None))
                } else {
                    let timeout = wait_timeout(core, p.timeout)?;
                    let spec = WaitSpec { output: None, timeout: Some(timeout), after_turn: Some(sent.turn) };
                    Some(watched(core, p.caller.clone(), TargetKey::Terminal(dst_id), spec).await)
                }
            } else {
                None
            };

            let mut out_lineage = lineage.clone();
            out_lineage.artifact_path = Some(path_str.clone());
            Ok(HandoffResult {
                target: dst_addr,
                target_kind: "terminal".into(),
                kind: p.kind,
                source: src.address.clone(),
                lineage: out_lineage,
                included_turns: pkg.included_turns.clone(),
                omitted_turns: pkg.omitted_turns.clone(),
                bytes: pkg.bytes,
                artifact_path: Some(path_str),
                native_fork: false,
                prompt_sent: sent.started != Some(false),
                wait: wait_res,
            })
        }
    }
}

async fn handoff_to_new_pty(
    core: Ctx<'_>,
    p: &HandoffParams,
    src: &SourceSnapshot,
    cwd: &str,
    pkg: &crate::handoff::HandoffPackage,
    lineage: &mut Lineage,
    _turns_label: Option<String>,
) -> Result<HandoffResult, RpcError> {
    let preset = p
        .preset
        .clone()
        .or_else(|| src.provider.clone())
        .unwrap_or_else(|| "claude".into());
    let base_argv = if !p.argv.is_empty() {
        p.argv.clone()
    } else if !src.is_agent && p.preset.is_none() && !src.argv.is_empty() {
        src.argv.clone()
    } else {
        default_pty_argv(&preset)
    };

    let target_cli_kind = PtyCliKind::from_str_name(&preset.to_ascii_lowercase());
    let mut spawn_argv = base_argv.clone();
    let mut native_fork = false;

    if p.kind == LineageKind::Fork
        && p.argv.is_empty()
        && cwd == src.cwd
        && matches!(target_cli_kind, PtyCliKind::Claude | PtyCliKind::Codex)
        && src.provider.as_deref() == Some(target_cli_kind.as_str())
        && let Some(ref sid) = src.session_id
    {
        let src_cwd = src.cwd.clone();
        let sid_owned = sid.clone();
        let to_turn = p.to_turn.unwrap_or_else(|| pkg.total_turns.max(1));
        let fork_uuid = crate::checkpoint::random_uuid_v4();
        let fork_uuid_bg = fork_uuid.clone();
        let fork_res = blocking(move || {
            let file = match target_cli_kind {
                PtyCliKind::Claude => find_claude_session_file(&src_cwd, &sid_owned),
                PtyCliKind::Codex => find_codex_rollout_by_sid(&src_cwd, &sid_owned),
                _ => None,
            };
            let Some(src_file) = file else {
                return Ok(false);
            };
            let res = match target_cli_kind {
                PtyCliKind::Claude => fork_claude_jsonl(&src_file, to_turn, &fork_uuid_bg),
                PtyCliKind::Codex => fork_codex_jsonl(&src_file, to_turn, &fork_uuid_bg),
                _ => return Ok(false),
            };
            Ok(res.is_ok())
        })
        .await;

        if matches!(fork_res, Ok(true)) {
            let to_turn = p.to_turn.unwrap_or_else(|| pkg.total_turns.max(1));
            let h2_exe = h2_exe_string();
            spawn_argv = build_rollback_argv(
                target_cli_kind,
                &base_argv,
                &core.hooks_dir(),
                &h2_exe,
                to_turn,
                Some(&fork_uuid),
            );
            native_fork = true;
        }
    }

    let term_res = pty::terminal_run(
        core,
        TerminalRunParams {
            argv: spawn_argv,
            cwd: Some(cwd.to_string()),
            name: p.name.clone(),
            keep: p.keep,
            parent: Some(src.address.clone()),
            caller: p.caller.clone(),
            size: Some(InitialSize {
                cols: p.cols.unwrap_or(src.cols.max(40)),
                rows: p.rows.unwrap_or(src.rows.max(12)),
            }),
            lineage: Some(lineage.clone()),
            ..Default::default()
        },
    )
    .await?;

    let new_addr = term_res.terminal.0.address.clone();
    let new_id = pty::resolve_pty(core, &new_addr)?;
    let clean_prompt = p.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);

    let mut artifact_path = None;
    let mut prompt_sent = false;
    let mut after_turn = None;
    let use_quiet = !matches!(target_cli_kind, PtyCliKind::Claude | PtyCliKind::Codex | PtyCliKind::Agy) || !p.argv.is_empty();

    if native_fork {
        if let Some(prompt_text) = clean_prompt {
            let msg = match p.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(n) => format!("[Note: {n}]\n{prompt_text}"),
                None => prompt_text,
            };
            let fm = core.pty_first_message(new_id, use_quiet, &msg, p.caller.as_deref()).await;
            prompt_sent = fm.sent;
            after_turn = fm.turn;
        }
    } else {
        let staged = crate::put::stage_terminal(&core.paths.state, new_id, "handoff.md", pkg.markdown.as_bytes())
            .map_err(|e| RpcError::internal(format!("failed to stage handoff markdown for {new_addr}: {e}")))?;
        let path_str = staged.display().to_string();
        lineage.artifact_path = Some(path_str.clone());
        artifact_path = Some(path_str.clone());
        {
            let mut st = core.lock();
            if let Some(t) = st.pty_reg.terminals.get_mut(&new_id) {
                t.lineage = Some(lineage.clone());
            }
            core.save_pty(&st);
        }
        core.flush_saved().await?;

        let mut pointer = format!(
            "[{} from {}] Read the full context handoff at: {}",
            p.kind.display_label(),
            src.address,
            path_str
        );
        if let Some(prompt_text) = clean_prompt {
            pointer.push('\n');
            pointer.push_str(&prompt_text);
        } else {
            pointer.push_str("\nRead that file first for full context and git state before continuing.");
        }
        let fm = core.pty_first_message(new_id, use_quiet, &pointer, p.caller.as_deref()).await;
        prompt_sent = fm.sent;
        after_turn = fm.turn;
    }

    let wait_res = if p.wait && prompt_sent {
        let timeout = wait_timeout(core, p.timeout)?;
        let spec = WaitSpec { output: None, timeout: Some(timeout), after_turn };
        Some(watched(core, p.caller.clone(), TargetKey::Terminal(new_id), spec).await)
    } else {
        None
    };

    Ok(HandoffResult {
        target: new_addr,
        target_kind: "terminal".into(),
        kind: p.kind,
        source: src.address.clone(),
        lineage: lineage.clone(),
        included_turns: pkg.included_turns.clone(),
        omitted_turns: pkg.omitted_turns.clone(),
        bytes: pkg.bytes,
        artifact_path,
        native_fork,
        prompt_sent,
        wait: wait_res,
    })
}

async fn handoff_to_new_acp(
    core: Ctx<'_>,
    p: &HandoffParams,
    src: &SourceSnapshot,
    cwd: &str,
    pkg: &crate::handoff::HandoffPackage,
    lineage: &Lineage,
    turns_label: Option<String>,
) -> Result<HandoffResult, RpcError> {
    let (target_agent, target_argv) = if !p.argv.is_empty() {
        (p.preset.clone(), p.argv.clone())
    } else if let Some(preset) = p.preset.clone() {
        (Some(preset), Vec::new())
    } else if let Some(provider) = src.provider.clone() {
        (Some(provider), Vec::new())
    } else if src.is_agent && !src.argv.is_empty() {
        (None, src.argv.clone())
    } else {
        (Some("claude".into()), Vec::new())
    };

    let clean_prompt = p.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let keep = p.keep || clean_prompt.is_none();

    let run_res = run::agent_run(
        core,
        RunParams {
            agent: target_agent,
            argv: target_argv,
            prompt: None,
            cwd: Some(cwd.to_string()),
            name: p.name.clone(),
            model: p.model.clone(),
            keep,
            parent: Some(src.address.clone()),
            caller: p.caller.clone(),
            wait: false,
            lineage: Some(lineage.clone()),
            ..Default::default()
        },
    )
    .await?;

    let new_addr = run_res.agent.0.address.clone();
    let new_id = resolve(core, &new_addr)?;
    let wrapped = wrap_handoff_envelope(p.kind, &src.address, &pkg.markdown);

    {
        let mut st = core.lock();
        let mut tc = core.target_checkpoints(&mut st, &new_addr);
        tc.pending_handoff = Some(wrapped);
        core.save_target_checkpoints(&mut st, tc);
        core.record_transcript(
            &mut st,
            new_id,
            Entry::Handoff {
                turn: 0,
                handoff_kind: p.kind.as_str().into(),
                from_address: src.address.clone(),
                from_provider: src.provider.clone(),
                from_session_id: src.session_id.clone(),
                from_turns: turns_label,
                artifact_path: None,
                ts_ms: now_ms(),
            },
        );
        core.save(&st);
    }
    core.flush_saved().await?;

    let mut prompt_sent = false;
    let mut wait_res = None;
    if let Some(prompt_text) = clean_prompt {
        let sent = agent_send(
            core,
            SendParams {
                agent: new_addr.clone(),
                text: prompt_text,
                steer: false,
                only_if: None,
                wait: p.wait,
                timeout: p.timeout,
                from: None,
                caller: p.caller.clone(),
            },
        )
        .await?;
        prompt_sent = sent.started != Some(false);
        wait_res = sent.wait;
    }

    Ok(HandoffResult {
        target: new_addr,
        target_kind: "agent".into(),
        kind: p.kind,
        source: src.address.clone(),
        lineage: lineage.clone(),
        included_turns: pkg.included_turns.clone(),
        omitted_turns: pkg.omitted_turns.clone(),
        bytes: pkg.bytes,
        artifact_path: None,
        native_fork: false,
        prompt_sent,
        wait: wait_res,
    })
}
