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
    lineage: Option<Lineage>,
}

pub(super) async fn agent_handoff(core: Ctx<'_>, p: HandoffParams) -> Result<HandoffResult, RpcError> {
    not_stopping(&core.lock())?;

    if let (Some(from), Some(to)) = (p.from_turn, p.to_turn)
        && from > to
    {
        return Err(RpcError::invalid_params("from_turn must be <= to_turn"));
    }

    let src = collect_source(core, &p.source).await?;
    let origin_cwd = p
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&src.cwd)
        .to_string();

    let mut cwd = origin_cwd.clone();
    let mut created_worktree: Option<(String, String, String)> = None;
    if p.into.is_none() && p.kind == LineageKind::Fork && p.worktree {
        let state_dir = core.paths.state.clone();
        let origin_bg = origin_cwd.clone();
        let tag = crate::drove::crypto::rand_id("fk");
        let (wt_path, wt_branch) = blocking(move || {
            crate::drove::integrate::create_fork_worktree(&state_dir, &origin_bg, &tag).map_err(|msg| {
                if msg.starts_with("WORKTREE_BLOCKED:") {
                    RpcError::new("worktree_blocked", msg, "")
                } else {
                    RpcError::invalid_params(msg)
                }
            })
        })
        .await?;
        cwd = wt_path.clone();
        created_worktree = Some((origin_cwd.clone(), wt_path, wt_branch));
    }

    let cwd_for_pkg = cwd.clone();
    let wt_origin_for_pkg = created_worktree.as_ref().map(|(orig, _, _)| orig.clone());
    let entries_for_pkg = src.entries.clone();
    let kind = p.kind;
    let src_addr_pkg = src.address.clone();
    let src_prov_pkg = src.provider.clone();
    let src_sid_pkg = src.session_id.clone();
    let src_model_pkg = src.model.clone();
    let to_turn = p.to_turn;
    let from_turn = match (p.from_turn, p.recent_turns) {
        (Some(f), _) => Some(f),
        (None, Some(n)) if n > 0 => resolve_recent_from_turn(&src.entries, to_turn, n),
        _ => None,
    };
    let max_bytes = p.max_bytes.unwrap_or(DEFAULT_HANDOFF_MAX_BYTES);
    let note_pkg = p.note.clone();
    let is_zh = p
        .language
        .as_deref()
        .is_some_and(|l| l.trim().to_ascii_lowercase().starts_with("zh"));

    let mut pkg = {
        let cwd_for_pkg = cwd_for_pkg.clone();
        let wt_origin_for_pkg = wt_origin_for_pkg.clone();
        let entries_for_pkg = entries_for_pkg.clone();
        let src_addr_pkg = src_addr_pkg.clone();
        let src_prov_pkg = src_prov_pkg.clone();
        let src_sid_pkg = src_sid_pkg.clone();
        let src_model_pkg = src_model_pkg.clone();
        let note_pkg = note_pkg.clone();
        blocking(move || {
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
                max_bytes,
                note: note_pkg.as_deref(),
                is_zh,
                worktree_origin: wt_origin_for_pkg.as_deref(),
                omitted_summary: None,
            }))
        })
        .await?
    };

    let mut distilled = false;
    if !p.raw && p.into.is_none() && !pkg.omitted_turns.is_empty() && core.ai.handoff_distill_enabled() {
        if let Some(summary) = crate::ai::distill_omitted_turns(
            &core.ai,
            &src.address,
            &src.entries,
            &pkg.omitted_turns,
            is_zh,
        )
        .await
        {
            let summary_bg = summary;
            pkg = blocking(move || {
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
                    max_bytes,
                    note: note_pkg.as_deref(),
                    is_zh,
                    worktree_origin: wt_origin_for_pkg.as_deref(),
                    omitted_summary: Some(&summary_bg),
                }))
            })
            .await?;
            distilled = true;
        }
    } else if !p.raw && p.into.is_some() && core.ai.handoff_distill_enabled() {
        if let Some(merged_md) = crate::ai::distill_merge_back(
            &core.ai,
            &src.address,
            p.kind,
            &src.entries,
            pkg.bytes,
            is_zh,
        )
        .await
        {
            pkg.bytes = merged_md.len();
            pkg.markdown = merged_md;
            distilled = true;
        }
    }

    let turns_label = (!pkg.included_turns.is_empty()).then(|| format_turn_ranges(&pkg.included_turns));
    let mut lineage = Lineage {
        kind: p.kind,
        parent: src.address.clone(),
        parent_provider: src.provider.clone(),
        parent_session_id: src.session_id.clone(),
        from_turn: from_turn.or_else(|| pkg.included_turns.first().copied()),
        to_turn: p.to_turn.or_else(|| pkg.included_turns.last().copied()),
        artifact_path: None,
        worktree_path: created_worktree.as_ref().map(|(_, wt, _)| wt.clone()),
        worktree_origin: created_worktree.as_ref().map(|(orig, _, _)| orig.clone()),
        worktree_branch: created_worktree.as_ref().map(|(_, _, br)| br.clone()),
        worktree_merged: false,
    };

    if let Some(into_raw) = p.into.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return handoff_into_existing(core, &p, &src, &pkg, &lineage, turns_label, into_raw, distilled).await;
    }

    let res = if p.pty {
        handoff_to_new_pty(core, &p, &src, &cwd, &pkg, &mut lineage, turns_label, distilled).await
    } else {
        handoff_to_new_acp(core, &p, &src, &cwd, &pkg, &lineage, turns_label, distilled).await
    };
    if res.is_err()
        && let Some((orig, wt, br)) = created_worktree
    {
        let _ = tokio::task::spawn_blocking(move || {
            crate::drove::integrate::cleanup_fork_worktree(&orig, std::path::Path::new(&wt), Some(&br));
        })
        .await;
    }
    res
}

pub(super) async fn ai_handoff_recommend(
    core: Ctx<'_>,
    p: crate::ai::HandoffRecommendParams,
) -> Result<crate::ai::HandoffRecommendResult, RpcError> {
    not_stopping(&core.lock())?;
    let src = collect_source(core, &p.source).await?;
    Ok(crate::ai::recommend_handoff(
        &core.ai,
        &src.address,
        src.provider.as_deref(),
        src.model.as_deref(),
        &src.entries,
        &p.installed_presets,
        p.language.as_deref(),
    )
    .await)
}

async fn collect_source(core: &Arc<Core>, source: &str) -> Result<SourceSnapshot, RpcError> {
    match resolve_any(core, source)? {
        TargetKey::Agent(id) => {
            let (address, cwd, provider, session_id, model, argv, lineage) = {
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
                    a.lineage.clone(),
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
                lineage,
            })
        }
        TargetKey::Terminal(id) => {
            let (address, cwd, cols, rows, argv, snap, fallback_kind, fallback_sid, lineage) = {
                let mut st = core.lock();
                let Some(t) = st.pty_reg.terminals.get(&id).cloned() else {
                    return Err(RpcError::new(
                        "not_found",
                        format!("{} not found", crate::terminals::address(id)),
                        "",
                    ));
                };
                let mut snap = snapshot_from_terminal(&t, st.pty_prompts.get(&id));
                crate::pty_transcript::populate_session_affinity(&mut snap, &t.address, core, &mut st);
                let tc = core.target_checkpoints(&mut st, &t.address);
                let live_cwd = t
                    .foreground
                    .as_ref()
                    .map(|f| f.cwd.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| t.last_cwd.as_deref().filter(|s| !s.is_empty()))
                    .unwrap_or(&t.cwd)
                    .to_string();
                (
                    t.address,
                    live_cwd,
                    t.cols,
                    t.rows,
                    t.argv,
                    snap,
                    tc.cli_kind,
                    tc.cli_session_id,
                    t.lineage,
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
                lineage,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handoff_into_existing(
    core: Ctx<'_>,
    p: &HandoffParams,
    src: &SourceSnapshot,
    pkg: &crate::handoff::HandoffPackage,
    lineage: &Lineage,
    turns_label: Option<String>,
    into_raw: &str,
    distilled: bool,
) -> Result<HandoffResult, RpcError> {
    let mut merge_instruction = String::new();
    let mut merged_wt_path: Option<String> = None;
    let mut merged_wt_branch: Option<String> = None;
    let mut worktree_merge_requested = false;
    if p.merge_worktree
        && let Some(ref src_lin) = src.lineage
        && let (Some(wt_path), Some(origin_cwd)) = (
            src_lin.worktree_path.as_deref().filter(|s| !s.trim().is_empty()),
            src_lin.worktree_origin.as_deref().filter(|s| !s.trim().is_empty()),
        )
    {
        let origin_bg = origin_cwd.to_string();
        let wt_bg = wt_path.to_string();
        let branch_bg = src_lin.worktree_branch.clone();
        let summary = blocking(move || {
            Ok(crate::drove::integrate::inspect_fork_worktree(
                &origin_bg,
                &wt_bg,
                branch_bg.as_deref(),
            ))
        })
        .await?;

        let branch_display = if summary.branch.is_empty() {
            src_lin.worktree_branch.clone().unwrap_or_else(|| "(unknown)".into())
        } else {
            summary.branch.clone()
        };

        let is_zh = p
            .language
            .as_deref()
            .is_some_and(|l| l.trim().to_ascii_lowercase().starts_with("zh"));
        let mut msg = if is_zh {
            format!(
                "[合回来自 {} 的 Worktree] 请将该分叉 Worktree 中的代码改动合并到当前主分支/工作区（`{}`）：\n- 分叉分支：`{}`\n- 分叉 Worktree 路径：`{}`",
                src.address, origin_cwd, branch_display, wt_path
            )
        } else {
            format!(
                "[Merge Worktree from {}] Please merge the code changes from this forked worktree into the current main branch/workspace (`{}`):\n- Fork branch: `{}`\n- Fork worktree path: `{}`",
                src.address, origin_cwd, branch_display, wt_path
            )
        };
        if !summary.ahead_commits.is_empty() {
            msg.push_str(if is_zh { "\n- 分叉分支上的提交：\n" } else { "\n- Commits on fork branch:\n" });
            for c in &summary.ahead_commits {
                msg.push_str(&format!("  - {c}\n"));
            }
        }
        if !summary.dirty_files.is_empty() {
            msg.push_str(if is_zh {
                "\n- 分叉 Worktree 中的未提交改动：\n"
            } else {
                "\n- Uncommitted changes inside the fork worktree:\n"
            });
            for f in &summary.dirty_files {
                msg.push_str(&format!("  - {f}\n"));
            }
            msg.push_str(if is_zh {
                "  （注意：合并时请一并包含该 Worktree 中的这些未提交改动。）"
            } else {
                "  (Note: include these uncommitted changes from the fork worktree when merging, e.g. by committing or applying them from that worktree.)"
            });
        }
        msg.push_str(&if is_zh {
            format!(
                "\n- 重要：请仅将代码改动合并到主分支/工作区，不要删除 Worktree 目录（`{wt_path}`），也不要删除 `{branch_display}` 分支。"
            )
        } else {
            format!(
                "\n- IMPORTANT: ONLY merge the code changes into the main branch/workspace. Do NOT delete or remove the worktree directory (`{wt_path}`) and do NOT delete the `{branch_display}` branch."
            )
        });

        merge_instruction = msg;
        merged_wt_path = Some(wt_path.to_string());
        merged_wt_branch = (!branch_display.is_empty() && branch_display != "(unknown)").then_some(branch_display);
        worktree_merge_requested = true;

        // Mark worktree_merged = true on the source record while keeping worktree_path and branch intact.
        if let Ok(key) = resolve_any(core, &src.address) {
            let mut st = core.lock();
            match key {
                TargetKey::Agent(aid) => {
                    if let Some(a) = st.reg.agents.get_mut(&aid)
                        && let Some(ref mut lin) = a.lineage
                    {
                        lin.worktree_merged = true;
                        if lin.worktree_branch.is_none() {
                            lin.worktree_branch = merged_wt_branch.clone();
                        }
                    }
                    core.save(&st);
                }
                TargetKey::Terminal(tid) => {
                    if let Some(t) = st.pty_reg.terminals.get_mut(&tid)
                        && let Some(ref mut lin) = t.lineage
                    {
                        lin.worktree_merged = true;
                        if lin.worktree_branch.is_none() {
                            lin.worktree_branch = merged_wt_branch.clone();
                        }
                    }
                    core.save_pty(&st);
                }
            }
        }
        core.flush_saved().await?;
    }

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
                        context_markdown: (!pkg.markdown.trim().is_empty()).then(|| pkg.markdown.clone()),
                        ts_ms: now_ms(),
                    },
                );
                core.save(&st);
            }
            core.flush_saved().await?;

            let prompt_text = if !merge_instruction.is_empty() {
                match clean_prompt {
                    Some(user_note) => format!("{merge_instruction}\n\n{user_note}"),
                    None => merge_instruction.clone(),
                }
            } else {
                clean_prompt.unwrap_or_else(|| {
                    let is_zh = p
                        .language
                        .as_deref()
                        .is_some_and(|l| l.trim().to_ascii_lowercase().starts_with("zh"));
                    if is_zh {
                        format!(
                            "[来自 {} 的{}] 请阅读并吸收上述合回的上下文与结论，总结关键要点或下一步建议。",
                            src.address,
                            p.kind.display_label_zh()
                        )
                    } else {
                        format!(
                            "[{} from {}] Please review and incorporate the incoming handoff context above, then summarize key takeaways or next steps.",
                            p.kind.display_label(),
                            src.address
                        )
                    }
                })
            };
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
                distilled,
                prompt_sent,
                worktree_path: merged_wt_path,
                worktree_branch: merged_wt_branch,
                worktree_merge_requested,
                merged_files: Vec::new(),
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

            let is_zh = p
                .language
                .as_deref()
                .is_some_and(|l| l.trim().to_ascii_lowercase().starts_with("zh"));
            let mut pointer = if is_zh {
                format!(
                    "[来自 {} 的{}] 上下文交接文件已保存至：{}",
                    src.address,
                    p.kind.display_label_zh(),
                    path_str
                )
            } else {
                format!(
                    "[{} from {}] Context handoff saved at: {}",
                    p.kind.display_label(),
                    src.address,
                    path_str
                )
            };
            if !merge_instruction.is_empty() {
                pointer.push('\n');
                pointer.push_str(&merge_instruction);
                if let Some(prompt_text) = clean_prompt {
                    pointer.push('\n');
                    pointer.push_str(&prompt_text);
                }
            } else if let Some(prompt_text) = clean_prompt {
                pointer.push('\n');
                pointer.push_str(&prompt_text);
            } else if is_zh {
                pointer.push_str("\n请先读取该文件了解合回的上下文内容后再继续。");
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
                distilled,
                prompt_sent: sent.started != Some(false),
                worktree_path: merged_wt_path,
                worktree_branch: merged_wt_branch,
                worktree_merge_requested,
                merged_files: Vec::new(),
                wait: wait_res,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handoff_to_new_pty(
    core: Ctx<'_>,
    p: &HandoffParams,
    src: &SourceSnapshot,
    cwd: &str,
    pkg: &crate::handoff::HandoffPackage,
    lineage: &mut Lineage,
    _turns_label: Option<String>,
    distilled: bool,
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

    let inherited_display = p.display.clone().or_else(|| {
        if src.is_agent {
            return None;
        }
        let src_id = pty::resolve_pty(core, &src.address).ok()?;
        let st = core.lock();
        core.active_viewer_display(&st, src_id)
    });

    let mut env = BTreeMap::new();
    if let (Some(wt_path), Some(wt_origin)) = (
        lineage.worktree_path.as_deref().filter(|s| !s.trim().is_empty()),
        lineage.worktree_origin.as_deref().filter(|s| !s.trim().is_empty()),
    ) {
        env.insert("H2_WORKTREE_PATH".into(), wt_path.to_string());
        env.insert("H2_WORKTREE_ORIGIN".into(), wt_origin.to_string());
    }

    let term_res = pty::terminal_run(
        core,
        TerminalRunParams {
            argv: spawn_argv,
            cwd: Some(cwd.to_string()),
            name: p.name.clone(),
            keep: p.keep,
            env,
            parent: (p.kind == LineageKind::SideChat).then(|| src.address.clone()),
            caller: p.caller.clone(),
            size: Some(InitialSize {
                cols: p.cols.unwrap_or(src.cols.max(40)),
                rows: p.rows.unwrap_or(src.rows.max(12)),
            }),
            display: inherited_display,
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
    let mut initial_msg: Option<String> = None;

    if native_fork {
        if let Some(prompt_text) = clean_prompt {
            let msg = match p.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(n) => format!("[Note: {n}]\n{prompt_text}"),
                None => prompt_text,
            };
            initial_msg = Some(msg);
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

        let is_zh = p
            .language
            .as_deref()
            .is_some_and(|l| l.trim().to_ascii_lowercase().starts_with("zh"));
        let mut pointer = if is_zh {
            format!(
                "[来自 {} 的{}] 请先读取完整的上下文交接文件：{}",
                src.address,
                p.kind.display_label_zh(),
                path_str
            )
        } else {
            format!(
                "[{} from {}] Read the full context handoff at: {}",
                p.kind.display_label(),
                src.address,
                path_str
            )
        };
        if let Some(prompt_text) = clean_prompt {
            pointer.push('\n');
            pointer.push_str(&prompt_text);
        } else if p.kind == LineageKind::SideChat {
            if is_zh {
                pointer.push_str("\n请先读取该文件以消化上下文。本轮仅读取交接上下文，不要做任何文件修改或执行变更类命令，简短确认已理解后等待用户的下一步提问。（当前为只读旁路讨论，除非明确要求，否则不要修改工作区文件。）");
            } else {
                pointer.push_str("\nRead that file first to absorb the context ONLY. Do NOT make any modifications, edit files, or run mutating commands on this turn — briefly confirm you have read the context and wait for the user's next instruction. (This is a read-only side-chat discussion: do NOT modify workspace files unless explicitly requested.)");
            }
        } else if is_zh {
            pointer.push_str("\n请先读取该文件以消化交接上下文。本轮仅读取交接的上下文，不要做任何修改、不要编辑文件或执行变更类命令，简短确认已理解后等待用户的下一步指令。");
        } else {
            pointer.push_str("\nRead that file first to absorb the handoff context ONLY. Do NOT make any modifications, edit files, or run mutating commands on this turn — briefly confirm you have understood the context and wait for the user's next instruction.");
        }
        if let Some(wt_notice) = worktree_workspace_notice(lineage, is_zh) {
            pointer.push('\n');
            pointer.push_str(&wt_notice);
        }
        initial_msg = Some(pointer);
    }

    if let Some(msg) = initial_msg {
        if p.wait {
            let fm = core.pty_first_message(new_id, use_quiet, &msg, p.caller.as_deref()).await;
            prompt_sent = fm.sent;
            after_turn = fm.turn;
        } else {
            // Return immediately so UI can mount the new tab/split right away (avoiding a 30s RPC
            // timeout deadlock if the CLI shows an interactive startup prompt such as Codex update).
            let core_bg = Arc::clone(core);
            let caller_bg = p.caller.clone();
            tokio::spawn(async move {
                let _ = core_bg.pty_first_message(new_id, use_quiet, &msg, caller_bg.as_deref()).await;
            });
            prompt_sent = true;
        }
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
        worktree_path: lineage.worktree_path.clone(),
        worktree_branch: lineage.worktree_branch.clone(),
        worktree_merge_requested: false,
        merged_files: Vec::new(),
        lineage: lineage.clone(),
        included_turns: pkg.included_turns.clone(),
        omitted_turns: pkg.omitted_turns.clone(),
        bytes: pkg.bytes,
        artifact_path,
        native_fork,
        distilled,
        prompt_sent,
        wait: wait_res,
    })
}

fn worktree_workspace_notice(lineage: &Lineage, is_zh: bool) -> Option<String> {
    let wt_path = lineage.worktree_path.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let wt_origin = lineage.worktree_origin.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let branch_suffix_zh = lineage
        .worktree_branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|b| format!("（分支 `{b}`）"))
        .unwrap_or_default();
    let branch_suffix_en = lineage
        .worktree_branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|b| format!(" (branch `{b}`)"))
        .unwrap_or_default();
    Some(if is_zh {
        format!(
            "[Worktree 工作区声明] 当前你的工作区位于独立的 Git Worktree 目录 `{wt_path}`{branch_suffix_zh}，而不是父 Agent 所在的原目录 `{wt_origin}`。后续所有代码读取、修改、测试与命令都必须严格限定在 `{wt_path}` 中执行，严禁访问或修改父工作区 `{wt_origin}`。"
        )
    } else {
        format!(
            "[Worktree Workspace] Your workspace is an isolated Git worktree at `{wt_path}`{branch_suffix_en}, NOT the parent agent's directory `{wt_origin}`. All subsequent file reads, edits, tests, and commands MUST stay strictly inside `{wt_path}`; NEVER access or modify `{wt_origin}`."
        )
    })
}

#[allow(clippy::too_many_arguments)]
async fn handoff_to_new_acp(
    core: Ctx<'_>,
    p: &HandoffParams,
    src: &SourceSnapshot,
    cwd: &str,
    pkg: &crate::handoff::HandoffPackage,
    lineage: &Lineage,
    turns_label: Option<String>,
    distilled: bool,
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
    let is_zh = p
        .language
        .as_deref()
        .is_some_and(|l| l.trim().to_ascii_lowercase().starts_with("zh"));

    let mut env = BTreeMap::new();
    if let (Some(wt_path), Some(wt_origin)) = (
        lineage.worktree_path.as_deref().filter(|s| !s.trim().is_empty()),
        lineage.worktree_origin.as_deref().filter(|s| !s.trim().is_empty()),
    ) {
        env.insert("H2_WORKTREE_PATH".into(), wt_path.to_string());
        env.insert("H2_WORKTREE_ORIGIN".into(), wt_origin.to_string());
    }
    let system_prompt = worktree_workspace_notice(lineage, is_zh);

    let run_res = run::agent_run(
        core,
        RunParams {
            agent: target_agent,
            argv: target_argv,
            prompt: None,
            system_prompt,
            cwd: Some(cwd.to_string()),
            name: p.name.clone(),
            model: p.model.clone(),
            keep,
            env,
            parent: (p.kind == LineageKind::SideChat).then(|| src.address.clone()),
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
                context_markdown: (!pkg.markdown.trim().is_empty()).then(|| pkg.markdown.clone()),
                ts_ms: now_ms(),
            },
        );
        core.save(&st);
    }
    core.flush_saved().await?;

    let prompt_text = clean_prompt.unwrap_or_else(|| {
        let mut msg = match p.kind {
            LineageKind::SideChat => {
                if is_zh {
                    format!(
                        "[来自 {} 的旁路讨论] 请先阅读并消化上述携带的交接上下文。本轮仅读取交接的上下文，不要做任何文件修改或执行变更类命令，简短确认已理解后等待用户的下一步提问。（当前为只读旁路讨论，除非明确要求，否则不要修改工作区文件。）",
                        src.address
                    )
                } else {
                    format!(
                        "[Side Chat from {}] Please read and absorb the incoming handoff context above ONLY. Do NOT make any modifications, edit files, or run mutating commands on this turn — briefly confirm you have understood the context and wait for the user's next question. (This is a read-only side-chat discussion: do NOT modify workspace files unless explicitly requested.)",
                        src.address
                    )
                }
            }
            LineageKind::Handoff | LineageKind::Fork => {
                if is_zh {
                    format!(
                        "[来自 {} 的{}] 请先阅读并消化上述携带的交接上下文。本轮仅读取交接的上下文，不要做任何修改、不要编辑文件或执行变更类命令，简短确认已理解后等待用户的下一步指令。",
                        src.address,
                        p.kind.display_label_zh()
                    )
                } else {
                    format!(
                        "[{} from {}] Please read and absorb the incoming handoff context above ONLY. Do NOT make any modifications, edit files, or run mutating commands on this turn — briefly confirm you have understood the context and wait for the user's next instruction.",
                        p.kind.display_label(),
                        src.address
                    )
                }
            }
        };
        if let Some(wt_notice) = worktree_workspace_notice(lineage, is_zh) {
            msg.push('\n');
            msg.push_str(&wt_notice);
        }
        msg
    });

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
    let prompt_sent = sent.started != Some(false);
    let wait_res = sent.wait;

    Ok(HandoffResult {
        target: new_addr,
        target_kind: "agent".into(),
        kind: p.kind,
        source: src.address.clone(),
        worktree_path: lineage.worktree_path.clone(),
        worktree_branch: lineage.worktree_branch.clone(),
        worktree_merge_requested: false,
        merged_files: Vec::new(),
        lineage: lineage.clone(),
        included_turns: pkg.included_turns.clone(),
        omitted_turns: pkg.omitted_turns.clone(),
        bytes: pkg.bytes,
        artifact_path: None,
        native_fork: false,
        distilled,
        prompt_sent,
        wait: wait_res,
    })
}

fn resolve_recent_from_turn(entries: &[Entry], to_turn: Option<u64>, recent_turns: u64) -> Option<u64> {
    if recent_turns == 0 {
        return None;
    }
    let hi = to_turn.unwrap_or(u64::MAX);
    let mut turns: Vec<u64> = entries.iter().filter_map(|e| e.turn().filter(|&t| t > 0 && t <= hi)).collect();
    turns.sort_unstable();
    turns.dedup();
    if turns.is_empty() {
        return None;
    }
    let keep = usize::try_from(recent_turns).unwrap_or(usize::MAX);
    Some(turns[turns.len().saturating_sub(keep)])
}

#[cfg(test)]
mod tests {
    use super::resolve_recent_from_turn;
    use crate::transcript::Entry;

    fn user_entry(turn: u64) -> Entry {
        Entry::User { turn, from: None, text: format!("turn {turn}"), ts_ms: turn }
    }

    #[test]
    fn resolve_recent_from_turn_selects_tail_turns() {
        let entries: Vec<Entry> = (1..=10).map(user_entry).collect();
        assert_eq!(resolve_recent_from_turn(&entries, None, 3), Some(8));
        assert_eq!(resolve_recent_from_turn(&entries, None, 10), Some(1));
        assert_eq!(resolve_recent_from_turn(&entries, None, 25), Some(1));
        assert_eq!(resolve_recent_from_turn(&entries, Some(6), 2), Some(5));
        assert_eq!(resolve_recent_from_turn(&entries, None, 0), None);
        assert_eq!(resolve_recent_from_turn(&[], None, 5), None);
    }

    #[test]
    fn resolve_recent_from_turn_handles_sparse_and_duplicate_turns() {
        let entries = vec![user_entry(1), user_entry(1), user_entry(3), user_entry(7), user_entry(9)];
        assert_eq!(resolve_recent_from_turn(&entries, None, 2), Some(7));
        assert_eq!(resolve_recent_from_turn(&entries, None, 3), Some(3));
    }
}
