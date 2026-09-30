//! Mock of external providers (Solana RPC, Jupiter Swap/Price/Trigger, Jito)
//! for local testing of transaction-service. Holds no secrets and never
//! touches a real chain: all state lives in an in-memory ledger.

mod config;
mod ledger;
mod providers;
mod scenario;

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

use crate::{
    config::{AppEnv, Config},
    ledger::Ledger,
    scenario::Scenario,
};

pub type Shared = Arc<Mutex<Ledger>>;

fn app() -> Router {
    let state: Shared = Arc::new(Mutex::new(Ledger::new(Scenario::default())));
    Router::new()
        .route("/health", get(health))
        .merge(providers::solana::router())
        .merge(providers::jupiter::router())
        .merge(providers::jito::router())
        .merge(providers::control::router())
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env = AppEnv::parse(std::env::var("APP_ENV").ok().as_deref())?;
    let config = Config::load(env, Path::new("config"))?;
    let address = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!(
        "provider-mock-service ({}) listening on {address}",
        env.name()
    );
    axum::serve(listener, app())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use super::*;

    async fn call(request: Request<Body>) -> (StatusCode, Value) {
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn rpc(body: Value) -> (StatusCode, Value) {
        call(
            Request::post("/solana-rpc")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (status, body) = call(Request::get("/health").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn recent_prioritization_fees_are_deterministic() {
        let request = json!({"jsonrpc": "2.0", "id": 7, "method": "getRecentPrioritizationFees", "params": []});
        let (status, first) = rpc(request.clone()).await;
        let (_, second) = rpc(request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first, second);
        assert_eq!(
            first,
            json!({"jsonrpc": "2.0", "id": 7, "result": [
                {"slot": 348_125_000, "prioritizationFee": 0},
                {"slot": 348_125_001, "prioritizationFee": 1_000},
                {"slot": 348_125_002, "prioritizationFee": 5_000},
            ]})
        );
    }

    #[tokio::test]
    async fn unsupported_method_returns_json_rpc_error() {
        let (status, body) =
            rpc(json!({"jsonrpc": "2.0", "id": "a", "method": "getProgramAccounts", "params": []}))
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], "a");
        assert_eq!(body["error"]["code"], -32601);
        assert!(body.get("result").is_none());
    }

    #[tokio::test]
    async fn malformed_requests_return_json_rpc_errors() {
        let (_, body) = call(
            Request::post("/solana-rpc")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await;
        assert_eq!(body["error"]["code"], -32700);
        let (_, body) = rpc(json!({"id": 1, "method": "getRecentPrioritizationFees"})).await;
        assert_eq!(body["error"]["code"], -32600);
    }

    #[test]
    fn both_environment_configs_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
        let local = Config::load(AppEnv::Local, &dir).unwrap();
        assert_eq!(
            (local.server.host.as_str(), local.server.port),
            ("127.0.0.1", 8080)
        );
        let production = Config::load(AppEnv::Production, &dir).unwrap();
        assert_eq!(production.server.host, "0.0.0.0");
    }

    #[test]
    fn missing_or_unknown_environment_fails() {
        assert_eq!(
            AppEnv::parse(None),
            Err(config::ConfigError::MissingEnvironment)
        );
        assert!(matches!(
            AppEnv::parse(Some("staging")),
            Err(config::ConfigError::UnknownEnvironment(_))
        ));
    }
}
