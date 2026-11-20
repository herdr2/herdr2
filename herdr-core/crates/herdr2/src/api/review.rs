//! `agent.review`: deliver structured line-level diff review feedback to ACP agents or PTY terminals.

use super::*;
use crate::review::{
    DEFAULT_REVIEW_INLINE_MAX_BYTES, MAX_REVIEW_COMMENTS, ReviewBuildInput, build_review_markdown,
    build_staged_pointer_message,
};
use crate::runtime::TargetKey;

pub(super) async fn agent_review(core: Ctx<'_>, p: ReviewParams) -> Result<ReviewResult, RpcError> {
    not_stopping(&core.lock())?;

    if p.comments.is_empty() {
        return Err(RpcError::invalid_params("comments must contain at least one review comment"));
    }
    if p.comments.len() > MAX_REVIEW_COMMENTS {
        return Err(RpcError::invalid_params(format!(
            "at most {MAX_REVIEW_COMMENTS} review comments per call"
        )));
    }
    for (idx, c) in p.comments.iter().enumerate() {
        if c.path.trim().is_empty() {
            return Err(RpcError::invalid_params(format!("comments[{idx}].path must not be empty")));
        }
        if c.body.trim().is_empty() {
            return Err(RpcError::invalid_params(format!("comments[{idx}].body must not be empty")));
        }
        if c.start_line == 0 {
            return Err(RpcError::invalid_params(format!("comments[{idx}].start_line must be >= 1")));
        }
    }

    let is_zh = p
        .language
        .as_deref()
        .is_some_and(|l| l.trim().to_ascii_lowercase().starts_with("zh"));

    let pkg = build_review_markdown(&ReviewBuildInput {
        source: p.source.as_deref(),
        checkpoint_turn: p.checkpoint_turn,
        repo_root: p.repo_root.as_deref(),
        note: p.note.as_deref(),
        comments: &p.comments,
        is_zh,
    });

    match resolve_any(core, &p.target)? {
        TargetKey::Agent(id) => {
            if !record(core, id)?.is_live() {
                return Err(RpcError::new(
                    "finished",
                    format!("{} has finished", crate::agents::address(id)),
                    "start a new session or choose another live agent",
                ));
            }
            let target_addr = crate::agents::address(id);
            let (delivery_text, staged_path) = if pkg.bytes <= DEFAULT_REVIEW_INLINE_MAX_BYTES {
                (pkg.markdown.clone(), None)
            } else {
                let path = crate::put::stage(&core.paths.state, id, "diff-review.md", pkg.markdown.as_bytes())
                    .map_err(|e| RpcError::internal(format!("failed to stage review for {target_addr}: {e}")))?;
                let path_str = path.display().to_string();
                let ptr = build_staged_pointer_message(
                    &pkg,
                    &path_str,
                    p.note.as_deref(),
                    p.checkpoint_turn,
                    is_zh,
                );
                (ptr, Some(path_str))
            };

            let sender = p.caller.as_deref();
            let sent = core.send(id, &delivery_text, p.steer, None, sender).await?;
            core.clear_manual_status_key(TargetKey::Agent(id));
            core.clear_manual_status_caller(sender);
            core.saw(sender, id);
            core.flush_saved().await?;

            Ok(ReviewResult {
                target: target_addr,
                target_kind: "agent".into(),
                comment_count: pkg.comment_count,
                file_count: pkg.file_count,
                staged_path,
                status: sent.status,
                turn: sent.turn,
                inbox_len: sent.inbox_len,
            })
        }
        TargetKey::Terminal(id) => {
            if !record_pty(core, id)?.is_live() {
                return Err(RpcError::new(
                    "finished",
                    format!("{} has finished", crate::terminals::address(id)),
                    "choose another live terminal or agent",
                ));
            }
            let target_addr = crate::terminals::address(id);
            let staged = crate::put::stage_terminal(&core.paths.state, id, "diff-review.md", pkg.markdown.as_bytes())
                .map_err(|e| RpcError::internal(format!("failed to stage review for {target_addr}: {e}")))?;
            let path_str = staged.display().to_string();
            let pointer =
                build_staged_pointer_message(&pkg, &path_str, p.note.as_deref(), p.checkpoint_turn, is_zh);

            let sender = p.caller.as_deref();
            let sent = core.pty_send(id, &pointer, p.steer, None).await?;
            core.clear_manual_status_key(TargetKey::Terminal(id));
            core.clear_manual_status_caller(sender);
            core.pty_saw(sender, id);
            core.flush_saved().await?;

            Ok(ReviewResult {
                target: target_addr,
                target_kind: "terminal".into(),
                comment_count: pkg.comment_count,
                file_count: pkg.file_count,
                staged_path: Some(path_str),
                status: sent.status,
                turn: sent.turn,
                inbox_len: 0,
            })
        }
    }
}
