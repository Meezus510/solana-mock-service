//! Test control API under `/__mock`: set the scenario, reset state, inspect
//! the ledger, and mint a throwaway wallet for the service under test.

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::{Shared, ledger::Ledger, scenario::Scenario};

pub fn router_for_scope(scope: &str) -> Router<Shared> {
    let router = Router::new()
        .route("/__mock/scenario", post(set_scenario).get(get_scenario))
        .route("/__mock/reset", post(reset))
        .route("/__mock/state", get(state))
        .route("/__mock/keypair", post(keypair));
    if scope == "snapshots" {
        router.route("/__mock/requests", get(requests))
    } else {
        router
    }
}

async fn set_scenario(State(state): State<Shared>, Json(scenario): Json<Scenario>) -> Json<Value> {
    let mut ledger=state.lock().expect("ledger");
    ledger.scenario=scenario;
    ledger.snapshot_counters.clear();
    Json(json!({"status": "ok"}))
}

async fn get_scenario(State(state): State<Shared>) -> Json<Scenario> {
    Json(state.lock().expect("ledger").scenario.clone())
}

/// Fresh chain state, keeping the scenario unless one is supplied.
async fn reset(State(state): State<Shared>, body: Option<Json<Scenario>>) -> Json<Value> {
    let mut ledger = state.lock().expect("ledger");
    let scenario = body.map_or_else(|| ledger.scenario.clone(), |Json(scenario)| scenario);
    *ledger = Ledger::new(scenario);
    Json(json!({"status": "ok"}))
}

async fn state(State(state): State<Shared>) -> Json<Value> {
    let mut ledger = state.lock().expect("ledger");
    ledger.settle();
    Json(ledger.state_summary())
}

/// A throwaway wallet for local runs, so the real signer never has to be
/// used against the mock.
async fn keypair() -> Json<Value> {
    let keypair = Keypair::new();
    Json(json!({
        "pubkey": keypair.pubkey().to_string(),
        "secret_base58": bs58::encode(keypair.to_bytes()).into_string(),
    }))
}

async fn requests(State(state): State<Shared>) -> Json<Value> { Json(json!(state.lock().expect("ledger").snapshot_requests)) }
