/// HTTP daemon: serves review requests over localhost.
/// CLI clients can connect via `quorum review --daemon` instead of parsing locally.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::cache::ParseCache;
use crate::config::{Config, EnvConfigSource};
use crate::feedback::FeedbackStore;
use crate::finding::Finding;
use crate::llm_client::OpenAiClient;
use crate::parser::Language;
use crate::pipeline::{self, LlmReviewer, PipelineConfig};

/// Shared state for the HTTP daemon.
pub struct DaemonState {
    pub parse_cache: Arc<ParseCache>,
    pub config: Config,
    pub feedback_store: FeedbackStore,
    pub llm_reviewer: Option<Box<dyn LlmReviewer>>,
}

#[derive(Deserialize)]
pub struct ReviewRequest {
    pub file_path: String,
    pub code: String,
}

#[derive(Serialize, Deserialize)]
pub struct ReviewResponse {
    pub findings: Vec<Finding>,
    pub cache_hit: bool,
}

#[derive(Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub cache_size: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub cache: CacheStatsJson,
    pub feedback_count: usize,
}

#[derive(Serialize)]
pub struct CacheStatsJson {
    pub size: usize,
    pub capacity: usize,
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
}

/// Build the axum router.
pub fn build_router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/review", post(review))
        .with_state(state)
}

async fn health(State(state): State<Arc<DaemonState>>) -> Json<HealthResponse> {
    let cs = state.parse_cache.stats();
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        cache_size: cs.size,
        cache_hits: cs.hits,
        cache_misses: cs.misses,
    })
}

async fn stats(State(state): State<Arc<DaemonState>>) -> Json<StatsResponse> {
    let cs = state.parse_cache.stats();
    let feedback_count = state.feedback_store.count().unwrap_or(0);
    Json(StatsResponse {
        cache: CacheStatsJson {
            size: cs.size,
            capacity: cs.capacity,
            hits: cs.hits,
            misses: cs.misses,
            hit_rate: cs.hit_rate(),
        },
        feedback_count,
    })
}

async fn review(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<ReviewRequest>,
) -> Result<Json<ReviewResponse>, (StatusCode, String)> {
    let lang = Language::from_path(std::path::Path::new(&req.file_path))
        .ok_or_else(|| (StatusCode::BAD_REQUEST, format!("Unsupported file type: {}", req.file_path)))?;

    let cache_before = state.parse_cache.stats().hits;

    let feedback = state.feedback_store.load_all().unwrap_or_default();
    let pipeline_cfg = PipelineConfig {
        models: vec![state.config.model.clone()],
        feedback,
        ..Default::default()
    };

    let result = pipeline::review_source(
        std::path::Path::new(&req.file_path),
        &req.code,
        lang,
        state.llm_reviewer.as_deref(),
        &pipeline_cfg,
        Some(&state.parse_cache),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Review error: {}", e)))?;

    let cache_hit = state.parse_cache.stats().hits > cache_before;

    Ok(Json(ReviewResponse {
        findings: result.findings,
        cache_hit,
    }))
}

/// Default socket path for the daemon.
pub fn default_socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(dir).join("quorum.sock")
}

/// Default port for the daemon.
pub const DEFAULT_PORT: u16 = 7842;

/// Create the daemon state.
pub fn create_daemon_state(cache_capacity: usize) -> anyhow::Result<Arc<DaemonState>> {
    let config = Config::load(&EnvConfigSource)?;
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let feedback_path = PathBuf::from(&home).join(".quorum/feedback.jsonl");
    let feedback_store = FeedbackStore::new(feedback_path);

    let llm_reviewer: Option<Box<dyn LlmReviewer>> = if let Ok(api_key) = config.require_api_key() {
        Some(Box::new(OpenAiClient::new(&config.base_url, api_key)))
    } else {
        None
    };

    Ok(Arc::new(DaemonState {
        parse_cache: Arc::new(ParseCache::new(cache_capacity)),
        config,
        feedback_store,
        llm_reviewer,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state() -> Arc<DaemonState> {
        Arc::new(DaemonState {
            parse_cache: Arc::new(ParseCache::new(10)),
            config: Config {
                base_url: "https://example.com".into(),
                api_key: None,
                model: "test".into(),
            },
            feedback_store: FeedbackStore::new(PathBuf::from("/tmp/quorum-http-test.jsonl")),
            llm_reviewer: None,
        })
    }

    #[tokio::test]
    async fn health_endpoint() {
        let app = build_router(test_state());
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.status, "ok");
    }

    #[tokio::test]
    async fn stats_endpoint() {
        let app = build_router(test_state());
        let req = Request::builder()
            .uri("/stats")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn review_endpoint_rust() {
        let state = test_state();
        let app = build_router(state.clone());

        let body = serde_json::json!({
            "file_path": "test.rs",
            "code": "fn main() { let x = 42; }"
        });

        let req = Request::builder()
            .method("POST")
            .uri("/review")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let review: ReviewResponse = serde_json::from_slice(&body).unwrap();
        assert!(!review.cache_hit, "First request should be a cache miss");

        // Second request should hit cache
        let app2 = build_router(state.clone());
        let body2 = serde_json::json!({
            "file_path": "test.rs",
            "code": "fn main() { let x = 42; }"
        });
        let req2 = Request::builder()
            .method("POST")
            .uri("/review")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body2).unwrap()))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        let body2 = axum::body::to_bytes(resp2.into_body(), usize::MAX).await.unwrap();
        let review2: ReviewResponse = serde_json::from_slice(&body2).unwrap();
        assert!(review2.cache_hit, "Second request should be a cache hit");
    }

    #[tokio::test]
    async fn review_endpoint_python_with_findings() {
        let app = build_router(test_state());
        let body = serde_json::json!({
            "file_path": "app.py",
            "code": "SECRET_KEY = \"my-secret-abc123\"\napp.run(debug=True, host=\"0.0.0.0\")"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/review")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let review: ReviewResponse = serde_json::from_slice(&body).unwrap();
        assert!(!review.findings.is_empty(), "Should find issues in vulnerable Python");
    }

    #[tokio::test]
    async fn review_endpoint_unsupported_extension() {
        let app = build_router(test_state());
        let body = serde_json::json!({
            "file_path": "file.xyz",
            "code": "some code"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/review")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
