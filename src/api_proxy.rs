//! TCP API proxy: clients can reach the HTTP API through hbbs when the API
//! port is not directly reachable (RustDesk `HttpProxyRequest` fallback).
//!
//! Only paths under `/api` are forwarded. Configure the upstream with
//! `--api-server` / `API_SERVER` (default: `http://127.0.0.1:<hbbs_port-2>`).

use hbb_common::{
    anyhow::{anyhow, bail},
    bytes::BytesMut,
    log,
    rendezvous_proto::{HeaderEntry, HttpProxyRequest, HttpProxyResponse, RendezvousMessage},
    tokio::sync::Semaphore,
    ResultType,
};
use std::{net::IpAddr, sync::Arc};

const HTTP_PROXY_MAX_BODY: usize = 2 * 1024 * 1024;
const HTTP_PROXY_MAX_PATH: usize = 2048;
const HTTP_PROXY_MAX_HEADERS: usize = 32;
const HTTP_PROXY_MAX_HEADER_BYTES: usize = 16 * 1024;
const HTTP_PROXY_MAX_CONCURRENCY: usize = 32;
const HTTP_PROXY_TIMEOUT_SECS: u64 = 15;

#[derive(Clone)]
pub struct ApiProxy {
    api_server: String,
    http_client: reqwest::Client,
    slots: Arc<Semaphore>,
}

impl ApiProxy {
    pub fn new(api_server: String) -> ResultType<Self> {
        validate_api_server(&api_server)?;
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_PROXY_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        log::info!("API proxy upstream: {}", api_server);
        Ok(Self {
            api_server,
            http_client,
            slots: Arc::new(Semaphore::new(HTTP_PROXY_MAX_CONCURRENCY)),
        })
    }

    pub fn default_for_port(port: i32) -> ResultType<Self> {
        Self::new(format!("http://127.0.0.1:{}", port - 2))
    }

    pub fn from_arg_or_default(port: i32) -> ResultType<Self> {
        let configured = crate::common::get_arg("api-server");
        if configured.is_empty() {
            Self::default_for_port(port)
        } else {
            Self::new(configured)
        }
    }

    pub async fn handle(&self, req: HttpProxyRequest, client_ip: IpAddr) -> HttpProxyResponse {
        match self.slots.clone().try_acquire_owned() {
            Ok(_permit) => match self.forward(req, client_ip).await {
                Ok(resp) => resp,
                Err(err) => {
                    log::warn!("API proxy from {} failed: {}", client_ip, err);
                    HttpProxyResponse {
                        status: 502,
                        error: "API upstream request failed".to_owned(),
                        ..Default::default()
                    }
                }
            },
            Err(_) => HttpProxyResponse {
                status: 503,
                error: "API proxy is busy".to_owned(),
                ..Default::default()
            },
        }
    }

    async fn forward(
        &self,
        req: HttpProxyRequest,
        client_ip: IpAddr,
    ) -> ResultType<HttpProxyResponse> {
        validate_http_proxy_path(&req.path)?;
        if req.body.len() > HTTP_PROXY_MAX_BODY {
            bail!("HTTP proxy request body is too large");
        }
        if req.headers.len() > HTTP_PROXY_MAX_HEADERS {
            bail!("HTTP proxy request has too many headers");
        }
        let header_bytes = req.headers.iter().try_fold(0usize, |total, header| {
            total
                .checked_add(header.name.len())
                .and_then(|value| value.checked_add(header.value.len()))
        });
        if header_bytes.map(|n| n > HTTP_PROXY_MAX_HEADER_BYTES).unwrap_or(true) {
            bail!("HTTP proxy request headers are too large");
        }

        let method = http_proxy_method(&req.method)?;
        let base = self.api_server.trim_end_matches('/');
        let url = format!("{}{}", base, req.path);
        let mut builder = self.http_client.request(method, url);
        for entry in &req.headers {
            if !is_allowed_http_proxy_request_header(&entry.name) {
                continue;
            }
            let name = reqwest::header::HeaderName::from_bytes(entry.name.as_bytes())?;
            let value = reqwest::header::HeaderValue::from_str(&entry.value)?;
            builder = builder.header(name, value);
        }
        builder = builder
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header("x-forwarded-for", client_ip.to_string())
            .header("x-real-ip", client_ip.to_string());

        let mut resp = builder.body(req.body.clone()).send().await?;
        if resp.content_length().unwrap_or_default() > HTTP_PROXY_MAX_BODY as u64 {
            bail!("HTTP proxy response body is too large");
        }

        let status = resp.status().as_u16() as i32;
        let mut headers = Vec::new();
        let mut response_header_bytes = 0usize;
        for (name, value) in resp
            .headers()
            .iter()
            .filter(|(name, _)| !is_hop_by_hop_header(name.as_str()))
        {
            let Ok(value) = value.to_str() else {
                continue;
            };
            response_header_bytes = response_header_bytes
                .checked_add(name.as_str().len())
                .and_then(|total| total.checked_add(value.len()))
                .ok_or_else(|| anyhow!("HTTP proxy response headers are too large"))?;
            if headers.len() >= HTTP_PROXY_MAX_HEADERS
                || response_header_bytes > HTTP_PROXY_MAX_HEADER_BYTES
            {
                bail!("HTTP proxy response headers are too large");
            }
            headers.push(HeaderEntry {
                name: name.as_str().to_owned(),
                value: value.to_owned(),
                ..Default::default()
            });
        }

        let mut body = BytesMut::with_capacity(
            resp.content_length()
                .unwrap_or_default()
                .min(HTTP_PROXY_MAX_BODY as u64) as usize,
        );
        while let Some(chunk) = resp.chunk().await? {
            let new_len = body
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| anyhow!("HTTP proxy response body is too large"))?;
            if new_len > HTTP_PROXY_MAX_BODY {
                bail!("HTTP proxy response body is too large");
            }
            body.extend_from_slice(&chunk);
        }

        Ok(HttpProxyResponse {
            status,
            headers,
            body: body.freeze().into(),
            ..Default::default()
        })
    }
}

pub fn wrap_response(resp: HttpProxyResponse) -> RendezvousMessage {
    let mut msg_out = RendezvousMessage::new();
    msg_out.set_http_proxy_response(resp);
    msg_out
}

fn validate_api_server(api_server: &str) -> ResultType<()> {
    if api_server.is_empty() {
        bail!("API server must not be empty");
    }
    let url = reqwest::Url::parse(api_server)?;
    let Some(host) = url.host_str() else {
        bail!("API server must include a host");
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.port() == Some(0)
    {
        bail!(
            "API server must be an HTTP(S) origin without credentials, path, query or fragment"
        );
    }
    if url.scheme() == "http" {
        let host_without_brackets = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let is_loopback = host.eq_ignore_ascii_case("localhost")
            || host_without_brackets
                .parse::<IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false);
        if !is_loopback {
            bail!("Remote API servers must use HTTPS");
        }
    }
    Ok(())
}

fn allowed_http_proxy_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn validate_http_proxy_path(path: &str) -> ResultType<()> {
    if path.is_empty()
        || path.len() > HTTP_PROXY_MAX_PATH
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.contains("://")
        || path.contains('\\')
        || path.contains('#')
    {
        bail!("HTTP proxy path must be relative");
    }
    if path.chars().any(char::is_control) {
        bail!("HTTP proxy path contains control characters");
    }
    let raw_path = path.split_once('?').map_or(path, |(raw_path, _)| raw_path);
    let lowercase_path = raw_path.to_ascii_lowercase();
    if lowercase_path.contains("%2e")
        || lowercase_path.contains("%2f")
        || lowercase_path.contains("%5c")
        || lowercase_path.contains("%25")
    {
        bail!("HTTP proxy path contains an ambiguous encoded path segment");
    }
    let parsed = reqwest::Url::parse(&format!("http://proxy.invalid{path}"))?;
    if parsed.host_str() != Some("proxy.invalid") || !allowed_http_proxy_path(parsed.path()) {
        bail!("HTTP proxy path is not allowed");
    }
    Ok(())
}

fn is_allowed_http_proxy_request_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "accept"
            | "accept-language"
            | "authorization"
            | "content-type"
            | "if-modified-since"
            | "if-none-match"
            | "range"
            | "user-agent"
            | "x-csrf-token"
            | "x-requested-with"
    )
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

fn http_proxy_method(method: &str) -> ResultType<reqwest::Method> {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Ok(reqwest::Method::GET),
        "POST" => Ok(reqwest::Method::POST),
        "PUT" => Ok(reqwest::Method::PUT),
        "DELETE" => Ok(reqwest::Method::DELETE),
        _ => bail!("HTTP proxy method is not allowed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_loopback_http_api() {
        assert!(validate_api_server("http://127.0.0.1:21114").is_ok());
        assert!(validate_api_server("http://localhost:21114").is_ok());
    }

    #[test]
    fn rejects_remote_http_api() {
        assert!(validate_api_server("http://example.com:21114").is_err());
    }

    #[test]
    fn accepts_remote_https_api() {
        assert!(validate_api_server("https://api.example.com").is_ok());
    }

    #[test]
    fn only_api_paths() {
        assert!(validate_http_proxy_path("/api/heartbeat").is_ok());
        assert!(validate_http_proxy_path("/api/login").is_ok());
        assert!(validate_http_proxy_path("/admin").is_err());
        assert!(validate_http_proxy_path("/api/../etc/passwd").is_err());
    }
}
