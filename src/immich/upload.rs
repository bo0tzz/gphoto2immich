//! `POST /api/assets` — multipart upload with the `x-immich-checksum` header
//! that lets the server short-circuit duplicates.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{multipart, StatusCode};
use serde::Deserialize;
use std::path::Path;

use super::{ensure_success, immich_datetime, ImmichClient};

const PATH: &str = "/api/assets";

#[derive(Debug, Clone)]
pub struct UploadRequest<'a> {
    pub file_path: &'a Path,
    pub filename: &'a str,
    pub file_created_at: DateTime<Utc>,
    pub sha1_hex: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    /// Newly created: server returned 201 with the new asset id.
    Created,
    /// Server recognised the checksum as an existing asset: 200 with the
    /// existing id.
    Duplicate,
}

#[derive(Debug, Clone)]
pub struct UploadResult {
    pub asset_id: String,
    pub outcome: UploadOutcome,
}

#[derive(Deserialize, Debug)]
struct AssetCreateResp {
    id: String,
    #[serde(default)]
    status: Option<String>,
}

impl ImmichClient {
    pub async fn upload(&self, req: UploadRequest<'_>) -> Result<UploadResult> {
        let bytes = tokio::fs::read(req.file_path)
            .await
            .with_context(|| format!("reading {} for upload", req.file_path.display()))?;
        let part = multipart::Part::bytes(bytes)
            .file_name(req.filename.to_owned())
            .mime_str("application/octet-stream")
            .context("setting multipart mime")?;
        let form = multipart::Form::new()
            .text("fileCreatedAt", immich_datetime(req.file_created_at))
            .text("fileModifiedAt", immich_datetime(req.file_created_at))
            .text("isFavorite", "false")
            .part("assetData", part);

        let resp = self
            .http()
            .post(self.url(PATH))
            .header("x-immich-checksum", req.sha1_hex)
            .multipart(form)
            .send()
            .await
            .context("POST /api/assets")?;
        let status = resp.status();
        let resp = ensure_success(resp, "asset upload").await?;
        let body: AssetCreateResp = resp.json().await.context("decoding upload response")?;
        let outcome = match status {
            StatusCode::CREATED => UploadOutcome::Created,
            StatusCode::OK => UploadOutcome::Duplicate,
            other => return Err(anyhow!("unexpected 2xx status from /api/assets: {other}")),
        };
        // Immich also signals duplicate-handling via the `status` field
        // (e.g. "duplicate"); fall back to that if status code is ambiguous.
        let outcome = match (outcome, body.status.as_deref()) {
            (_, Some("duplicate")) => UploadOutcome::Duplicate,
            (o, _) => o,
        };
        Ok(UploadResult {
            asset_id: body.id,
            outcome,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> ImmichClient {
        ImmichClient::new(&server.uri(), "test-api-key").unwrap()
    }

    fn make_payload() -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"FUJIFILMDATA").unwrap();
        f
    }

    #[tokio::test]
    async fn fresh_upload_returns_created() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/assets"))
            .and(header("x-api-key", "test-api-key"))
            .and(header("x-immich-checksum", "deadbeef"))
            .and(header_exists("content-type"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "new-asset-id",
                "status": "created"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let file = make_payload();
        let client = make_client(&server);
        let result = client
            .upload(UploadRequest {
                file_path: file.path(),
                filename: "DSCF0001.JPG",
                file_created_at: Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap(),
                sha1_hex: "deadbeef",
            })
            .await
            .unwrap();
        assert_eq!(result.asset_id, "new-asset-id");
        assert_eq!(result.outcome, UploadOutcome::Created);
    }

    #[tokio::test]
    async fn duplicate_returns_duplicate() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/assets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "existing-asset-id",
                "status": "duplicate"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let file = make_payload();
        let client = make_client(&server);
        let result = client
            .upload(UploadRequest {
                file_path: file.path(),
                filename: "DSCF0001.JPG",
                file_created_at: Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap(),
                sha1_hex: "deadbeef",
            })
            .await
            .unwrap();
        assert_eq!(result.asset_id, "existing-asset-id");
        assert_eq!(result.outcome, UploadOutcome::Duplicate);
    }

    #[tokio::test]
    async fn surfaces_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/assets"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let file = make_payload();
        let client = make_client(&server);
        let err = client
            .upload(UploadRequest {
                file_path: file.path(),
                filename: "DSCF0001.JPG",
                file_created_at: Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap(),
                sha1_hex: "deadbeef",
            })
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("500"), "msg: {msg}");
        assert!(msg.contains("boom"), "msg: {msg}");
    }
}
