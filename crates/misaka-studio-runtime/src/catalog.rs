//! The model catalog — searching Hugging Face and reading what a repository actually contains.
//!
//! # Two calls, on purpose
//!
//! Search returns repositories; a repository holds up to twenty quantizations of one model, and
//! choosing between them is the decision this whole app exists to make easy. So the file list is
//! a second call, made when a repository is opened, and it carries the size and digest of every
//! file — which is what lets the UI say "Q4_K_M, 4.7 GB, fits" before anything is downloaded.
//!
//! # `lfs.oid` is the SHA-256, and that is worth a great deal
//!
//! Hugging Face stores large files in Git LFS, whose object id **is** the file's SHA-256. The
//! tree endpoint hands it over for free, so the Studio knows a model's digest before downloading
//! it: the download can be verified against a digest published by the repository rather than
//! against itself, and `h_M` can be derived the moment the file lands — with no 40 GB re-read.
//!
//! # The endpoint is configurable
//!
//! `HF_ENDPOINT` is the variable the rest of the ecosystem uses for mirrors and corporate
//! proxies, and it is what this reads. Nothing here hardcodes `huggingface.co` except the
//! default.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A repository, as search returns it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// `bartowski/Qwen3-4B-Instruct-GGUF`.
    pub id: String,
    pub downloads: u64,
    pub likes: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    pub last_modified: Option<String>,
    /// Gated repositories need an accepted licence and a token; the UI says so instead of
    /// letting the download fail with a 401.
    pub gated: bool,
    pub pipeline_tag: Option<String>,
}

/// One downloadable file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogFile {
    /// Path within the repository.
    pub path: String,
    pub size: Option<u64>,
    /// The LFS object id — the file's SHA-256 — when the file is LFS-tracked.
    pub sha256: Option<String>,
    /// Quantization parsed from the filename, so a file list can be sorted by quality.
    pub quantization: Option<misaka_studio_core::quant::Quantization>,
}

/// A repository with its files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogRepo {
    pub id: String,
    /// The commit the listing was taken at. Recorded in the sidecar so a downloaded model names
    /// an immutable revision rather than a branch that moves.
    pub revision: Option<String>,
    pub gated: bool,
    pub files: Vec<CatalogFile>,
    /// The base (unquantized) repository, from the card's `base_model` tag. Part of `h_M`.
    pub base_model: Option<String>,
}

/// Hugging Face's HTTP API.
pub struct Catalog {
    endpoint: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Catalog {
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Self {
        Catalog {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .user_agent(concat!("misaka-studio/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client builds"),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn authorized(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        }
    }

    /// Search for GGUF repositories.
    ///
    /// `filter=gguf` is what keeps the list to things this app can actually run: without it, a
    /// search for "qwen" returns hundreds of PyTorch repositories that no local runtime here
    /// loads, and every one of them is a dead end the user has to learn to recognise.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<CatalogEntry>> {
        let url = format!("{}/api/models", self.endpoint);
        let limit = limit.clamp(1, 100).to_string();
        let response = self
            .authorized(self.http.get(&url).query(&[
                ("search", query),
                ("filter", "gguf"),
                ("sort", "downloads"),
                ("direction", "-1"),
                ("limit", &limit),
            ]))
            .send()
            .await
            .map_err(|e| Error::Catalog { message: format!("{url}: {e}") })?;

        let response = check(response, &url).await?;
        let raw: Vec<RawModel> = decode(response, &url).await?;
        Ok(raw.into_iter().filter_map(RawModel::into_entry).collect())
    }

    /// The files in a repository, at a revision (`main` by default).
    pub async fn repo(&self, repo_id: &str, revision: Option<&str>) -> Result<CatalogRepo> {
        let revision = revision.unwrap_or("main");
        let info_url = format!("{}/api/models/{repo_id}/revision/{revision}", self.endpoint);
        let response = self
            .authorized(self.http.get(&info_url))
            .send()
            .await
            .map_err(|e| Error::Catalog { message: format!("{info_url}: {e}") })?;
        let response = check(response, &info_url).await?;
        let info: RawRepoInfo = decode(response, &info_url).await?;

        let tree_url = format!("{}/api/models/{repo_id}/tree/{revision}", self.endpoint);
        let response = self
            .authorized(self.http.get(&tree_url).query(&[("recursive", "true")]))
            .send()
            .await
            .map_err(|e| Error::Catalog { message: format!("{tree_url}: {e}") })?;
        let response = check(response, &tree_url).await?;
        let tree: Vec<RawTreeEntry> = decode(response, &tree_url).await?;

        let mut files: Vec<CatalogFile> = tree
            .into_iter()
            .filter(|e| e.kind.as_deref() != Some("directory"))
            .filter(|e| e.path.to_ascii_lowercase().ends_with(".gguf"))
            .map(|e| {
                let quantization = misaka_studio_core::quant::Quantization::from_filename(&e.path);
                CatalogFile {
                    // The LFS block carries the authoritative size and digest; the plain `size`
                    // of an LFS pointer is the size of the pointer, not the file.
                    size: e.lfs.as_ref().and_then(|l| l.size).or(e.size),
                    sha256: e.lfs.as_ref().and_then(|l| l.oid.clone()),
                    path: e.path,
                    quantization,
                }
            })
            .collect();
        // Largest first: within one repository, size tracks quality, and the top of the list is
        // where someone with a big machine should be looking.
        files.sort_by(|a, b| b.size.cmp(&a.size));

        Ok(CatalogRepo {
            id: repo_id.to_string(),
            revision: info.sha,
            gated: info.gated.map(|g| g.is_gated()).unwrap_or(false),
            base_model: info.card_data.and_then(|c| c.base_model.and_then(|b| b.first())),
            files,
        })
    }

    /// The URL a file is downloaded from.
    pub fn download_url(&self, repo_id: &str, revision: &str, path: &str) -> String {
        format!("{}/{repo_id}/resolve/{revision}/{path}", self.endpoint)
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

/// Turn a non-2xx into an error that names what to do about it.
/// Decode a JSON body, saying *what* failed when it fails.
///
/// `Response::json()` collapses every parse failure into the string "error decoding response
/// body" — no field, no offset, no clue whether the cause was the network or the shape. Hugging
/// Face adds and changes fields regularly, so that is precisely the error a user of this app is
/// most likely to hit, and it is the one that tells them least.
///
/// Reading the body first costs one allocation and buys a serde error that names the field and
/// the line, plus the text that was actually there. A search response is kilobytes; a repository
/// tree is tens of kilobytes. Neither is worth protecting at the cost of an unactionable error.
async fn decode<T: serde::de::DeserializeOwned>(response: reqwest::Response, url: &str) -> Result<T> {
    let body =
        response.text().await.map_err(|e| Error::Catalog { message: format!("{url}: the response body could not be read: {e}") })?;
    serde_json::from_str(&body).map_err(|e| {
        // The line serde stopped on, so the message carries the offending text and not just a
        // coordinate the reader has no way to look up.
        let context = body.lines().nth(e.line().saturating_sub(1)).map(|line| {
            let from = e.column().saturating_sub(60);
            let snippet: String = line.chars().skip(from).take(160).collect();
            format!(" — near: {snippet}")
        });
        Error::Catalog {
            message: format!("{url}: the response did not match what this client expects: {e}{}", context.unwrap_or_default()),
        }
    })
}

async fn check(response: reqwest::Response, url: &str) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let hint = match status.as_u16() {
        401 | 403 => " — this repository is gated or private; accept its licence on Hugging Face and add an access token in Settings",
        404 => " — no such repository or revision",
        429 => " — Hugging Face is rate-limiting this address; adding an access token in Settings raises the limit",
        _ => "",
    };
    Err(Error::Catalog { message: format!("{url} returned {status}{hint}. {}", body.chars().take(200).collect::<String>()) })
}

// --- wire shapes -----------------------------------------------------------
// Deserialization structs kept private and lenient: Hugging Face adds fields regularly, and a
// strict shape here would turn every upstream addition into a broken search box.

#[derive(Deserialize)]
struct RawModel {
    /// Hugging Face sends **both** `id` and `modelId`, carrying the same value.
    ///
    /// `#[serde(alias = "modelId")]` looks like the lenient way to accept either name and is the
    /// exact opposite: an alias points both names at one field, so a payload carrying both is a
    /// *duplicate field* and serde rejects the entire response — every search, for every query,
    /// with an error that named neither the field nor the cause. Two optional fields accept
    /// either name, both, or neither.
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "modelId")]
    model_id: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default, rename = "lastModified")]
    last_modified: Option<String>,
    #[serde(default)]
    gated: Option<Gated>,
    #[serde(default, rename = "pipeline_tag")]
    pipeline_tag: Option<String>,
}

impl RawModel {
    /// `None` for an entry carrying no identifier under either name: there is nothing to open and
    /// nothing to download, so it is dropped rather than allowed to fail the whole search.
    fn into_entry(self) -> Option<CatalogEntry> {
        let id = self.id.or(self.model_id)?;
        Some(CatalogEntry {
            id,
            downloads: self.downloads,
            likes: self.likes,
            tags: self.tags,
            last_modified: self.last_modified,
            gated: self.gated.map(|g| g.is_gated()).unwrap_or(false),
            pipeline_tag: self.pipeline_tag,
        })
    }
}

/// `gated` is `false`, `"auto"` or `"manual"` depending on the repository — an untagged union
/// that a plain `bool` field fails to parse, taking the whole search result with it.
#[derive(Deserialize)]
#[serde(untagged)]
enum Gated {
    Flag(bool),
    Kind(String),
}

impl Gated {
    fn is_gated(&self) -> bool {
        match self {
            Gated::Flag(b) => *b,
            Gated::Kind(s) => s != "false",
        }
    }
}

#[derive(Deserialize)]
struct RawRepoInfo {
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    gated: Option<Gated>,
    #[serde(default, rename = "cardData")]
    card_data: Option<RawCardData>,
}

#[derive(Deserialize)]
struct RawCardData {
    #[serde(default)]
    base_model: Option<BaseModel>,
}

/// `base_model` is a string in most cards and a list in merges.
#[derive(Deserialize)]
#[serde(untagged)]
enum BaseModel {
    One(String),
    Many(Vec<String>),
}

impl BaseModel {
    fn first(self) -> Option<String> {
        match self {
            BaseModel::One(s) => Some(s),
            BaseModel::Many(v) => v.into_iter().next(),
        }
    }
}

#[derive(Deserialize)]
struct RawTreeEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    path: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<RawLfs>,
}

#[derive(Deserialize)]
struct RawLfs {
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::routing::get;

    /// A stand-in for the Hugging Face API, so the catalog is tested against HTTP rather than
    /// against a mocked client — the wire shapes are exactly what this module gets wrong.
    async fn fake_hub() -> String {
        let app = axum::Router::new()
            .route(
                "/api/models",
                get(|| async {
                    Json(serde_json::json!([
                        // **Both** `id` and `modelId`, because that is what the real endpoint
                        // sends on every entry. This fixture used to carry one name per entry and
                        // never both, which is how `#[serde(alias = "modelId")]` on `id` — a
                        // duplicate field against a real response — passed its own test while
                        // failing every search in the shipped app.
                        {
                            "_id": "66e98ae0be5913b903da60c1",
                            "id": "bartowski/Qwen3-4B-Instruct-GGUF",
                            "modelId": "bartowski/Qwen3-4B-Instruct-GGUF",
                            "downloads": 12345, "likes": 42,
                            "tags": ["gguf", "text-generation"],
                            "lastModified": "2026-01-02T03:04:05.000Z",
                            "gated": false,
                            "pipeline_tag": "text-generation"
                        },
                        // `gated: "manual"` — a string where a bool would be expected. The shape
                        // that breaks a naive client.
                        { "id": "meta/gated-model-GGUF", "downloads": 7, "gated": "manual" },
                        // Only `modelId`: the older shape, still accepted.
                        { "modelId": "legacy/only-model-id-GGUF", "downloads": 3 },
                        // No identifier under either name — dropped, and it must not cost the
                        // user the other three results.
                        { "downloads": 1 }
                    ]))
                }),
            )
            .route(
                "/api/models/{*rest}",
                get(|axum::extract::Path(rest): axum::extract::Path<String>| async move {
                    if rest.contains("/tree/") {
                        Json(serde_json::json!([
                            { "type": "directory", "path": "docs" },
                            { "type": "file", "path": "README.md", "size": 900 },
                            { "type": "file", "path": "Qwen3-4B-Q4_K_M.gguf", "size": 135,
                              "lfs": { "oid": "aa11", "size": 2_600_000_000u64 } },
                            { "type": "file", "path": "Qwen3-4B-Q8_0.gguf", "size": 135,
                              "lfs": { "oid": "bb22", "size": 4_300_000_000u64 } }
                        ]))
                    } else {
                        Json(serde_json::json!({
                            "sha": "abc123def456",
                            "gated": false,
                            "cardData": { "base_model": ["Qwen/Qwen3-4B-Instruct"] }
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn search_reads_both_id_shapes_and_both_gated_shapes() {
        let catalog = Catalog::new(fake_hub().await, None);
        let results = catalog.search("qwen", 10).await.expect("searches");
        assert_eq!(results.len(), 3, "the entry with no identifier is dropped, the rest survive");
        // Carries `id` and `modelId` together, as every real entry does.
        assert_eq!(results[0].id, "bartowski/Qwen3-4B-Instruct-GGUF");
        assert_eq!(results[0].downloads, 12345);
        assert!(!results[0].gated);
        assert_eq!(results[1].id, "meta/gated-model-GGUF");
        assert!(results[1].gated, "\"manual\" is gated");
        assert_eq!(results[2].id, "legacy/only-model-id-GGUF", "`modelId` alone still identifies a repository");
    }

    #[tokio::test]
    async fn a_repo_listing_gives_sizes_digests_and_quantizations() {
        let catalog = Catalog::new(fake_hub().await, None);
        let repo = catalog.repo("bartowski/Qwen3-4B-Instruct-GGUF", None).await.expect("lists");
        assert_eq!(repo.revision.as_deref(), Some("abc123def456"));
        assert_eq!(repo.base_model.as_deref(), Some("Qwen/Qwen3-4B-Instruct"));
        assert_eq!(repo.files.len(), 2, "only GGUFs");

        // Largest first, and the LFS size wins over the pointer's own size.
        assert_eq!(repo.files[0].path, "Qwen3-4B-Q8_0.gguf");
        assert_eq!(repo.files[0].size, Some(4_300_000_000));
        assert_eq!(repo.files[0].sha256.as_deref(), Some("bb22"));
        assert_eq!(repo.files[1].quantization.as_ref().expect("quant").label, "Q4_K_M");
    }

    #[tokio::test]
    async fn a_missing_repo_explains_itself() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let app = axum::Router::new()
                .route("/api/models/{*rest}", get(|| async { (axum::http::StatusCode::NOT_FOUND, "Repo not found") }));
            let _ = axum::serve(listener, app).await;
        });
        let catalog = Catalog::new(format!("http://{addr}"), None);
        let err = catalog.repo("nobody/nothing", None).await.unwrap_err();
        assert!(err.to_string().contains("no such repository"), "got {err}");
    }

    #[test]
    fn download_urls_pin_the_revision() {
        let catalog = Catalog::new("https://example.test", None);
        assert_eq!(
            catalog.download_url("org/repo", "abc123", "model-Q4_K_M.gguf"),
            "https://example.test/org/repo/resolve/abc123/model-Q4_K_M.gguf"
        );
    }
}
