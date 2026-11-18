// HERDR: production GPUI HttpClient backed by reqwest::blocking off-thread
// (GPUI runs on smol without a Tokio runtime, so async reqwest would panic).

use std::{io::Read as _, pin::Pin, sync::Arc, time::Duration};

use gpui::http_client::{
    AsyncBody, HttpClient, RedirectPolicy, Request, RequestTimeout, Response, Url,
    http::{HeaderName, HeaderValue},
};
use smol::io::AsyncReadExt as _;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;

/// Creates an [`HttpClient`] for GPUI's `Application::with_http_client`.
pub fn http_client(user_agent: &str) -> Arc<dyn HttpClient> {
    Arc::new(ShellHttpClient::new(user_agent))
}

struct ShellHttpClient {
    user_agent_str: String,
    user_agent: Option<HeaderValue>,
    no_follow: Result<reqwest::blocking::Client, String>,
    follow: Result<reqwest::blocking::Client, String>,
}

impl ShellHttpClient {
    fn new(user_agent: &str) -> Self {
        let user_agent_header = HeaderValue::from_str(user_agent).ok();
        let no_follow = build_client(user_agent, reqwest::redirect::Policy::none());
        let follow = build_client(user_agent, reqwest::redirect::Policy::limited(MAX_REDIRECTS));
        Self {
            user_agent_str: user_agent.to_owned(),
            user_agent: user_agent_header,
            no_follow,
            follow,
        }
    }

    fn client_for(&self, policy: Option<&RedirectPolicy>) -> anyhow::Result<reqwest::blocking::Client> {
        match policy {
            None | Some(RedirectPolicy::NoFollow) | Some(RedirectPolicy::FollowLimit(0)) => {
                self.no_follow.clone().map_err(anyhow::Error::msg)
            }
            Some(RedirectPolicy::FollowAll) => self.follow.clone().map_err(anyhow::Error::msg),
            Some(RedirectPolicy::FollowLimit(limit)) => {
                if *limit as usize == MAX_REDIRECTS {
                    self.follow.clone().map_err(anyhow::Error::msg)
                } else {
                    build_client(
                        &self.user_agent_str,
                        reqwest::redirect::Policy::limited((*limit as usize).min(20)),
                    )
                    .map_err(anyhow::Error::msg)
                }
            }
        }
    }
}

fn build_client(
    user_agent: &str,
    redirect: reqwest::redirect::Policy,
) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .timeout(DEFAULT_TIMEOUT)
        .redirect(redirect)
        .build()
        .map_err(|error| format!("creating HTTP client failed: {error}"))
}

impl HttpClient for ShellHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        self.user_agent.as_ref()
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<Response<AsyncBody>>> + Send + 'static>>
    {
        let policy = req.extensions().get::<RedirectPolicy>().cloned();
        let timeout = req.extensions().get::<RequestTimeout>().copied();
        let client_result = self.client_for(policy.as_ref());
        let (parts, mut body) = req.into_parts();

        Box::pin(async move {
            let client = client_result?;
            let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())?;
            let url = parts.uri.to_string();

            let mut req_bytes = Vec::new();
            (&mut body)
                .take(MAX_REQUEST_BYTES + 1)
                .read_to_end(&mut req_bytes)
                .await?;
            if req_bytes.len() as u64 > MAX_REQUEST_BYTES {
                anyhow::bail!("HTTP request body exceeded the {MAX_REQUEST_BYTES} byte limit");
            }

            let mut headers = reqwest::header::HeaderMap::new();
            for (name, value) in &parts.headers {
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
                    reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
                ) {
                    headers.append(name, value);
                }
            }

            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<Response<AsyncBody>> {
                    let mut builder = client.request(method, &url).headers(headers);
                    if let Some(RequestTimeout(duration)) = timeout {
                        builder = builder.timeout(duration);
                    }
                    if !req_bytes.is_empty() {
                        builder = builder.body(req_bytes);
                    }
                    let response = builder.send()?;
                    let status = response.status().as_u16();
                    let mut out = Response::builder().status(status);
                    for (name, value) in response.headers() {
                        if let (Ok(name), Ok(value)) = (
                            HeaderName::from_bytes(name.as_str().as_bytes()),
                            HeaderValue::from_bytes(value.as_bytes()),
                        ) {
                            out = out.header(name, value);
                        }
                    }
                    let mut bytes = Vec::new();
                    response
                        .take(MAX_RESPONSE_BYTES + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
                        anyhow::bail!(
                            "HTTP response body exceeded the {MAX_RESPONSE_BYTES} byte limit"
                        );
                    }
                    Ok(out.body(AsyncBody::from(bytes))?)
                })();
                let _ = tx.send_blocking(result);
            });

            rx.recv()
                .await
                .map_err(|_| anyhow::anyhow!("HTTP request worker stopped unexpectedly"))?
        })
    }
}
