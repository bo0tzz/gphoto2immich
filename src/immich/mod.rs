//! Thin client for the Immich HTTP API.
//!
//! All functions return on the tokio runtime. The camera thread schedules
//! Immich calls via the runtime handle in the upload pipeline.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::{header, Client, StatusCode, Url};

pub mod search;
pub mod stack;
pub mod upload;

pub use upload::{UploadOutcome, UploadRequest};

#[derive(Clone)]
pub struct ImmichClient {
    http: Client,
    base: Url,
}

impl ImmichClient {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self> {
        let base = parse_base_url(base_url)?;
        let mut default_headers = header::HeaderMap::new();
        let auth = header::HeaderValue::from_str(api_key)
            .context("API key contains invalid header characters")?;
        default_headers.insert("x-api-key", auth);
        default_headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );

        let http = Client::builder()
            .user_agent(concat!("fujimmich/", env!("CARGO_PKG_VERSION")))
            .default_headers(default_headers)
            .timeout(Duration::from_secs(60))
            .build()
            .context("building reqwest client")?;
        Ok(ImmichClient { http, base })
    }

    /// Resolve a path relative to the base URL. Paths must start with `/`.
    pub fn url(&self, path: &str) -> Url {
        debug_assert!(path.starts_with('/'));
        // base URL is guaranteed to have a trailing slash by `parse_base_url`,
        // so .join with a leading-slash path appends correctly even if base
        // already includes a path prefix.
        self.base.join(path.trim_start_matches('/')).expect("valid path")
    }

    pub(crate) fn http(&self) -> &Client {
        &self.http
    }
}

fn parse_base_url(s: &str) -> Result<Url> {
    let mut url = Url::parse(s).map_err(|e| anyhow!("invalid IMMICH_URL {s:?}: {e}"))?;
    if !url.path().ends_with('/') {
        let mut path = url.path().to_owned();
        path.push('/');
        url.set_path(&path);
    }
    Ok(url)
}

/// Surface a more useful error than reqwest's default when the server
/// rejects with a non-2xx status.
async fn ensure_success(resp: reqwest::Response, op: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(anyhow!(
        "{op} failed with {status}: {}",
        truncate(&body, 500)
    ))
}

/// Some Immich responses include 200 + an asset id for known-duplicate
/// uploads; callers want to distinguish that from a real success.
fn is_ok_or_dup(status: StatusCode) -> bool {
    status.is_success()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_join_with_no_trailing_slash() {
        let c = ImmichClient::new("https://immich.example.com", "abc").unwrap();
        assert_eq!(
            c.url("/api/search/metadata").as_str(),
            "https://immich.example.com/api/search/metadata"
        );
    }

    #[test]
    fn url_join_with_path_prefix() {
        let c = ImmichClient::new("https://example.com/immich/", "abc").unwrap();
        assert_eq!(
            c.url("/api/assets").as_str(),
            "https://example.com/immich/api/assets"
        );
    }

    #[test]
    fn rejects_invalid_base_url() {
        assert!(ImmichClient::new("not a url", "abc").is_err());
    }
}
