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

#[cfg(test)]
fn app() -> Router { app_for_scope("all") }
fn app_for_scope(scope: &str) -> Router {
    let state: Shared = Arc::new(Mutex::new(Ledger::new(Scenario::default())));
    let router = Router::new()
        .route("/health", get(health))
        .merge(providers::control::router_for_scope(scope));
    // Snapshot and full-simulation providers share endpoint paths, so select
    // one implementation rather than mounting overlapping Axum routes.
    let router = if scope == "snapshots" {
        router.merge(providers::snapshot::router())
    } else {
        router.merge(providers::solana::router())
            .merge(providers::jupiter::router())
            .merge(providers::jito::router())
            .merge(providers::evidence::router())
    };
    router.with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env = AppEnv::parse(std::env::var("APP_ENV").ok().as_deref())?;
    let config = Config::load(
        env,
        Path::new(&std::env::var("MOCK_CONFIG_DIR").unwrap_or_else(|_| "config".into())),
    )?;
    let scope = std::env::var("MOCK_PROVIDERS").unwrap_or_else(|_| "all".into());
    if !matches!(scope.as_str(), "all" | "snapshots") {
        return Err("MOCK_PROVIDERS must be all or snapshots".into());
    }
    let primary = std::env::var("MOCK_BIND_ADDR").ok();
    let legacy = std::env::var("MOCK_BIND").ok();
    if matches!((&primary, &legacy), (Some(a), Some(b)) if a != b) {
        return Err("conflicting mock bind overrides".into());
    }
    let address = match primary.or(legacy) {
        Some(address) => {
            let parsed: std::net::SocketAddr = address.parse()?;
            if env != AppEnv::Local || !parsed.ip().is_loopback() {
                return Err("mock bind override requires local loopback".into());
            }
            address
        }
        None => format!("{}:{}", config.server.host, config.server.port),
    };
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!(
        "provider-mock-service ({}) listening on {address}",
        env.name()
    );
    axum::serve(listener, app_for_scope(&scope))
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
    async fn snapshot_scope_disables_unrelated_transaction_routes() {
        let response=app_for_scope("snapshots").oneshot(Request::post("/solana-rpc").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn snapshot_scope_retains_request_audit_and_head_configuration() {
        let router = app_for_scope("snapshots");
        let configured = router.clone().oneshot(Request::post("/__mock/scenario")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"telegram":{"head_message_id":321},"birdeye":{"faults":{"/defi/price":[{"status":null}]}}}"#)).unwrap()).await.unwrap();
        assert_eq!(configured.status(), StatusCode::OK);
        let head = router.clone().oneshot(Request::post("/telegram/provider")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"op":"head"}"#)).unwrap()).await.unwrap();
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(head.into_body(), 1 << 16).await.unwrap()).unwrap();
        assert_eq!(body["head_message_id"], 321);
        let audit = router.oneshot(Request::get("/__mock/requests").body(Body::empty()).unwrap()).await.unwrap();
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(audit.into_body(), 1 << 16).await.unwrap()).unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (status, body) = call(Request::get("/health").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn authenticated_vault_matches_the_execution_vault() {
        let wallet = solana_pubkey::Pubkey::new_unique();
        let (_, auth) = call(
            Request::post("/jupiter/trigger/v2/auth/verify")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"walletPubkey":wallet.to_string()}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        let (status, vault) = call(
            Request::get("/jupiter/trigger/v2/vault")
                .header(
                    "authorization",
                    format!("Bearer {}", auth["token"].as_str().unwrap()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            vault["vaultPubkey"],
            crate::ledger::vault_of(&wallet).to_string()
        );
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
