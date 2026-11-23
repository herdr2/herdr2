//! Multimodal ACP content staging, capability-aware wire encoding, and CLI attachment helpers.

use crate::acp::proto::{
    ContentBlock, EmbeddedResourceContent, PromptCapabilities, detect_mime_type, ext_for_mime, file_uri_from_path,
    name_from_uri, path_from_file_uri,
};
use crate::put;
use crate::rpc::RpcError;
use crate::transcript::Attachment;
use std::path::Path;

pub const MAX_BLOCKS_PER_PROMPT: usize = 16;
const MAX_TEXT_RESOURCE_BYTES: usize = 512 * 1024;
const MAX_PREVIEW_CHARS: usize = 4000;

fn cap_preview(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars();
    let head: String = chars.by_ref().take(MAX_PREVIEW_CHARS).collect();
    if chars.next().is_some() { Some(format!("{head}\n…")) } else { Some(head) }
}

fn default_name(prefix: &str, uri: Option<&str>, mime: Option<&str>) -> String {
    if let Some(name) = uri.and_then(name_from_uri) {
        return name;
    }
    let ext = mime.map(ext_for_mime).unwrap_or("bin");
    format!("{prefix}.{ext}")
}

fn decode_checked(raw: &str, label: &str) -> Result<Vec<u8>, RpcError> {
    let clean: String = raw.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if clean.is_empty() {
        return Err(RpcError::invalid_params(format!("{label} data cannot be empty")));
    }
    if clean.len() / 4 * 3 > put::MAX_BYTES + 3 {
        return Err(RpcError::invalid_params(format!("{label} exceeds {} MB limit", put::MAX_BYTES >> 20)));
    }
    let bytes =
        put::decode(&clean).ok_or_else(|| RpcError::invalid_params(format!("{label} data is not valid base64")))?;
    if bytes.len() > put::MAX_BYTES {
        return Err(RpcError::invalid_params(format!("{label} exceeds {} MB limit", put::MAX_BYTES >> 20)));
    }
    Ok(bytes)
}

/// Validates and stages incoming `SendParams.content` blocks into `<state>/files/<addr>/`.
/// Returns `(extra_texts, staged_attachments)`.
pub fn stage_prompt_content(
    state_dir: &Path,
    agent_id: u32,
    blocks: &[ContentBlock],
) -> Result<(Vec<String>, Vec<Attachment>), RpcError> {
    if blocks.len() > MAX_BLOCKS_PER_PROMPT {
        return Err(RpcError::invalid_params(format!("at most {MAX_BLOCKS_PER_PROMPT} content blocks per prompt")));
    }
    let mut extra_texts = Vec::new();
    let mut attachments = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    extra_texts.push(text.clone());
                }
            }
            ContentBlock::Image { data, mime_type, uri } => {
                let bytes = decode_checked(data, "image")?;
                let hint = uri.as_deref().unwrap_or("");
                let mime = detect_mime_type(hint, &bytes)
                    .map(String::from)
                    .or_else(|| (!mime_type.trim().is_empty()).then(|| mime_type.trim().to_string()))
                    .unwrap_or_else(|| "image/png".into());
                let name = default_name("image", uri.as_deref(), Some(&mime));
                let staged = put::stage(state_dir, agent_id, &name, &bytes)
                    .map_err(|e| RpcError::internal(format!("cannot stage image attachment: {e}")))?;
                let staged_str = staged.display().to_string();
                let staged_name = staged.file_name().and_then(|s| s.to_str()).unwrap_or(&name).to_string();
                attachments.push(Attachment {
                    kind: "image".into(),
                    mime_type: Some(mime),
                    name: Some(staged_name),
                    path: Some(staged_str.clone()),
                    uri: Some(file_uri_from_path(&staged_str)),
                    text: None,
                    bytes: Some(bytes.len() as u64),
                });
            }
            ContentBlock::Audio { data, mime_type } => {
                let bytes = decode_checked(data, "audio")?;
                let mime = detect_mime_type("", &bytes)
                    .map(String::from)
                    .or_else(|| (!mime_type.trim().is_empty()).then(|| mime_type.trim().to_string()))
                    .unwrap_or_else(|| "audio/wav".into());
                let name = default_name("audio", None, Some(&mime));
                let staged = put::stage(state_dir, agent_id, &name, &bytes)
                    .map_err(|e| RpcError::internal(format!("cannot stage audio attachment: {e}")))?;
                let staged_str = staged.display().to_string();
                let staged_name = staged.file_name().and_then(|s| s.to_str()).unwrap_or(&name).to_string();
                attachments.push(Attachment {
                    kind: "audio".into(),
                    mime_type: Some(mime),
                    name: Some(staged_name),
                    path: Some(staged_str.clone()),
                    uri: Some(file_uri_from_path(&staged_str)),
                    text: None,
                    bytes: Some(bytes.len() as u64),
                });
            }
            ContentBlock::Resource { resource } => match resource {
                EmbeddedResourceContent::Text { uri, mime_type, text } => {
                    let bytes = text.as_bytes();
                    if bytes.len() > put::MAX_BYTES {
                        return Err(RpcError::invalid_params(format!(
                            "resource exceeds {} MB limit",
                            put::MAX_BYTES >> 20
                        )));
                    }
                    let mime = mime_type
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from)
                        .or_else(|| detect_mime_type(uri, bytes).map(String::from))
                        .unwrap_or_else(|| "text/plain".into());
                    let name = default_name("resource", Some(uri), Some(&mime));
                    let staged = put::stage(state_dir, agent_id, &name, bytes)
                        .map_err(|e| RpcError::internal(format!("cannot stage text resource: {e}")))?;
                    let staged_str = staged.display().to_string();
                    let display_name = name_from_uri(uri).unwrap_or(name);
                    attachments.push(Attachment {
                        kind: "resource".into(),
                        mime_type: Some(mime),
                        name: Some(display_name),
                        path: Some(staged_str),
                        uri: Some(uri.clone()),
                        text: cap_preview(text),
                        bytes: Some(bytes.len() as u64),
                    });
                }
                EmbeddedResourceContent::Blob { uri, mime_type, blob } => {
                    let bytes = decode_checked(blob, "resource blob")?;
                    let mime = mime_type
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from)
                        .or_else(|| detect_mime_type(uri, &bytes).map(String::from))
                        .unwrap_or_else(|| "application/octet-stream".into());
                    let name = default_name("resource", Some(uri), Some(&mime));
                    let staged = put::stage(state_dir, agent_id, &name, &bytes)
                        .map_err(|e| RpcError::internal(format!("cannot stage binary resource: {e}")))?;
                    let staged_str = staged.display().to_string();
                    let display_name = name_from_uri(uri).unwrap_or(name);
                    attachments.push(Attachment {
                        kind: "resource".into(),
                        mime_type: Some(mime),
                        name: Some(display_name),
                        path: Some(staged_str),
                        uri: Some(uri.clone()),
                        text: None,
                        bytes: Some(bytes.len() as u64),
                    });
                }
            },
            ContentBlock::ResourceLink { uri, name, title, description, mime_type, size } => {
                let display_name = title
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| (!name.trim().is_empty()).then_some(name.as_str()))
                    .map(String::from)
                    .or_else(|| name_from_uri(uri));
                attachments.push(Attachment {
                    kind: "resource_link".into(),
                    mime_type: mime_type.clone(),
                    name: display_name,
                    path: path_from_file_uri(uri),
                    uri: Some(uri.clone()),
                    text: description.as_deref().and_then(cap_preview),
                    bytes: *size,
                });
            }
        }
    }
    Ok((extra_texts, attachments))
}

/// Stages multimodal output blocks emitted by an ACP agent (`agent_message_chunk` or `tool_call`).
/// Never fails: malformed binary payloads are downgraded to metadata-only attachments.
pub fn stage_output_blocks(
    state_dir: &Path,
    agent_id: u32,
    prefix: &str,
    blocks: &[ContentBlock],
) -> (String, Vec<Attachment>) {
    let mut text_out = String::new();
    let mut attachments = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => text_out.push_str(text),
            ContentBlock::Image { data, mime_type, uri } => {
                let clean: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                let decoded = put::decode(&clean).filter(|b| !b.is_empty() && b.len() <= put::MAX_BYTES);
                let mime = decoded
                    .as_deref()
                    .and_then(|b| detect_mime_type(uri.as_deref().unwrap_or(""), b))
                    .map(String::from)
                    .or_else(|| (!mime_type.trim().is_empty()).then(|| mime_type.trim().to_string()))
                    .unwrap_or_else(|| "image/png".into());
                let name = default_name(prefix, uri.as_deref(), Some(&mime));
                let (staged_path, bytes_len, final_name) = match decoded {
                    Some(bytes) => match put::stage(state_dir, agent_id, &name, &bytes) {
                        Ok(p) => {
                            let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or(&name).to_string();
                            (Some(p.display().to_string()), Some(bytes.len() as u64), fname)
                        }
                        Err(_) => (None, Some(bytes.len() as u64), name),
                    },
                    None => (uri.as_deref().and_then(path_from_file_uri), None, name),
                };
                attachments.push(Attachment {
                    kind: "image".into(),
                    mime_type: Some(mime),
                    name: Some(final_name),
                    path: staged_path,
                    uri: uri.clone(),
                    text: None,
                    bytes: bytes_len,
                });
            }
            ContentBlock::Audio { data, mime_type } => {
                let clean: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                let decoded = put::decode(&clean).filter(|b| !b.is_empty() && b.len() <= put::MAX_BYTES);
                let mime = decoded
                    .as_deref()
                    .and_then(|b| detect_mime_type("", b))
                    .map(String::from)
                    .or_else(|| (!mime_type.trim().is_empty()).then(|| mime_type.trim().to_string()))
                    .unwrap_or_else(|| "audio/wav".into());
                let name = default_name(prefix, None, Some(&mime));
                let (staged_path, bytes_len, final_name) = match decoded {
                    Some(bytes) => match put::stage(state_dir, agent_id, &name, &bytes) {
                        Ok(p) => {
                            let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or(&name).to_string();
                            (Some(p.display().to_string()), Some(bytes.len() as u64), fname)
                        }
                        Err(_) => (None, Some(bytes.len() as u64), name),
                    },
                    None => (None, None, name),
                };
                attachments.push(Attachment {
                    kind: "audio".into(),
                    mime_type: Some(mime),
                    name: Some(final_name),
                    path: staged_path,
                    uri: None,
                    text: None,
                    bytes: bytes_len,
                });
            }
            ContentBlock::Resource { resource } => match resource {
                EmbeddedResourceContent::Text { uri, mime_type, text } => {
                    let bytes = text.as_bytes();
                    let mime = mime_type
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from)
                        .or_else(|| detect_mime_type(uri, bytes).map(String::from))
                        .unwrap_or_else(|| "text/plain".into());
                    let name = default_name(prefix, Some(uri), Some(&mime));
                    let staged_path = (bytes.len() <= put::MAX_BYTES)
                        .then(|| put::stage(state_dir, agent_id, &name, bytes).ok())
                        .flatten()
                        .map(|p| p.display().to_string());
                    attachments.push(Attachment {
                        kind: "resource".into(),
                        mime_type: Some(mime),
                        name: name_from_uri(uri).or(Some(name)),
                        path: staged_path.or_else(|| path_from_file_uri(uri)),
                        uri: Some(uri.clone()),
                        text: cap_preview(text),
                        bytes: Some(bytes.len() as u64),
                    });
                }
                EmbeddedResourceContent::Blob { uri, mime_type, blob } => {
                    let clean: String = blob.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                    let decoded = put::decode(&clean).filter(|b| !b.is_empty() && b.len() <= put::MAX_BYTES);
                    let mime = mime_type
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from)
                        .or_else(|| decoded.as_deref().and_then(|b| detect_mime_type(uri, b)).map(String::from))
                        .unwrap_or_else(|| "application/octet-stream".into());
                    let name = default_name(prefix, Some(uri), Some(&mime));
                    let (staged_path, bytes_len) = match decoded {
                        Some(bytes) => (
                            put::stage(state_dir, agent_id, &name, &bytes).ok().map(|p| p.display().to_string()),
                            Some(bytes.len() as u64),
                        ),
                        None => (path_from_file_uri(uri), None),
                    };
                    attachments.push(Attachment {
                        kind: "resource".into(),
                        mime_type: Some(mime),
                        name: name_from_uri(uri).or(Some(name)),
                        path: staged_path,
                        uri: Some(uri.clone()),
                        text: None,
                        bytes: bytes_len,
                    });
                }
            },
            ContentBlock::ResourceLink { uri, name, title, description, mime_type, size } => {
                let display_name = title
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| (!name.trim().is_empty()).then_some(name.as_str()))
                    .map(String::from)
                    .or_else(|| name_from_uri(uri));
                attachments.push(Attachment {
                    kind: "resource_link".into(),
                    mime_type: mime_type.clone(),
                    name: display_name,
                    path: path_from_file_uri(uri),
                    uri: Some(uri.clone()),
                    text: description.as_deref().and_then(cap_preview),
                    bytes: *size,
                });
            }
        }
    }
    (text_out, attachments)
}

/// Materializes wire `ContentBlock`s for ACP `session/prompt`, respecting advertised `PromptCapabilities`.
/// Returns `(blocks, used_rich_blocks)` where `used_rich_blocks` indicates whether native `image`/`audio`/`resource`
/// blocks were included (so callers can gracefully retry with `force_downgrade = true` on `-32602`).
pub fn materialize_prompt_blocks(
    wire_text: &str,
    attachments: &[Attachment],
    caps: PromptCapabilities,
    force_downgrade: bool,
) -> (Vec<ContentBlock>, bool) {
    let mut blocks = Vec::new();
    let mut fallback_notes = Vec::new();
    let mut used_rich = false;

    for att in attachments {
        let name = att
            .name
            .clone()
            .or_else(|| att.uri.as_deref().and_then(name_from_uri))
            .or_else(|| att.path.as_deref().and_then(name_from_uri))
            .unwrap_or_else(|| att.kind.clone());
        let link_uri =
            att.uri.clone().or_else(|| att.path.as_deref().map(file_uri_from_path)).unwrap_or_else(|| name.clone());

        match att.kind.as_str() {
            "image" if !force_downgrade && caps.image => {
                if let Some(path) = att.path.as_deref()
                    && let Ok(bytes) = std::fs::read(path)
                {
                    let mime = att
                        .mime_type
                        .clone()
                        .or_else(|| detect_mime_type(path, &bytes).map(String::from))
                        .unwrap_or_else(|| "image/png".into());
                    blocks.push(ContentBlock::Image {
                        data: put::encode(&bytes),
                        mime_type: mime,
                        uri: Some(link_uri),
                    });
                    used_rich = true;
                    continue;
                }
                blocks.push(ContentBlock::ResourceLink {
                    uri: link_uri,
                    name,
                    title: None,
                    description: att.text.clone(),
                    mime_type: att.mime_type.clone(),
                    size: att.bytes,
                });
            }
            "audio" if !force_downgrade && caps.audio => {
                if let Some(path) = att.path.as_deref()
                    && let Ok(bytes) = std::fs::read(path)
                {
                    let mime = att
                        .mime_type
                        .clone()
                        .or_else(|| detect_mime_type(path, &bytes).map(String::from))
                        .unwrap_or_else(|| "audio/wav".into());
                    blocks.push(ContentBlock::Audio { data: put::encode(&bytes), mime_type: mime });
                    used_rich = true;
                    continue;
                }
                blocks.push(ContentBlock::ResourceLink {
                    uri: link_uri,
                    name,
                    title: None,
                    description: att.text.clone(),
                    mime_type: att.mime_type.clone(),
                    size: att.bytes,
                });
            }
            "resource" if !force_downgrade && caps.embedded_context => {
                if let Some(path) = att.path.as_deref()
                    && let Ok(bytes) = std::fs::read(path)
                {
                    let is_text = bytes.len() <= MAX_TEXT_RESOURCE_BYTES
                        && !bytes.contains(&0)
                        && std::str::from_utf8(&bytes).is_ok();
                    let resource = if is_text {
                        EmbeddedResourceContent::Text {
                            uri: link_uri,
                            mime_type: att.mime_type.clone(),
                            text: String::from_utf8(bytes).expect("checked utf-8"),
                        }
                    } else {
                        EmbeddedResourceContent::Blob {
                            uri: link_uri,
                            mime_type: att.mime_type.clone(),
                            blob: put::encode(&bytes),
                        }
                    };
                    blocks.push(ContentBlock::Resource { resource });
                    used_rich = true;
                    continue;
                }
                if let Some(text) = att.text.clone() {
                    blocks.push(ContentBlock::Resource {
                        resource: EmbeddedResourceContent::Text {
                            uri: link_uri,
                            mime_type: att.mime_type.clone(),
                            text,
                        },
                    });
                    used_rich = true;
                    continue;
                }
                blocks.push(ContentBlock::ResourceLink {
                    uri: link_uri,
                    name,
                    title: None,
                    description: None,
                    mime_type: att.mime_type.clone(),
                    size: att.bytes,
                });
            }
            "resource_link" => {
                blocks.push(ContentBlock::ResourceLink {
                    uri: link_uri,
                    name,
                    title: None,
                    description: att.text.clone(),
                    mime_type: att.mime_type.clone(),
                    size: att.bytes,
                });
            }
            _ => {
                // Capability not supported (or downgraded retry): emit mandatory ACP `resource_link`
                // plus a concise text hint pointing to the staged local path.
                blocks.push(ContentBlock::ResourceLink {
                    uri: link_uri,
                    name: name.clone(),
                    title: None,
                    description: att.text.clone(),
                    mime_type: att.mime_type.clone(),
                    size: att.bytes,
                });
                if let Some(path) = att.path.as_deref() {
                    fallback_notes.push(format!("[Attached {}: {path}]", att.kind));
                }
            }
        }
    }

    let mut combined_text = wire_text.to_string();
    if !fallback_notes.is_empty() {
        if !combined_text.is_empty() {
            combined_text.push_str("\n\n");
        }
        combined_text.push_str(&fallback_notes.join("\n"));
    }

    let mut out = Vec::with_capacity(blocks.len() + usize::from(!combined_text.is_empty()));
    if !combined_text.is_empty() {
        out.push(ContentBlock::Text { text: combined_text });
    }
    out.extend(blocks);
    (out, used_rich)
}

/// Converts CLI `--image`, `--resource`, and `--resource-link` flags into ACP `ContentBlock`s.
pub fn cli_files_to_content_blocks(
    images: &[String],
    resources: &[String],
    resource_links: &[String],
) -> Result<Vec<ContentBlock>, RpcError> {
    let total = images.len() + resources.len() + resource_links.len();
    if total > MAX_BLOCKS_PER_PROMPT {
        return Err(RpcError::invalid_params(format!("at most {MAX_BLOCKS_PER_PROMPT} attachments per send")));
    }
    let mut blocks = Vec::with_capacity(total);
    for raw_path in images {
        let path = Path::new(raw_path);
        let bytes = std::fs::read(path)
            .map_err(|e| RpcError::new("usage", format!("cannot read image {raw_path}: {e}"), ""))?;
        if bytes.is_empty() {
            return Err(RpcError::invalid_params(format!("image {raw_path} is empty")));
        }
        if bytes.len() > put::MAX_BYTES {
            return Err(RpcError::invalid_params(format!(
                "image {raw_path} exceeds {} MB limit",
                put::MAX_BYTES >> 20
            )));
        }
        let mime = detect_mime_type(raw_path, &bytes)
            .filter(|m| m.starts_with("image/"))
            .ok_or_else(|| RpcError::invalid_params(format!("{raw_path} is not a recognized image format")))?;
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        blocks.push(ContentBlock::Image {
            data: put::encode(&bytes),
            mime_type: mime.to_string(),
            uri: Some(file_uri_from_path(&abs.display().to_string())),
        });
    }
    for raw_path in resources {
        let path = Path::new(raw_path);
        let bytes = std::fs::read(path)
            .map_err(|e| RpcError::new("usage", format!("cannot read resource {raw_path}: {e}"), ""))?;
        if bytes.len() > put::MAX_BYTES {
            return Err(RpcError::invalid_params(format!(
                "resource {raw_path} exceeds {} MB limit",
                put::MAX_BYTES >> 20
            )));
        }
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let uri = file_uri_from_path(&abs.display().to_string());
        let mime = detect_mime_type(raw_path, &bytes).map(String::from);
        let is_text =
            bytes.len() <= MAX_TEXT_RESOURCE_BYTES && !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok();
        let resource = if is_text {
            EmbeddedResourceContent::Text {
                uri,
                mime_type: mime.or_else(|| Some("text/plain".into())),
                text: String::from_utf8(bytes).expect("checked utf-8"),
            }
        } else {
            EmbeddedResourceContent::Blob {
                uri,
                mime_type: mime.or_else(|| Some("application/octet-stream".into())),
                blob: put::encode(&bytes),
            }
        };
        blocks.push(ContentBlock::Resource { resource });
    }
    for raw_uri in resource_links {
        let trimmed = raw_uri.trim();
        if trimmed.is_empty() {
            continue;
        }
        let uri = if trimmed.contains("://") {
            trimmed.to_string()
        } else {
            let p = Path::new(trimmed);
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            file_uri_from_path(&abs.display().to_string())
        };
        let name = name_from_uri(&uri).unwrap_or_else(|| trimmed.to_string());
        let mime = detect_mime_type(&name, &[]).map(String::from);
        blocks.push(ContentBlock::ResourceLink {
            uri,
            name,
            title: None,
            description: None,
            mime_type: mime,
            size: None,
        });
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

    #[test]
    fn stages_and_materializes_image_with_capability_negotiation() {
        let dir = crate::test_tmp().join(format!("h2-media-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();

        let input = vec![ContentBlock::Image {
            data: put::encode(TINY_PNG),
            mime_type: "image/png".into(),
            uri: Some("file:///tmp/bug.png".into()),
        }];
        let (extra, atts) = stage_prompt_content(&dir, 1, &input).unwrap();
        assert!(extra.is_empty());
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].kind, "image");
        assert_eq!(atts[0].mime_type.as_deref(), Some("image/png"));
        assert!(std::path::Path::new(atts[0].path.as_deref().unwrap()).is_file());

        // When adapter supports images: sends native ContentBlock::Image.
        let (native, used_rich) = materialize_prompt_blocks(
            "Inspect this",
            &atts,
            PromptCapabilities { image: true, audio: false, embedded_context: true },
            false,
        );
        assert!(used_rich);
        assert_eq!(native.len(), 2);
        assert!(matches!(&native[0], ContentBlock::Text { text } if text == "Inspect this"));
        assert!(matches!(&native[1], ContentBlock::Image { mime_type, .. } if mime_type == "image/png"));

        // When adapter lacks image capability: downgrades to ResourceLink + staged path hint.
        let (fallback, used_rich_fb) =
            materialize_prompt_blocks("Inspect this", &atts, PromptCapabilities::default(), false);
        assert!(!used_rich_fb);
        assert_eq!(fallback.len(), 2);
        assert!(matches!(&fallback[0], ContentBlock::Text { text } if text.contains("[Attached image:")));
        assert!(matches!(&fallback[1], ContentBlock::ResourceLink { mime_type: Some(m), .. } if m == "image/png"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stages_outbound_assistant_and_tool_multimodal_blocks() {
        let dir = crate::test_tmp().join(format!("h2-media-out-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();

        let blocks = vec![
            ContentBlock::Text { text: "Here is the chart:".into() },
            ContentBlock::Image { data: put::encode(TINY_PNG), mime_type: "image/png".into(), uri: None },
            ContentBlock::Resource {
                resource: EmbeddedResourceContent::Text {
                    uri: "file:///repo/report.md".into(),
                    mime_type: Some("text/markdown".into()),
                    text: "# Summary\nOK".into(),
                },
            },
        ];
        let (text, atts) = stage_output_blocks(&dir, 2, "out", &blocks);
        assert_eq!(text, "Here is the chart:");
        assert_eq!(atts.len(), 2);
        assert_eq!(atts[0].kind, "image");
        assert!(std::path::Path::new(atts[0].path.as_deref().unwrap()).is_file());
        assert_eq!(atts[1].kind, "resource");
        assert_eq!(atts[1].name.as_deref(), Some("report.md"));
        assert_eq!(atts[1].text.as_deref(), Some("# Summary\nOK"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
