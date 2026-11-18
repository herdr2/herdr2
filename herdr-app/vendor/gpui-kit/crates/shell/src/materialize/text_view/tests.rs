use super::*;
use crate::{HttpRequestGrant, ScriptView, ShellRuntime};
use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, div,
    http_client::{AsyncBody, FakeHttpClient, Method, RedirectPolicy, Response},
};
use std::{ops::Deref as _, sync::Mutex};

// A two-pixel PNG generated in memory, not an external fixture or URL.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0xf4, 0x22, 0x7f,
    0x8a, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x14, 0x32, 0x09, 0xfb,
    0xcf, 0xc0, 0xc0, 0xc0, 0x00, 0x00, 0x09, 0x0d, 0x01, 0x9d, 0xf7, 0x66, 0xcd, 0x15, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

type Requests = Arc<Mutex<Vec<String>>>;

fn get_grant(paths: &[&str]) -> Capabilities {
    Capabilities::new().http_requests([HttpRequestGrant::new(
        "images.example",
        ["GET"],
        paths.iter().copied(),
        [] as [&str; 0],
    )])
}

fn recording_client(
    reply: impl Fn(&str) -> Response<AsyncBody> + Send + Sync + 'static,
) -> (Arc<dyn HttpClient>, Requests) {
    let requests = Requests::default();
    let recorded = requests.clone();
    let client = FakeHttpClient::create(move |request| {
        assert_eq!(request.method(), Method::GET);
        assert_eq!(
            request.extensions().get::<RedirectPolicy>(),
            Some(&RedirectPolicy::NoFollow),
            "the transport must not follow redirects before authorization",
        );
        let url = request.uri().to_string();
        recorded.lock().unwrap().push(url.clone());
        let response = reply(&url);
        async move { Ok(response) }
    });
    (client, requests)
}

fn png_response() -> Response<AsyncBody> {
    Response::builder()
        .status(200)
        .header("content-type", "image/png")
        .body(PNG.to_vec().into())
        .unwrap()
}

fn fetch(
    client: Arc<dyn HttpClient>,
    capabilities: Capabilities,
) -> Result<Vec<u8>, ImageCacheError> {
    smol::block_on(request_image(
        client,
        capabilities,
        reqwest::Url::parse("https://images.example/image.png").unwrap(),
    ))
}

fn redirect(status: u16, target: &str) -> Response<AsyncBody> {
    Response::builder()
        .status(status)
        .header("location", target)
        .body(().into())
        .unwrap()
}

#[test]
fn document_image_urls_obey_get_grants() {
    let exact = get_grant(&["/image.png"]);
    for (url, allowed) in [
        ("https://images.example/image.png", true),
        (
            "https://images.example:443/image.png?size=small#preview",
            true,
        ),
        ("https://images.example/other.png", false),
        ("https://other.example/image.png", false),
        ("https://images.example:8443/image.png", false),
        ("http://images.example/image.png", false),
        ("data:image/png;base64,AAAA", false),
        ("file:///image.png", false),
        ("custom:image.png", false),
        ("//images.example/image.png", false),
        ("/image.png", false),
        ("image.png", false),
        ("https://", false),
        ("https://user:password@images.example/image.png", false),
    ] {
        assert_eq!(image_url(&exact, url).is_ok(), allowed, "{url}");
        assert!(image_url(&Capabilities::new(), url).is_err(), "{url}");
        if let Ok(parsed) = reqwest::Url::parse(url) {
            let (client, requests) = recording_client(|_| png_response());
            assert_eq!(
                smol::block_on(request_image(client, exact.clone(), parsed)).is_ok(),
                allowed,
                "{url}",
            );
            assert_eq!(
                requests.lock().unwrap().len(),
                usize::from(allowed),
                "{url}"
            );
        }
    }
    let post_only = Capabilities::new().http_requests([HttpRequestGrant::new(
        "images.example",
        ["POST"],
        ["/image.png"],
        [] as [&str; 0],
    )]);
    assert!(image_url(&post_only, "https://images.example/image.png").is_err());
    let (client, requests) = recording_client(|_| png_response());
    assert!(fetch(client, post_only).is_err());
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn document_image_redirects_reauthorize_each_hop() {
    for target in [
        "https://other.example/image.png",
        "https://images.example/not-granted.png",
        "https://images.example:8443/image.png",
        "file:///image.png",
        "data:image/png;base64,AAAA",
        "custom:image.png",
        "https://user:password@images.example/image.png",
    ] {
        let (client, requests) = recording_client(move |_| redirect(302, target));
        let result = fetch(client, get_grant(&["/image.png"]));
        assert!(result.is_err(), "{target}");
        assert_eq!(requests.lock().unwrap().len(), 1, "{target}");
    }
    // Even a legacy grant to both protocols must not allow HTTPS downgrade.
    let (client, requests) = recording_client(|_| redirect(302, "http://images.example/image.png"));
    let result = fetch(
        client,
        Capabilities::new().network_hosts(["images.example".to_owned()]),
    );
    assert!(result.is_err());
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[gpui::test]
fn document_image_redirects_preserve_authorized_get(cx: &mut TestAppContext) {
    for status in [301, 302, 303, 307, 308] {
        let (client, requests) = recording_client(move |url| {
            if url.ends_with("/image.png") {
                redirect(status, "/final.png")
            } else {
                png_response()
            }
        });
        let bytes = fetch(client, get_grant(&["/image.png", "/final.png"])).unwrap();
        assert_eq!(bytes, PNG);
        let image = cx
            .update(|cx| decode_image(bytes, cx.svg_renderer()))
            .unwrap();
        assert_eq!(image.size(0), gpui::size(2.into(), 1.into()));
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0x56, 0x34, 0x12, 0xff]);
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "https://images.example/image.png",
                "https://images.example/final.png",
            ],
        );
    }
}

#[test]
fn document_image_requests_bound_bodies_and_redirects() {
    for (status, body, allowed) in [
        (200, vec![0; MAX_IMAGE_BYTES as usize], true),
        (200, vec![0; MAX_IMAGE_BYTES as usize + 1], false),
        (404, Vec::new(), false),
        (302, Vec::new(), false), // Missing Location must not trigger a fallback.
    ] {
        let (client, requests) = recording_client(move |_| {
            Response::builder()
                .status(status)
                .body(body.clone().into())
                .unwrap()
        });
        let result = fetch(client, get_grant(&["/image.png"]));
        assert_eq!(result.is_ok(), allowed);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
    let (client, requests) = recording_client(|_| redirect(302, "/image.png"));
    assert!(fetch(client, get_grant(&["/image.png"])).is_err());
    assert_eq!(requests.lock().unwrap().len(), MAX_REDIRECTS + 1);
}

struct Documents(Vec<Entity<ScriptView>>);

impl Render for Documents {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.0.iter().cloned())
    }
}

fn source(format: &str, document: &str, scrollable: bool) -> String {
    let document = serde_json::to_string(document).unwrap();
    format!(
        r#"
import {{ View }} from "gpui-kit";
import {{ TextView }} from "gpui-base";
export default class Document extends View {{
  render() {{ return TextView.{format}("document", {document}).scrollable({scrollable}); }}
}}
"#,
    )
}

fn mount_documents(
    cx: &mut TestAppContext,
    source: &str,
    capabilities: Vec<Capabilities>,
) -> VisualTestContext {
    cx.update(crate::init);
    let runtime = ShellRuntime::new_isolated().expect("runtime");
    cx.update(|cx| runtime.set_global(cx));
    let view_type = runtime
        .load_source("document-images.js", source)
        .expect("source");
    let window = cx.add_window(move |window, cx| {
        let views = capabilities
            .into_iter()
            .enumerate()
            .map(|(ix, capabilities)| {
                let policy = Rc::new(
                    Policy::new()
                        .with_application(format!("document-{ix}"))
                        .with_capabilities(capabilities),
                );
                runtime
                    .instantiate_view_with_policy(&view_type, policy, window, cx)
                    .expect("view")
            })
            .collect();
        Documents(views)
    });
    VisualTestContext::from_window(*window.deref(), cx)
}

fn draw_documents(cx: &mut VisualTestContext) {
    for _ in 0..4 {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }
}

#[gpui::test]
fn text_view_document_images_require_policy(cx: &mut TestAppContext) {
    for (format, document) in [
        ("markdown", "![image](https://images.example/image.png)"),
        (
            "markdown",
            "Before ![image](https://images.example/image.png) after",
        ),
        (
            "markdown",
            "| Image |\n| --- |\n| ![image](https://images.example/image.png) |",
        ),
        ("html", r#"<img src="https://images.example/image.png">"#),
        (
            "html",
            r#"<p>Before <img width="2" height="1" src="https://images.example/image.png"> after</p>"#,
        ),
        (
            "html",
            r#"<table><tr><td><img src="https://images.example/image.png"></td></tr></table>"#,
        ),
    ] {
        for scrollable in [false, true] {
            for allowed in [false, true] {
                let mut app = cx.new_app();
                let (client, requests) = recording_client(|_| png_response());
                app.update(|cx| cx.set_http_client(client));
                let capabilities = if allowed {
                    get_grant(&["/image.png"])
                } else {
                    Capabilities::new()
                };
                let mut context = mount_documents(
                    &mut app,
                    &source(format, document, scrollable),
                    vec![capabilities],
                );
                draw_documents(&mut context);
                assert_eq!(
                    requests.lock().unwrap().len(),
                    usize::from(allowed),
                    "{format}, scrollable={scrollable}, allowed={allowed}: {document}",
                );
                // Repainting must use this document's cache, not refetch or fall back.
                draw_documents(&mut context);
                assert_eq!(requests.lock().unwrap().len(), usize::from(allowed));
                app.quit();
            }
        }
    }
}

#[gpui::test]
fn text_view_data_images_cannot_bypass_policy(cx: &mut TestAppContext) {
    // This valid image would otherwise be decoded directly by Base, bypassing
    // ImageCache::load entirely. Check the asset cache as well as the transport.
    let data = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAAEUlEQVR4nGMUMgn7z8DAwAAACQ0BnfdmzRUAAAAASUVORK5CYII=";
    for (format, document) in [
        ("markdown", format!("Before ![image]({data}) after")),
        ("html", format!(r#"<img src="{data}">"#)),
    ] {
        let mut app = cx.new_app();
        let (client, requests) = recording_client(|_| png_response());
        app.update(|cx| cx.set_http_client(client));
        let mut context = mount_documents(
            &mut app,
            &source(format, &document, false),
            vec![Capabilities::new()],
        );
        draw_documents(&mut context);
        assert!(requests.lock().unwrap().is_empty());
        let image = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            PNG.to_vec(),
        ));
        assert!(!context.update(|_, cx| gpui::ImageSource::Image(image).is_asset_cached(cx)));
        app.quit();
    }
}

#[gpui::test]
fn text_view_document_images_keep_policies_separate(cx: &mut TestAppContext) {
    let (client, requests) = recording_client(|url| {
        if url.ends_with("/image.png") {
            redirect(302, "/final.png")
        } else {
            png_response()
        }
    });
    cx.update(|cx| cx.set_http_client(client));
    let mut context = mount_documents(
        cx,
        &source(
            "markdown",
            "![image](https://images.example/image.png)",
            false,
        ),
        vec![
            get_grant(&["/image.png", "/final.png"]),
            get_grant(&["/image.png"]),
            Capabilities::new(),
        ],
    );
    draw_documents(&mut context);
    let mut actual = requests.lock().unwrap().clone();
    actual.sort();
    assert_eq!(
        actual,
        [
            "https://images.example/final.png",
            "https://images.example/image.png",
            "https://images.example/image.png",
        ],
        "only the first document may follow the redirect; none may borrow another cache",
    );
}

#[gpui::test]
fn document_svg_images_cannot_read_local_files(cx: &mut TestAppContext) {
    let path = std::env::temp_dir().join(format!("gpui-shell-svg-{}.png", std::process::id()));
    std::fs::write(&path, PNG).unwrap();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><image href="{}" width="2" height="1"/></svg>"#,
        path.display()
    );
    let decoded = cx.update(|cx| decode_image(svg.into_bytes(), cx.svg_renderer()));
    std::fs::remove_file(&path).unwrap();
    assert!(decoded.is_err());

    // An SVG that references no file still decodes.
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><rect width="2" height="1"/></svg>"#;
    let decoded = cx.update(|cx| decode_image(svg.as_bytes().to_vec(), cx.svg_renderer()));
    assert!(decoded.is_ok());
}

fn load_document_image(
    cx: &mut TestAppContext,
    capabilities: Capabilities,
    options: TextViewImageOptions,
    uri: &str,
) -> ImageResult {
    let policy = Rc::new(Policy::new().with_capabilities(capabilities));
    let owner = cx.new(|_| DocumentImages {
        policy,
        options,
        used: HashSet::new(),
    });
    let result = Arc::new(Mutex::new(None));
    let out = result.clone();
    let uri = SharedUri::from(uri.to_owned());
    let fut = cx.update(|cx| DocumentImage::load((owner.downgrade(), uri), cx));
    cx.background_executor
        .spawn(async move {
            *out.lock().unwrap() = Some(fut.await);
        })
        .detach();
    cx.run_until_parked();
    result.lock().unwrap().take().expect("image load completed")
}

#[gpui::test]
fn text_view_local_images_respect_base_dir_root_dir_and_capabilities(cx: &mut TestAppContext) {
    let base = std::env::temp_dir().join(format!("gpui-shell-local-imgs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let docs = base.join("docs");
    let sub = docs.join("sub");
    let spaced = docs.join("my dir");
    let assets = base.join("assets");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(&spaced).unwrap();
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(sub.join("pic.png"), PNG).unwrap();
    std::fs::write(spaced.join("pic.png"), PNG).unwrap();
    std::fs::write(assets.join("top.png"), PNG).unwrap();
    std::fs::write(
        sub.join("diagram.svg"),
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="4" height="2"/></svg>"#,
    )
    .unwrap();
    let outside = std::env::temp_dir().join(format!("gpui-shell-local-outside-{}.png", std::process::id()));
    std::fs::write(&outside, PNG).unwrap();

    let options = TextViewImageOptions {
        base_dir: Some(docs.to_string_lossy().into_owned()),
        root_dir: Some(base.to_string_lossy().into_owned()),
        allow_data_images: false,
    };

    let read_granted = Capabilities::new().read_roots([base.clone()]);
    let file_uri = reqwest::Url::from_file_path(sub.join("pic.png"))
        .unwrap()
        .to_string();

    for uri in [
        "./sub/pic.png",
        "sub/pic.png",
        "../assets/top.png",
        "my%20dir/pic.png",
        "/assets/top.png",
        "sub/diagram.svg",
        file_uri.as_str(),
    ] {
        assert!(
            load_document_image(cx, read_granted.clone(), options.clone(), uri).is_ok(),
            "expected local image to load with read grant: {uri}"
        );
        assert!(
            load_document_image(cx, Capabilities::new(), options.clone(), uri).is_err(),
            "expected empty capabilities to refuse local image: {uri}"
        );
    }

    // Read-root containment refuses paths outside the granted root even with base_dir/root_dir.
    let outside_file_uri = reqwest::Url::from_file_path(&outside).unwrap().to_string();
    assert!(load_document_image(cx, read_granted.clone(), options.clone(), &outside_file_uri).is_err());

    // Unrestricted execution grant (used by trusted host mods) also resolves local files.
    let unrestricted = Capabilities::new().execute(crate::ExecuteGrant::Unrestricted);
    assert!(load_document_image(cx, unrestricted, options.clone(), "./sub/pic.png").is_ok());

    // End-to-end JS -> TextView.markdown options deserialization.
    let options_json = serde_json::json!({
        "base_dir": docs.to_string_lossy(),
        "root_dir": base.to_string_lossy(),
        "allow_data_images": true,
    });
    let js = format!(
        r#"
import {{ View }} from "gpui-kit";
import {{ TextView }} from "gpui-base";
export default class Document extends View {{
  render() {{ return TextView.markdown("document", "![pic](./sub/pic.png)", {options_json}); }}
}}
"#
    );
    let mut app = cx.new_app();
    let mut context = mount_documents(&mut app, &js, vec![read_granted]);
    draw_documents(&mut context);
    app.quit();

    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&base);
}

#[gpui::test]
fn text_view_data_images_require_opt_in_option(cx: &mut TestAppContext) {
    let png_data = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAAEUlEQVR4nGMUMgn7z8DAwAAACQ0BnfdmzRUAAAAASUVORK5CYII=";
    let svg_data = "data:image/svg+xml,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%20width%3D%222%22%20height%3D%221%22%3E%3Crect%20width%3D%222%22%20height%3D%221%22%2F%3E%3C%2Fsvg%3E";

    let disabled = TextViewImageOptions::default();
    let enabled = TextViewImageOptions {
        allow_data_images: true,
        ..Default::default()
    };

    for uri in [png_data, svg_data] {
        assert!(
            load_document_image(cx, Capabilities::new(), disabled.clone(), uri).is_err(),
            "data image must remain refused without allow_data_images: {uri}"
        );
        assert!(
            load_document_image(cx, Capabilities::new(), enabled.clone(), uri).is_ok(),
            "data image must decode when allow_data_images is true: {uri}"
        );
    }
}

#[test]
fn shell_http_client_supports_get_and_redirect_policies() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            let first_line = req.lines().next().unwrap_or("");
            if first_line.starts_with("GET /redirect ") {
                stream
                    .write_all(b"HTTP/1.1 302 Found\r\nLocation: /final.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            } else {
                let mut reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    PNG.len()
                )
                .into_bytes();
                reply.extend_from_slice(PNG);
                stream.write_all(&reply).unwrap();
            }
        }
    });

    let client = crate::http_client("herdr2-test");
    let no_follow = smol::block_on(client.get(
        &format!("http://{addr}/redirect"),
        ().into(),
        false,
    ))
    .unwrap();
    assert_eq!(no_follow.status().as_u16(), 302);
    assert_eq!(
        no_follow.headers().get("location").unwrap().to_str().unwrap(),
        "/final.png"
    );

    let mut followed = smol::block_on(client.get(
        &format!("http://{addr}/redirect"),
        ().into(),
        true,
    ))
    .unwrap();
    assert_eq!(followed.status().as_u16(), 200);
    let mut bytes = Vec::new();
    smol::block_on(followed.body_mut().read_to_end(&mut bytes)).unwrap();
    assert_eq!(bytes, PNG);

    handle.join().unwrap();
}
