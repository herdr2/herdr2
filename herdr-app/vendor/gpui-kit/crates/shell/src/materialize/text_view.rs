//! Fetch document images with the creating script's policy. GPUI owns asynchronous
//! loading, caching and completion notifications; the document owns their lifetime.

use std::{
    collections::HashSet,
    hash::{Hash as _, Hasher as _},
    io::Cursor,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{
    App, Asset, Bounds, Element, ElementId, GlobalElementId, ImageCacheError, ImageSource,
    InspectorElementId, IntoElement, LayoutId, Pixels, RenderImage, SharedString, SharedUri,
    SvgRenderer, WeakEntity, Window, http_client::HttpClient,
};
use gpui_base::TextView;
use image::AnimationDecoder as _;
use smol::io::AsyncReadExt as _;

use crate::{Capabilities, capability::is_openable_url, policy::Policy};

const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;
const IMAGE_TIMEOUT: Duration = Duration::from_secs(30);

type ImageResult = Result<Arc<RenderImage>, ImageCacheError>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct TextViewImageOptions {
    pub(crate) base_dir: Option<String>,
    pub(crate) root_dir: Option<String>,
    pub(crate) allow_data_images: bool,
}

pub(super) fn with_policy(
    view: TextView,
    policy: Rc<Policy>,
    options: TextViewImageOptions,
) -> impl IntoElement {
    PolicyTextView {
        view,
        policy,
        options,
    }
}

/// CLI check constructs elements without drawing. Only layout may initialize
/// the keyed image owner; GPUI's Asset still owns all loading and notifications.
struct PolicyTextView {
    view: TextView,
    policy: Rc<Policy>,
    options: TextViewImageOptions,
}

impl IntoElement for PolicyTextView {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for PolicyTextView {
    type RequestLayoutState = <TextView as Element>::RequestLayoutState;
    type PrepaintState = <TextView as Element>::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        self.view.id()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.view.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.view = image_sources(
            self.view.clone(),
            self.policy.clone(),
            self.options.clone(),
            window,
            cx,
        );
        self.view.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.view
            .prepaint(id, inspector_id, bounds, state, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .paint(id, inspector_id, bounds, state, prepaint, window, cx);
    }
}

fn image_sources(
    view: TextView,
    policy: Rc<Policy>,
    options: TextViewImageOptions,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    options.hash(&mut hasher);
    let key = SharedString::from(format!(
        "{}/shell-images/{:p}/{:016x}",
        view.id().expect("TextView has an element id"),
        Rc::as_ptr(&policy),
        hasher.finish(),
    ));
    let images = window.use_keyed_state(key, cx, |_, cx| {
        let owner = cx.weak_entity();
        cx.on_release(move |images: &mut DocumentImages, cx| {
            for uri in &images.used {
                let source = (owner.clone(), uri.clone());
                if let Some(Ok(image)) = cx.fetch_asset::<DocumentImage>(&source) {
                    cx.drop_image(image, None);
                }
                cx.remove_asset::<DocumentImage>(&source);
            }
        })
        .detach();
        DocumentImages {
            policy,
            options,
            used: HashSet::new(),
        }
    });
    view.image_source(move |uri| {
        let images = images.clone();
        let uri = uri.clone();
        ImageSource::Custom(Arc::new(move |window, cx| {
            images.update(cx, |images, _| {
                images.used.insert(uri.clone());
            });
            window.use_asset::<DocumentImage>(&(images.downgrade(), uri.clone()), cx)
        }))
    })
}

struct DocumentImages {
    // Retaining the immutable policy also keeps its identity from being reused.
    policy: Rc<Policy>,
    options: TextViewImageOptions,
    used: HashSet<SharedUri>,
}

struct DocumentImage;

impl Asset for DocumentImage {
    // The owner scopes both successful and failed loads to this view and policy.
    // A weak reference lets releasing the view cancel pending loads.
    type Source = (WeakEntity<DocumentImages>, SharedUri);
    type Output = ImageResult;

    fn load(
        (owner, uri): Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = ImageResult> + Send + 'static {
        let state = owner.upgrade().map(|images| {
            let images = images.read(cx);
            (images.policy.capabilities().clone(), images.options.clone())
        });
        let client = cx.http_client();
        let renderer = cx.svg_renderer();
        let executor = cx.background_executor().clone();
        let timeout = executor.timer(IMAGE_TIMEOUT);
        async move {
            let (capabilities, options) = state.ok_or_else(denied_image)?;
            let raw = uri.as_ref();
            if raw
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
            {
                if !options.allow_data_images {
                    return Err(denied_image());
                }
                let bytes = decode_data_image_url(raw)?;
                return decode_image(bytes, renderer);
            }
            if is_openable_url(raw) {
                let url = image_url(&capabilities, raw)?;
                return smol::future::race(
                    async move {
                        let bytes = request_image(client, capabilities, url).await?;
                        decode_image(bytes, renderer)
                    },
                    async move {
                        timeout.await;
                        Err(ImageCacheError::Asset(
                            "TextView image request timed out".into(),
                        ))
                    },
                )
                .await;
            }
            if options.base_dir.is_some() || options.root_dir.is_some() {
                let candidates = local_image_candidates(raw, &options)?;
                let bytes = executor
                    .spawn(async move { read_local_image(&capabilities, &candidates) })
                    .await?;
                return decode_image(bytes, renderer);
            }
            Err(denied_image())
        }
    }
}

fn decode_data_image_url(raw: &str) -> Result<Vec<u8>, ImageCacheError> {
    let parsed = data_url::DataUrl::process(raw)
        .map_err(|_| ImageCacheError::Asset("Invalid TextView data image URL".into()))?;
    if !parsed.mime_type().type_.eq_ignore_ascii_case("image") {
        return Err(denied_image());
    }
    let (bytes, _) = parsed
        .decode_to_vec()
        .map_err(|_| ImageCacheError::Asset("Invalid TextView data image encoding".into()))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ImageCacheError::Asset(
            "TextView image exceeds the 8 MiB limit".into(),
        ));
    }
    Ok(bytes)
}

fn local_image_candidates(
    value: &str,
    options: &TextViewImageOptions,
) -> Result<Vec<PathBuf>, ImageCacheError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with("//") {
        return Err(denied_image());
    }
    if trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
    {
        let url = reqwest::Url::parse(trimmed).map_err(|_| denied_image())?;
        let path = url.to_file_path().map_err(|()| denied_image())?;
        return Ok(vec![path]);
    }
    if let Some(colon) = trimmed.find(':') {
        let before_colon = &trimmed[..colon];
        let ends_before_sep = !before_colon.contains(['/', '\\', '?', '#']);
        if ends_before_sep
            && before_colon.len() > 1
            && before_colon.bytes().enumerate().all(|(i, b)| {
                if i == 0 {
                    b.is_ascii_alphabetic()
                } else {
                    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')
                }
            })
        {
            return Err(denied_image());
        }
    }
    let clean = trimmed.split(['?', '#']).next().unwrap_or("").trim();
    if clean.is_empty() {
        return Err(denied_image());
    }
    let decoded = percent_decode(clean).ok_or_else(denied_image)?;
    if decoded.contains('\0') {
        return Err(denied_image());
    }
    let normalized = if cfg!(unix) {
        decoded.replace('\\', "/")
    } else {
        decoded
    };
    let path = Path::new(&normalized);
    let mut candidates = Vec::new();
    if path.is_absolute() || normalized.starts_with('/') {
        if path.is_absolute() {
            candidates.push(path.to_path_buf());
        }
        if let Some(rel) = normalized.strip_prefix('/') {
            let rel = rel.trim_start_matches('/');
            if !rel.is_empty() {
                if let Some(root) = &options.root_dir {
                    let candidate = Path::new(root).join(rel);
                    if !candidates.contains(&candidate) {
                        candidates.push(candidate);
                    }
                }
                if let Some(base) = &options.base_dir {
                    let candidate = Path::new(base).join(rel);
                    if !candidates.contains(&candidate) {
                        candidates.push(candidate);
                    }
                }
            }
        }
    } else {
        if let Some(base) = &options.base_dir {
            candidates.push(Path::new(base).join(path));
        }
        if let Some(root) = &options.root_dir {
            let candidate = Path::new(root).join(path);
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    if candidates.is_empty() {
        return Err(denied_image());
    }
    Ok(candidates)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn read_local_image(
    capabilities: &Capabilities,
    candidates: &[PathBuf],
) -> Result<Vec<u8>, ImageCacheError> {
    if matches!(capabilities.execute_grant(), crate::ExecuteGrant::Unrestricted) {
        for candidate in candidates {
            match std::fs::File::open(candidate) {
                Ok(file) => {
                    if !file.metadata().map(|m| m.is_file()).unwrap_or(false) {
                        continue;
                    }
                    return read_bounded(file);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
        }
        return Err(ImageCacheError::Asset(
            "TextView local image not found".into(),
        ));
    }
    if capabilities.has_read_access() {
        let mut saw_allowed = false;
        for candidate in candidates {
            match capabilities.open(candidate, crate::capability::Access::Read) {
                Ok(grant) => {
                    saw_allowed = true;
                    let (dir, relative) = grant.into_parts();
                    match dir.open(&relative) {
                        Ok(file) => {
                            if !file.metadata().map(|m| m.is_file()).unwrap_or(false) {
                                continue;
                            }
                            return read_bounded(file);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(_) => continue,
            }
        }
        return if saw_allowed {
            Err(ImageCacheError::Asset(
                "TextView local image not found".into(),
            ))
        } else {
            Err(denied_image())
        };
    }
    Err(denied_image())
}

fn read_bounded(reader: impl std::io::Read) -> Result<Vec<u8>, ImageCacheError> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    reader
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ImageCacheError::Asset(
            "TextView image exceeds the 8 MiB limit".into(),
        ));
    }
    Ok(bytes)
}

fn denied_image() -> ImageCacheError {
    ImageCacheError::Asset(
        "TextView images require an absolute HTTP(S) URL and a capabilities.network GET grant"
            .into(),
    )
}

fn image_url(capabilities: &Capabilities, value: &str) -> Result<reqwest::Url, ImageCacheError> {
    if !is_openable_url(value) {
        return Err(denied_image());
    }
    let url = reqwest::Url::parse(value).map_err(|_| denied_image())?;
    // Do not turn document-supplied userinfo into implicit HTTP credentials.
    if !url.username().is_empty() || url.password().is_some() {
        return Err(denied_image());
    }
    if !capabilities.may_request(
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port(),
        "GET",
        url.path(),
    ) {
        return Err(denied_image());
    }
    Ok(url)
}

async fn request_image(
    client: Arc<dyn HttpClient>,
    capabilities: Capabilities,
    mut url: reqwest::Url,
) -> Result<Vec<u8>, ImageCacheError> {
    for redirects in 0..=MAX_REDIRECTS {
        url = image_url(&capabilities, url.as_str())?;
        url.set_fragment(None);
        // The host transport must never follow a redirect on our behalf.
        let mut response = client.get(url.as_str(), ().into(), false).await?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            if redirects == MAX_REDIRECTS {
                return Err(ImageCacheError::Asset(
                    "Too many TextView image redirects".into(),
                ));
            }
            let next = response
                .headers()
                .get("location")
                .and_then(|location| location.to_str().ok())
                .and_then(|location| url.join(location).ok())
                .ok_or_else(|| ImageCacheError::Asset("Invalid TextView image redirect".into()))?;
            if url.scheme() == "https" && next.scheme() == "http" {
                return Err(ImageCacheError::Asset(
                    "TextView image HTTPS downgrade refused".into(),
                ));
            }
            // The next iteration re-authorizes scheme, host, port, GET and path.
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(ImageCacheError::Asset(
                format!("TextView image request returned {}", response.status()).into(),
            ));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(ImageCacheError::Asset(
                "TextView image exceeds the 8 MiB limit".into(),
            ));
        }
        return Ok(bytes);
    }
    unreachable!("the final redirect is rejected above")
}

/// Match GPUI's resource decoding without handing it the document URL again.
/// Otherwise decoding would silently start a second, ungated HTTP request.
fn decode_image(bytes: Vec<u8>, renderer: SvgRenderer) -> ImageResult {
    let Ok(format) = image::guess_format(&bytes) else {
        refuse_svg_file_references(&bytes)?;
        return renderer
            .render_single_frame(&bytes, 1.0)
            .map_err(Into::into);
    };
    let frames = match format {
        image::ImageFormat::Gif => animation_frames(
            image::codecs::gif::GifDecoder::new(Cursor::new(&bytes))?.into_frames(),
        )?,
        image::ImageFormat::WebP => {
            let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&bytes))?;
            if decoder.has_animation() {
                let _ = decoder.set_background_color(image::Rgba([0, 0, 0, 0]));
                animation_frames(decoder.into_frames())?
            } else {
                static_frame(decoder)?
            }
        }
        _ => static_frame(
            image::ImageReader::with_format(Cursor::new(&bytes), format).into_decoder()?,
        )?,
    };
    Ok(Arc::new(RenderImage::new(frames)))
}

/// GPUI renders an SVG `<image>` whose href is not a data URL from the local
/// file it names, which would draw files the script has no grant to read.
/// Parse the SVG with a resolver that records such an href, and refuse the
/// image when it has one.
fn refuse_svg_file_references(bytes: &[u8]) -> Result<(), ImageCacheError> {
    let references_file = Arc::new(AtomicBool::new(false));
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new({
                let references_file = references_file.clone();
                move |_, _| {
                    references_file.store(true, Ordering::Relaxed);
                    None
                }
            }),
        },
        ..Default::default()
    };
    usvg::Tree::from_data(bytes, &options)
        .map_err(|error| ImageCacheError::Usvg(Arc::new(error)))?;
    if references_file.load(Ordering::Relaxed) {
        return Err(ImageCacheError::Asset(
            "TextView SVG images cannot reference files".into(),
        ));
    }
    Ok(())
}

fn static_frame(
    mut decoder: impl image::ImageDecoder,
) -> Result<smallvec::SmallVec<[image::Frame; 1]>, ImageCacheError> {
    let orientation = decoder.orientation()?;
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let mut data = image.into_rgba8();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(smallvec::smallvec![image::Frame::new(data)])
}

fn animation_frames(
    frames: image::Frames<'_>,
) -> Result<smallvec::SmallVec<[image::Frame; 1]>, ImageCacheError> {
    let mut decoded = smallvec::SmallVec::new();
    for frame in frames {
        match frame {
            Ok(mut frame) => {
                for pixel in frame.buffer_mut().chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
                decoded.push(frame);
            }
            Err(error) => tracing::debug!(%error, "Skipping an invalid TextView image frame"),
        }
    }
    if decoded.is_empty() {
        return Err(ImageCacheError::Asset(
            "TextView image has no decodable frames".into(),
        ));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests;
