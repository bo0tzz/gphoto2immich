//! `POST /api/search/metadata` — used both for the dedup pre-check on each
//! photo and to find the most-recently-uploaded asset for backfill cutoff.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use super::{ensure_success, immich_datetime, ImmichClient};

const PATH: &str = "/api/search/metadata";

#[derive(Serialize, Debug)]
struct MetadataSearchBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "originalFileName")]
    original_file_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "takenAfter")]
    taken_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "takenBefore")]
    taken_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    make: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct MetadataSearchResp {
    assets: AssetsPage,
}

#[derive(Deserialize, Debug)]
struct AssetsPage {
    items: Vec<AssetSummary>,
}

#[derive(Deserialize, Debug)]
pub struct AssetSummary {
    pub id: String,
    #[serde(rename = "fileCreatedAt")]
    pub file_created_at: Option<DateTime<Utc>>,
    #[serde(rename = "originalFileName")]
    pub original_file_name: Option<String>,
}

impl ImmichClient {
    /// Check whether an asset with the given filename was taken within ±2 min
    /// of `taken_at`, scoped to Fuji. Used as the dedup pre-check on each
    /// photo before downloading from the camera.
    pub async fn find_existing(
        &self,
        filename: &str,
        taken_at: DateTime<Utc>,
    ) -> Result<Option<AssetSummary>> {
        let window = ChronoDuration::minutes(2);
        let body = MetadataSearchBody {
            original_file_name: Some(filename),
            taken_after: Some(immich_datetime(taken_at - window)),
            taken_before: Some(immich_datetime(taken_at + window)),
            make: Some("FUJIFILM"),
            model: None,
            order: None,
            page: Some(1),
            // Pull a few in case the server's substring matching grabs
            // neighbours we'd then filter out below.
            size: Some(10),
        };
        let hits = self.metadata_search(&body).await?;
        // `originalFileName` is a substring/ILIKE search on the Immich side,
        // not exact. Filter to exact (case-insensitive) matches so a
        // "DSCF1234.JPG" search doesn't accept e.g. "DSCF1234.RAF" or
        // "DSCF1234.JPG.bak".
        Ok(hits
            .assets
            .items
            .into_iter()
            .find(|a| {
                a.original_file_name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(filename))
            }))
    }

    /// Returns the `fileCreatedAt` of the most recently uploaded asset from
    /// the given Fuji camera model, or `None` if Immich has none.
    pub async fn most_recent_taken_at(
        &self,
        model: Option<&str>,
    ) -> Result<Option<DateTime<Utc>>> {
        let body = MetadataSearchBody {
            original_file_name: None,
            taken_after: None,
            taken_before: None,
            make: Some("FUJIFILM"),
            model,
            order: Some("desc"),
            page: Some(1),
            size: Some(1),
        };
        let resp = self.metadata_search(&body).await?;
        Ok(resp.assets.items.into_iter().next().and_then(|a| a.file_created_at))
    }

    async fn metadata_search(&self, body: &MetadataSearchBody<'_>) -> Result<MetadataSearchResp> {
        let resp = self
            .http()
            .post(self.url(PATH))
            .json(body)
            .send()
            .await
            .context("POST /api/search/metadata")?;
        let resp = ensure_success(resp, "metadata search").await?;
        let parsed = resp
            .json::<MetadataSearchResp>()
            .await
            .context("decoding metadata search response")?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn make_client(server: &MockServer) -> ImmichClient {
        ImmichClient::new(&server.uri(), "test-api-key").unwrap()
    }

    #[tokio::test]
    async fn find_existing_sends_filename_and_window_and_make() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(header("x-api-key", "test-api-key"))
            .and(wiremock::matchers::body_json(json!({
                "originalFileName": "DSCF0001.JPG",
                "takenAfter": "2026-05-16T11:58:00.000Z",
                "takenBefore": "2026-05-16T12:02:00.000Z",
                "make": "FUJIFILM",
                "page": 1,
                "size": 10
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": { "items": [
                    { "id": "abc-123", "originalFileName": "DSCF0001.JPG",
                      "fileCreatedAt": "2026-05-16T12:00:00+00:00" }
                ]}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server).await;
        let taken = Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap();
        let found = client.find_existing("DSCF0001.JPG", taken).await.unwrap();
        assert_eq!(found.unwrap().id, "abc-123");
    }

    #[tokio::test]
    async fn find_existing_ignores_substring_neighbour() {
        // If Immich's substring matching returns the JPEG sibling's RAF,
        // we must not treat it as an exact filename hit and skip the
        // JPEG download.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": { "items": [
                    { "id": "raf-id", "originalFileName": "DSCF0001.RAF",
                      "fileCreatedAt": "2026-05-16T12:00:00+00:00" }
                ]}
            })))
            .mount(&server)
            .await;

        let client = make_client(&server).await;
        let taken = Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap();
        assert!(client.find_existing("DSCF0001.JPG", taken).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn find_existing_picks_exact_match_among_noise() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": { "items": [
                    { "id": "raf-id", "originalFileName": "DSCF0001.RAF",
                      "fileCreatedAt": "2026-05-16T12:00:00+00:00" },
                    { "id": "jpeg-id", "originalFileName": "DSCF0001.JPG",
                      "fileCreatedAt": "2026-05-16T12:00:00+00:00" }
                ]}
            })))
            .mount(&server)
            .await;

        let client = make_client(&server).await;
        let taken = Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap();
        let found = client.find_existing("DSCF0001.JPG", taken).await.unwrap();
        assert_eq!(found.unwrap().id, "jpeg-id");
    }

    #[tokio::test]
    async fn find_existing_returns_none_when_no_match() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": { "items": [] }
            })))
            .mount(&server)
            .await;

        let client = make_client(&server).await;
        let taken = Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap();
        assert!(client.find_existing("DSCF0002.JPG", taken).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn most_recent_taken_at_with_model() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(wiremock::matchers::body_json(json!({
                "make": "FUJIFILM",
                "model": "X-T5",
                "order": "desc",
                "page": 1,
                "size": 1
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": { "items": [
                    { "id": "newest", "fileCreatedAt": "2026-05-15T20:30:00+00:00" }
                ]}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server).await;
        let when = client.most_recent_taken_at(Some("X-T5")).await.unwrap();
        assert_eq!(when.unwrap().to_rfc3339(), "2026-05-15T20:30:00+00:00");
    }

    #[tokio::test]
    async fn most_recent_taken_at_returns_none_when_empty() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": { "items": [] }
            })))
            .mount(&server)
            .await;

        let client = make_client(&server).await;
        assert!(client.most_recent_taken_at(None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn surfaces_server_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string("{\"error\":\"unauthorized\"}"),
            )
            .mount(&server)
            .await;

        let client = make_client(&server).await;
        let err = client
            .find_existing("X.JPG", Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("401"), "expected 401 in error: {msg}");
        assert!(msg.contains("unauthorized"), "expected body in error: {msg}");
    }
}
