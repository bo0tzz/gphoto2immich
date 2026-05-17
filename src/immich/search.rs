//! `POST /api/search/metadata` — the only Immich endpoint we hit during
//! dedup. Used as a paginated primitive by [`super::cache::ImmichCache`];
//! the per-file `find_existing` and `most_recent_taken_at` shapes that
//! used to live here are now satisfied client-side from the cache.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ensure_success, ImmichClient};

const PATH: &str = "/api/search/metadata";

#[derive(Serialize, Debug)]
struct MetadataSearchBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    make: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<&'a str>,
    page: u32,
    size: u32,
}

#[derive(Deserialize, Debug)]
pub(super) struct MetadataSearchResp {
    pub assets: AssetsPage,
}

#[derive(Deserialize, Debug)]
pub(super) struct AssetsPage {
    pub items: Vec<AssetSummary>,
    /// `null` on the last page; some Immich versions return a string page
    /// number, others return a numeric one — we only care whether it's
    /// present, not its value.
    #[serde(rename = "nextPage", default)]
    pub next_page: Option<serde_json::Value>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AssetSummary {
    pub id: String,
    #[serde(rename = "fileCreatedAt")]
    pub file_created_at: Option<DateTime<Utc>>,
    #[serde(rename = "originalFileName")]
    pub original_file_name: Option<String>,
}

impl ImmichClient {
    /// Fetch one page of metadata search results, optionally scoped to
    /// a single EXIF `Make`, ordered desc by `fileCreatedAt` so the
    /// newest asset is first on page 1. Used by
    /// [`super::cache::ImmichCache::load`] to enumerate all relevant
    /// assets once per session instead of running one HTTP request per
    /// file on the camera.
    pub(super) async fn list_metadata_page(
        &self,
        make: Option<&str>,
        page: u32,
        size: u32,
    ) -> Result<AssetsPage> {
        let body = MetadataSearchBody {
            make,
            order: Some("desc"),
            page,
            size,
        };
        let resp = self
            .http()
            .post(self.url(PATH))
            .json(&body)
            .send()
            .await
            .context("POST /api/search/metadata")?;
        let resp = ensure_success(resp, "metadata search").await?;
        let parsed: MetadataSearchResp = resp
            .json()
            .await
            .context("decoding metadata search response")?;
        Ok(parsed.assets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> ImmichClient {
        ImmichClient::new(&server.uri(), "test-api-key").unwrap()
    }

    #[tokio::test]
    async fn list_metadata_page_sends_expected_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(header("x-api-key", "test-api-key"))
            .and(wiremock::matchers::body_json(json!({
                "make": "FUJIFILM",
                "order": "desc",
                "page": 1,
                "size": 250
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": {
                    "items": [
                        { "id": "a", "originalFileName": "DSCF0001.JPG",
                          "fileCreatedAt": "2026-05-16T12:00:00+00:00" }
                    ],
                    "nextPage": null
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        let page = client
            .list_metadata_page(Some("FUJIFILM"), 1, 250)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "a");
        assert!(page.next_page.is_none());
    }

    #[tokio::test]
    async fn list_metadata_page_omits_make_when_none() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            // No `make` field in the body.
            .and(wiremock::matchers::body_json(json!({
                "order": "desc",
                "page": 1,
                "size": 250
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": { "items": [], "nextPage": null }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = make_client(&server);
        let page = client.list_metadata_page(None, 1, 250).await.unwrap();
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn list_metadata_page_surfaces_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let err = client
            .list_metadata_page(Some("FUJIFILM"), 1, 250)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("401"), "msg: {msg}");
        assert!(msg.contains("nope"), "msg: {msg}");
    }
}
