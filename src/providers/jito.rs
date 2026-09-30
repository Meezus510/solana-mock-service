//! Jito block engine `sendTransaction`. Lands through the same ledger as RPC.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};

use crate::Shared;

pub fn router() -> Router<Shared> {
    Router::new().route("/jito", post(handle))
}

async fn handle(State(state): State<Shared>, Json(request): Json<Value>) -> Response {
    let mut ledger = state.lock().expect("ledger");
    if ledger.scenario.jito.down {
        ledger.count("jito_down");
        return (StatusCode::SERVICE_UNAVAILABLE, "jito unavailable").into_response();
    }
    ledger.settle();
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let params = request.get("params").cloned().unwrap_or(json!([]));
    Json(match super::solana::send(&mut ledger, &params, "jito") {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
    .into_response()
}
