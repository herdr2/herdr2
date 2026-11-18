//! Asynchronous text files for mods. Writes reject a stale editor snapshot.
use gpui_shell::{HostError, HostValue};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;

pub const LIMIT: u64 = 1024 * 1024;
pub const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;
static WRITES: Mutex<()> = Mutex::new(());

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
}
