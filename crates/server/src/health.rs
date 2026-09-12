//! `/health` endpoint — M0's whole surface: "daemon is alive, here is who I am".
//! The body type is `peregrine_api::HealthInfo` (single source of truth).

use axum::Json;
use axum::routing::get;
use peregrine_api::HealthInfo;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct Health {
    pub pid: u32,
    pub started: Instant,
}

pub fn router(state: Health) -> axum::Router {
    let state = std::sync::Arc::new(state);
    async fn health(
        axum::extract::State(state): axum::extract::State<std::sync::Arc<Health>>,
    ) -> Json<HealthInfo> {
        Json(HealthInfo::ok(state.pid, state.started.elapsed().as_secs()))
    }

    axum::Router::new()
        .route("/health", get(health))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_identity() {
        let app = router(Health {
            pid: 1234,
            started: Instant::now(),
        });
        let res = app
            .oneshot(
                axum::http::Request::get("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["name"], "peregrine");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["pid"], 1234);
        assert_eq!(body["version"], peregrine_api::VERSION);
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let app = router(Health {
            pid: 1,
            started: Instant::now(),
        });
        let res = app
            .oneshot(
                axum::http::Request::get("/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 404);
    }
}
