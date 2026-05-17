//! Per-session snapshot of Fuji assets in Immich, built up-front so the
//! per-file dedup pre-check is a local hash lookup instead of an HTTP
//! round-trip. Pulls every FUJIFILM asset's `(originalFileName,
//! fileCreatedAt, id)` via paginated metadata search.
//!
//! Trade-offs: one-time cost of N pages instead of N per-file requests;
//! cache is point-in-time so we can't detect assets that appear during a
//! session — fine since the daemon's session is a single sweep and ends
//! once the camera filesystem walk completes.
//!
//! Memory is bounded by user's Fuji asset count. ~80 bytes per asset, so
//! 100k assets is ~8 MB.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};

use super::ImmichClient;

const PAGE_SIZE: u32 = 250;
/// Same window we used when this was a per-file HTTP query — see the
/// commit history for the rationale: Immich rewrites `fileCreatedAt`
/// from EXIF on ingest, which drifts from our mtime-derived `taken_at`
/// by minutes in either direction.
const TAKEN_AT_WINDOW: Duration = Duration::hours(24);
const MAKE: &str = "FUJIFILM";

pub struct ImmichCache {
    by_filename: HashMap<String, Vec<Entry>>,
    max_taken_at: Option<DateTime<Utc>>,
    asset_count: usize,
}

struct Entry {
    taken_at: DateTime<Utc>,
    asset_id: String,
}

impl ImmichCache {
    /// Build a fresh cache by paging the metadata search until `nextPage`
    /// is null. Costs one HTTP request per `PAGE_SIZE` assets in Immich.
    pub async fn load(client: &ImmichClient) -> Result<Self> {
        let mut by_filename: HashMap<String, Vec<Entry>> = HashMap::new();
        let mut max_taken_at: Option<DateTime<Utc>> = None;
        let mut asset_count = 0usize;
        let mut page = 1u32;

        loop {
            let page_resp = client
                .list_by_make_page(MAKE, page, PAGE_SIZE)
                .await
                .with_context(|| format!("loading Immich asset cache (page {page})"))?;
            let next_page_present = page_resp.next_page.is_some();
            let returned = page_resp.items.len();
            for asset in page_resp.items {
                let (Some(name), Some(taken)) =
                    (asset.original_file_name.as_deref(), asset.file_created_at)
                else {
                    continue;
                };
                let key = name.to_ascii_lowercase();
                by_filename.entry(key).or_default().push(Entry {
                    taken_at: taken,
                    asset_id: asset.id,
                });
                if max_taken_at.is_none_or(|t| taken > t) {
                    max_taken_at = Some(taken);
                }
                asset_count += 1;
            }
            // Some Immich versions stop returning `nextPage` on empty
            // pages instead of providing an explicit signal — defend
            // against both.
            if !next_page_present || returned == 0 {
                break;
            }
            page += 1;
        }

        Ok(Self {
            by_filename,
            max_taken_at,
            asset_count,
        })
    }

    /// Find a previously uploaded Fuji asset matching `(filename, taken_at)`.
    /// Match is case-insensitive on the exact filename, with a ±24h window
    /// on `fileCreatedAt`.
    pub fn find_existing(&self, filename: &str, taken_at: DateTime<Utc>) -> Option<&str> {
        let candidates = self.by_filename.get(&filename.to_ascii_lowercase())?;
        candidates
            .iter()
            .find(|e| e.taken_at.signed_duration_since(taken_at).abs() <= TAKEN_AT_WINDOW)
            .map(|e| e.asset_id.as_str())
    }

    /// The newest `fileCreatedAt` we saw — what the backfill cutoff is
    /// derived from. `None` when Immich has no Fuji assets at all.
    pub fn max_taken_at(&self) -> Option<DateTime<Utc>> {
        self.max_taken_at
    }

    /// Total number of (asset, fileCreatedAt) pairs cached. Useful for
    /// log lines.
    pub fn asset_count(&self) -> usize {
        self.asset_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn make_client(server: &MockServer) -> ImmichClient {
        ImmichClient::new(&server.uri(), "test-api-key").unwrap()
    }

    fn asset(id: &str, name: &str, taken: &str) -> serde_json::Value {
        json!({
            "id": id,
            "originalFileName": name,
            "fileCreatedAt": taken,
        })
    }

    #[tokio::test]
    async fn load_handles_single_page() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": {
                    "items": [
                        asset("a", "DSCF0001.JPG", "2026-05-16T12:00:00+00:00"),
                        asset("b", "DSCF0002.JPG", "2026-05-16T12:01:00+00:00"),
                    ],
                    "nextPage": null
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cache = ImmichCache::load(&make_client(&server)).await.unwrap();
        assert_eq!(cache.asset_count(), 2);
        assert_eq!(
            cache.max_taken_at().unwrap().to_rfc3339(),
            "2026-05-16T12:01:00+00:00"
        );
    }

    #[tokio::test]
    async fn load_paginates_until_next_page_null() {
        let server = MockServer::start().await;
        // Page 1: returns nextPage="2".
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(move |req: &Request| {
                let body: serde_json::Value = req.body_json().unwrap();
                body["page"] == 1
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": {
                    "items": [ asset("a", "DSCF0001.JPG", "2026-05-16T12:00:00+00:00") ],
                    "nextPage": "2"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Page 2: returns nextPage=null.
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(move |req: &Request| {
                let body: serde_json::Value = req.body_json().unwrap();
                body["page"] == 2
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": {
                    "items": [ asset("b", "DSCF0002.JPG", "2026-05-17T12:00:00+00:00") ],
                    "nextPage": null
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cache = ImmichCache::load(&make_client(&server)).await.unwrap();
        assert_eq!(cache.asset_count(), 2);
    }

    #[tokio::test]
    async fn find_existing_matches_case_insensitively_within_window() {
        use chrono::TimeZone;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": {
                    "items": [ asset("a", "DSCF0001.JPG", "2026-05-16T12:00:00+00:00") ],
                    "nextPage": null
                }
            })))
            .mount(&server)
            .await;
        let cache = ImmichCache::load(&make_client(&server)).await.unwrap();

        // Same filename, taken_at within ±24h -> hit.
        let close = Utc.with_ymd_and_hms(2026, 5, 16, 18, 0, 0).unwrap();
        assert_eq!(cache.find_existing("DSCF0001.JPG", close), Some("a"));
        // Case-insensitive on the name.
        assert_eq!(cache.find_existing("dscf0001.jpg", close), Some("a"));
        // Outside the window: no hit.
        let far = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap();
        assert!(cache.find_existing("DSCF0001.JPG", far).is_none());
        // Unknown filename: no hit.
        assert!(cache.find_existing("DSCF9999.JPG", close).is_none());
    }

    #[tokio::test]
    async fn find_existing_disambiguates_cross_folder_repeats() {
        use chrono::TimeZone;
        let server = MockServer::start().await;
        // Same filename, two different shoots months apart (e.g. file
        // counter rollover on the card).
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "assets": {
                    "items": [
                        asset("old", "DSCF1234.JPG", "2026-01-01T12:00:00+00:00"),
                        asset("new", "DSCF1234.JPG", "2026-05-16T12:00:00+00:00"),
                    ],
                    "nextPage": null
                }
            })))
            .mount(&server)
            .await;
        let cache = ImmichCache::load(&make_client(&server)).await.unwrap();

        let near_old = Utc.with_ymd_and_hms(2026, 1, 1, 18, 0, 0).unwrap();
        let near_new = Utc.with_ymd_and_hms(2026, 5, 16, 18, 0, 0).unwrap();
        assert_eq!(cache.find_existing("DSCF1234.JPG", near_old), Some("old"));
        assert_eq!(cache.find_existing("DSCF1234.JPG", near_new), Some("new"));
    }
}
