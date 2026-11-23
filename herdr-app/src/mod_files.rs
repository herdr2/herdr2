//! Asynchronous text files for mods. Writes reject a stale editor snapshot.
use gpui_shell::{HostError, HostValue};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;

pub const LIMIT: u64 = 1024 * 1024;
pub const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;
pub const MAX_ATTACHMENT_BATCH: usize = 8;
const ATTACHMENT_DIR_REL: &str = "data/acp-attachments";
const ATTACHMENT_TTL_SECS: u64 = 24 * 60 * 60;
static WRITES: Mutex<()> = Mutex::new(());
static NEXT_ATTACH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn mime_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/x-icon" => "ico",
        _ => "png",
    }
}

fn detect_resource_mime(path_hint: &str, bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    let ext = Path::new(path_hint)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("md" | "markdown") => "text/markdown",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("csv") => "text/csv",
        Some("xml") => "application/xml",
        Some("yaml" | "yml") => "text/yaml",
        Some("toml") => "application/toml",
        Some("pdf") => "application/pdf",
        Some("txt" | "log" | "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "h" | "cpp" | "java" | "sh") => {
            "text/plain"
        }
        _ => {
            if !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() {
                "text/plain"
            } else {
                "application/octet-stream"
            }
        }
    }
}

/// Removes temporary composer attachment previews older than 24 hours.
pub fn prune_stale_attachments(app_dir: &Path) {
    let dir = app_dir.join(ATTACHMENT_DIR_REL);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if let Ok(modified) = meta.modified()
            && let Ok(age) = now.duration_since(modified)
            && age.as_secs() > ATTACHMENT_TTL_SECS
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn write_preview_file(app_dir: &Path, prefix: &str, ext: &str, bytes: &[u8]) -> Result<(std::path::PathBuf, String), String> {
    let dir = app_dir.join(ATTACHMENT_DIR_REL);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let seq = NEXT_ATTACH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let filename = format!("{prefix}-{stamp}-{seq}.{ext}");
    let abs = dir.join(&filename);
    std::fs::write(&abs, bytes).map_err(|e| e.to_string())?;
    Ok((abs, format!("{ATTACHMENT_DIR_REL}/{filename}")))
}

/// Stages raw clipboard image bytes into `<app_dir>/data/acp-attachments/` for preview and `h2 send --image`.
pub fn stage_clipboard_image(app_dir: &Path, raw_bytes: &[u8]) -> Result<serde_json::Value, String> {
    if raw_bytes.is_empty() {
        return Err("Clipboard image is empty.".into());
    }
    let (bytes, mime) = match detect_image_mime("", raw_bytes) {
        Some(m @ ("image/png" | "image/jpeg" | "image/gif" | "image/webp")) => (raw_bytes.to_vec(), m),
        _ => {
            let img = image::load_from_memory(raw_bytes)
                .map_err(|_| "Clipboard does not hold a readable image.".to_owned())?;
            let mut png = std::io::Cursor::new(Vec::new());
            img.write_to(&mut png, image::ImageFormat::Png)
                .map_err(|_| "Could not encode clipboard image as PNG.".to_owned())?;
            (png.into_inner(), "image/png")
        }
    };
    if bytes.len() as u64 > IMAGE_LIMIT {
        return Err("Pasted image exceeds the 8 MiB limit.".into());
    }
    prune_stale_attachments(app_dir);
    let ext = mime_ext(mime);
    let (abs, preview) = write_preview_file(app_dir, "clip", ext, &bytes)?;
    let name = abs
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("clipboard.png")
        .to_string();
    Ok(serde_json::json!({
        "kind": "image",
        "path": abs.display().to_string(),
        "preview": preview,
        "name": name,
        "mimeType": mime,
        "bytes": bytes.len(),
    }))
}

/// Inspects local paths (from drag-and-drop, file picker, or copied files) into composer attachment descriptors.
/// Images get a cached preview under `<app_dir>/data/acp-attachments/`; other files attach as ACP resources.
pub fn prepare_attachments(app_dir: &Path, paths: &[std::path::PathBuf]) -> Vec<serde_json::Value> {
    prune_stale_attachments(app_dir);
    paths
        .iter()
        .take(MAX_ATTACHMENT_BATCH)
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("attachment")
                .to_string();
            let meta = match std::fs::metadata(p) {
                Ok(m) => m,
                Err(_) => return serde_json::json!({ "error": format!("Cannot read `{name}`.") }),
            };
            if !meta.is_file() {
                return serde_json::json!({ "error": format!("`{name}` is not a regular file.") });
            }
            if meta.len() > IMAGE_LIMIT {
                return serde_json::json!({ "error": format!("`{name}` exceeds the 8 MiB limit.") });
            }
            let bytes = match std::fs::read(p) {
                Ok(b) => b,
                Err(_) => return serde_json::json!({ "error": format!("Cannot read `{name}`.") }),
            };
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            if let Some(mime) = detect_image_mime(&name, &bytes) {
                let (final_bytes, final_mime, send_path) = match mime {
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/svg+xml" => {
                        (bytes, mime, abs.display().to_string())
                    }
                    _ => match image::load_from_memory(&bytes) {
                        Ok(img) => {
                            let mut png = std::io::Cursor::new(Vec::new());
                            if img.write_to(&mut png, image::ImageFormat::Png).is_ok() {
                                let out = png.into_inner();
                                if out.len() as u64 > IMAGE_LIMIT {
                                    return serde_json::json!({ "error": format!("`{name}` exceeds the 8 MiB limit.") });
                                }
                                match write_preview_file(app_dir, "att", "png", &out) {
                                    Ok((conv_abs, preview)) => {
                                        return serde_json::json!({
                                            "kind": "image",
                                            "path": conv_abs.display().to_string(),
                                            "preview": preview,
                                            "name": name,
                                            "mimeType": "image/png",
                                            "bytes": out.len(),
                                        });
                                    }
                                    Err(e) => return serde_json::json!({ "error": e }),
                                }
                            }
                            (bytes, mime, abs.display().to_string())
                        }
                        Err(_) => (bytes, mime, abs.display().to_string()),
                    },
                };
                let preview = write_preview_file(app_dir, "att", mime_ext(final_mime), &final_bytes)
                    .ok()
                    .map(|(_, rel)| rel);
                serde_json::json!({
                    "kind": "image",
                    "path": send_path,
                    "preview": preview,
                    "name": name,
                    "mimeType": final_mime,
                    "bytes": final_bytes.len(),
                })
            } else {
                let mime = detect_resource_mime(&name, &bytes);
                serde_json::json!({
                    "kind": "resource",
                    "path": abs.display().to_string(),
                    "name": name,
                    "mimeType": mime,
                    "bytes": bytes.len(),
                })
            }
        })
        .collect()
}

pub fn read_image_data_url(path: &Path) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !file.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(IMAGE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    encode_image_data_url(&path.to_string_lossy(), &bytes).map(Some)
}

pub fn encode_image_data_url(path_hint: &str, bytes: &[u8]) -> Result<String, String> {
    use base64::Engine as _;
    if bytes.len() as u64 > IMAGE_LIMIT {
        return Err("Image file exceeds 8 MiB".into());
    }
    let mime = detect_image_mime(path_hint, bytes)
        .ok_or_else(|| "Unsupported or unreadable image format".to_owned())?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

fn detect_image_mime(path_hint: &str, bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if bytes.starts_with(b"II*\x00") || bytes.starts_with(b"MM\x00*") {
        return Some("image/tiff");
    }
    if bytes.starts_with(b"\x00\x00\x01\x00") {
        return Some("image/x-icon");
    }
    if let Ok(text) = std::str::from_utf8(&bytes[..bytes.len().min(2048)]) {
        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("<svg")
            || ((lower.starts_with("<?xml") || lower.starts_with("<!doctype svg"))
                && lower.contains("<svg"))
        {
            return Some("image/svg+xml");
        }
    }
    let ext = Path::new(path_hint)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("svg") => Some("image/svg+xml"),
        Some("bmp") => Some("image/bmp"),
        Some("ico") => Some("image/x-icon"),
        Some("tif" | "tiff") => Some("image/tiff"),
        _ => None,
    }
}

pub fn read(path: &Path) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut text = String::new();
    file.take(LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > LIMIT {
        return Err("Text file exceeds 1 MiB".into());
    }
    Ok(Some(text))
}

pub fn write(path: &Path, text: &str, expected: Option<&str>) -> Result<(), String> {
    let _guard = WRITES.lock().map_err(|e| e.to_string())?;
    if text.len() as u64 > LIMIT {
        return Err("Text file exceeds 1 MiB".into());
    }
    if read(path)?.as_deref() != expected {
        return Err(
            "The file changed outside Herdr. Reload before saving; your draft is still here."
                .into(),
        );
    }
    let parent = path.parent().ok_or("File needs a parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    if expected.is_none() {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        return f.write_all(text.as_bytes()).map_err(|e| e.to_string());
    }
    // Resolve symlinks so replacing the file does not replace the link itself.
    let target = crate::persist::resolve_target(path).map_err(|e| e.to_string())?;
    let temp = target.with_file_name(format!(
        ".herdr-app-write-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        f.set_permissions(
            std::fs::metadata(&target)
                .map_err(|e| e.to_string())?
                .permissions(),
        )
        .map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        if read(&target)?.as_deref() != expected {
            return Err("The file changed while saving. Reload before saving.".into());
        }
        std::fs::rename(&temp, &target).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(temp);
    result
}

/// Reads off the UI thread: the text, or null when absent.
pub fn read_with(
    read: impl FnOnce() -> Result<Option<String>, String> + Send + 'static,
) -> impl std::future::Future<Output = Result<HostValue, HostError>> + Send {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(read());
    });
    async move {
        let value = rx
            .recv()
            .await
            .map_err(|e| HostError::new(e.to_string()))?
            .map_err(HostError::new)?;
        Ok(value.map(HostValue::from).unwrap_or(HostValue::Null))
    }
}

/// Writes off the UI thread.
pub fn write_with(
    write: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> impl std::future::Future<Output = Result<HostValue, HostError>> + Send {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(write());
    });
    async move {
        rx.recv()
            .await
            .map_err(|e| HostError::new(e.to_string()))?
            .map_err(HostError::new)?;
        Ok(HostValue::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creates_updates_and_rejects_stale_text() {
        let root = crate::test_tmp().join(format!(
            "herdr-app-files-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join(".brain/todo.md");
        assert_eq!(read(&path).unwrap(), None);
        write(&path, "- [ ] Task\n", None).unwrap();
        assert!(write(&path, "overwrite", None).is_err());
        write(&path, "- [x] Task\n", Some("- [ ] Task\n")).unwrap();
        assert!(write(&path, "stale", Some("- [ ] Task\n")).is_err());
        assert_eq!(read(&path).unwrap().unwrap(), "- [x] Task\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_and_encodes_images_as_data_urls() {
        let root = crate::test_tmp().join(format!(
            "herdr-app-images-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let png_path = root.join("dot.png");
        let svg_path = root.join("box.svg");
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
        ];
        std::fs::write(&png_path, png).unwrap();
        std::fs::write(&svg_path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();

        let png_url = read_image_data_url(&png_path).unwrap().unwrap();
        assert!(png_url.starts_with("data:image/png;base64,"));
        let svg_url = read_image_data_url(&svg_path).unwrap().unwrap();
        assert!(svg_url.starts_with("data:image/svg+xml;base64,"));
        assert_eq!(read_image_data_url(&root.join("missing.png")).unwrap(), None);
        assert!(encode_image_data_url("plain.bin", b"not-an-image").is_err());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stages_clipboard_images_and_prepares_mixed_attachments() {
        let root = crate::test_tmp().join(format!(
            "herdr-app-att-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let app_dir = root.join("app");
        let files_dir = root.join("files");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&files_dir).unwrap();

        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
        ];
        let pasted = stage_clipboard_image(&app_dir, &png).unwrap();
        assert_eq!(pasted["kind"], "image");
        assert_eq!(pasted["mimeType"], "image/png");
        assert!(pasted["preview"].as_str().unwrap().starts_with("data/acp-attachments/clip-"));
        assert!(Path::new(pasted["path"].as_str().unwrap()).is_file());

        let img_file = files_dir.join("shot.png");
        let md_file = files_dir.join("spec.md");
        std::fs::write(&img_file, png).unwrap();
        std::fs::write(&md_file, "# Spec\n").unwrap();

        let prepared = prepare_attachments(&app_dir, &[img_file, md_file, files_dir.join("missing.png")]);
        assert_eq!(prepared.len(), 3);
        assert_eq!(prepared[0]["kind"], "image");
        assert_eq!(prepared[0]["name"], "shot.png");
        assert!(prepared[0]["preview"].as_str().unwrap().starts_with("data/acp-attachments/att-"));
        assert_eq!(prepared[1]["kind"], "resource");
        assert_eq!(prepared[1]["mimeType"], "text/markdown");
        assert_eq!(prepared[1]["name"], "spec.md");
        assert!(prepared[2]["error"].as_str().is_some());

        std::fs::remove_dir_all(root).unwrap();
    }
}
