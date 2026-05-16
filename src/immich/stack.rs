//! `POST /api/stacks` + the asset-state check that decides whether to skip
//! stack creation because Immich already has one for the JPEG.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{ensure_success, ImmichClient};

const STACKS_PATH: &str = "/api/stacks";

#[derive(Serialize)]
struct StackCreateBody<'a> {
    #[serde(rename = "assetIds")]
    asset_ids: &'a [String],
}

#[derive(Deserialize)]
struct AssetDetail {
    #[serde(rename = "stack")]
    stack: Option<serde_json::Value>,
}

impl ImmichClient {
    /// Returns `true` if Immich already has a stack record attached to this
    /// asset, in which case we skip POST /api/stacks.
    pub async fn asset_has_stack(&self, asset_id: &str) -> Result<bool> {
        let url = self.url(&format!("/api/assets/{asset_id}"));
        let resp = self
            .http()
            .get(url)
            .send()
            .await
            .context("GET /api/assets/{id}")?;
        let resp = ensure_success(resp, "asset detail").await?;
        let body: AssetDetail = resp.json().await.context("decoding asset detail")?;
        Ok(body.stack.is_some())
    }

    /// Create a stack containing the given asset ids. The first id is treated
    /// as the primary by Immich's controller.
    pub async fn create_stack(&self, asset_ids: &[String]) -> Result<()> {
        let body = StackCreateBody { asset_ids };
        let resp = self
            .http()
            .post(self.url(STACKS_PATH))
            .json(&body)
            .send()
            .await
            .context("POST /api/stacks")?;
        let _ = ensure_success(resp, "stack create").await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> ImmichClient {
        ImmichClient::new(&server.uri(), "test-api-key").unwrap()
    }

    #[tokio::test]
    async fn asset_has_stack_true() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/assets/abc-123$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "abc-123",
                "stack": { "id": "stack-1", "primaryAssetId": "abc-123" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        assert!(client.asset_has_stack("abc-123").await.unwrap());
    }

    #[tokio::test]
    async fn asset_has_stack_false_when_null() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/assets/.*$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "abc-123",
                "stack": null
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        assert!(!client.asset_has_stack("abc-123").await.unwrap());
    }

    #[tokio::test]
    async fn create_stack_sends_asset_ids() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/stacks"))
            .and(wiremock::matchers::body_json(json!({
                "assetIds": ["jpeg-id", "raf-id"]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "stack-1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        client
            .create_stack(&["jpeg-id".to_owned(), "raf-id".to_owned()])
            .await
            .unwrap();
    }
}
