//! Solana JSON-RPC over the mock ledger. Supports exactly the methods
//! transaction-service uses, plus `getRecentPrioritizationFees`.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::{Shared, ledger::SubmitError};

pub fn router() -> Router<Shared> {
    Router::new().route("/solana-rpc", post(handle).get(super::websocket::upgrade))
}

/// Fixed fee samples returned by `getRecentPrioritizationFees`.
const PRIORITIZATION_FEES: [(u64, u64); 3] =
    [(348_125_000, 0), (348_125_001, 1_000), (348_125_002, 5_000)];

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const PACKET_DATA_SIZE: usize = 1232;

/// Apply configured transport faults. `Some(response)` short-circuits.
pub async fn transport_faults(state: &Shared) -> Option<Response> {
    let (stall, limited, failed) = {
        let mut ledger = state.lock().expect("ledger");
        let faults = ledger.scenario.rpc.clone();
        let stall = ledger.chance(faults.stall_rate).then_some(faults.stall_ms);
        let limited = ledger.chance(faults.rate_limit_rate);
        let failed = !limited && ledger.chance(faults.server_error_rate);
        if stall.is_some() {
            ledger.count("rpc_stalls");
        }
        if limited {
            ledger.count("rpc_429");
        }
        if failed {
            ledger.count("rpc_503");
        }
        (stall, limited, failed)
    };
    if let Some(ms) = stall {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    if limited {
        return Some((StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response());
    }
    if failed {
        return Some((StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response());
    }
    None
}

async fn handle(State(state): State<Shared>, body: axum::body::Bytes) -> Response {
    let request: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return Json(error(Value::Null, PARSE_ERROR, "Parse error")).into_response(),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request
        .get("method")
        .and_then(Value::as_str)
        .filter(|_| request.get("jsonrpc").and_then(Value::as_str) == Some("2.0"))
    else {
        return Json(error(id, INVALID_REQUEST, "Invalid request")).into_response();
    };
    if method == "getRecentPrioritizationFees" {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": PRIORITIZATION_FEES
                .iter()
                .map(|(slot, fee)| json!({ "slot": slot, "prioritizationFee": fee }))
                .collect::<Vec<_>>(),
        }))
        .into_response();
    }
    if let Some(response) = transport_faults(&state).await {
        return response;
    }
    let params = request.get("params").cloned().unwrap_or(json!([]));
    let result = dispatch(&state, method, &params);
    Json(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => error(id, code, &message),
    })
    .into_response()
}

type RpcResult = Result<Value, (i64, String)>;

fn dispatch(state: &Shared, method: &str, params: &Value) -> RpcResult {
    let mut ledger = state.lock().expect("ledger");
    ledger.settle();
    ledger.count(method);
    let context = json!({"slot": ledger.slot()});
    match method {
        "getSlot" => Ok(json!(ledger.slot())),
        "getBlockHeight" => {
            let finalized = params[0]["commitment"].as_str() == Some("finalized");
            let height = ledger.height();
            Ok(json!(if finalized {
                height.saturating_sub(ledger.scenario.chain.finality_blocks)
            } else {
                height
            }))
        }
        "getBalance" => {
            let key = pubkey(&params[0])?;
            Ok(json!({"context": context, "value": ledger.lamports(&key)}))
        }
        "getTokenAccountsByOwner" => {
            let owner = pubkey(&params[0])?;
            // Filter by mint, or by program (every mock account is SPL Token).
            let matches: Vec<(Pubkey, Pubkey, u64, u8)> = if params[1].get("mint").is_some() {
                let mint = pubkey(&params[1]["mint"])?;
                ledger
                    .token_accounts_of(&owner, &mint)
                    .into_iter()
                    .map(|(address, amount, decimals)| (address, mint, amount, decimals))
                    .collect()
            } else if params[1]["programId"].as_str() == Some(crate::ledger::TOKEN_PROGRAM) {
                ledger.all_token_accounts_of(&owner)
            } else {
                Vec::new()
            };
            let accounts: Vec<Value> = matches
                .into_iter()
                .map(|(address, mint, amount, decimals)| {
                    json!({"pubkey": address.to_string(), "account": {"lamports": ledger.account_lamports(&address), "owner": crate::ledger::TOKEN_PROGRAM, "executable": false, "rentEpoch": 0,
                        "data": {"program": "spl-token", "space": 165, "parsed": {"type": "account", "info": {
                            "mint": mint.to_string(), "owner": owner.to_string(), "state": "initialized",
                            "tokenAmount": {"amount": amount.to_string(), "decimals": decimals}}}}}})
                })
                .collect();
            Ok(json!({"context": context, "value": accounts}))
        }
        "getLatestBlockhash" => {
            let (hash, last_valid) = ledger.issue_blockhash();
            Ok(
                json!({"context": context, "value": {"blockhash": hash.to_string(), "lastValidBlockHeight": last_valid}}),
            )
        }
        "sendTransaction" => send(&mut ledger, params, "rpc"),
        "simulateTransaction" => {
            let tx = decode(params)?;
            let wallet = params[1]["accounts"]["addresses"][0]
                .as_str()
                .and_then(|value| value.parse::<Pubkey>().ok())
                .unwrap_or(tx.message.static_account_keys()[0]);
            let (err, lamports) = ledger.simulate(&tx, &wallet);
            Ok(json!({"context": context, "value": {
                "err": err,
                "logs": [],
                "unitsConsumed": 60_000,
                "accounts": [{"lamports": lamports, "owner": "11111111111111111111111111111111", "data": ["", "base64"], "executable": false, "rentEpoch": 0}],
            }}))
        }
        "getSignatureStatuses" => {
            let values: Vec<Value> = params[0]
                .as_array()
                .ok_or((INVALID_PARAMS, "expected signature list".to_owned()))?
                .iter()
                .map(|value| {
                    let Some(signature) = value.as_str().and_then(|s| s.parse::<Signature>().ok())
                    else {
                        return Value::Null;
                    };
                    status(&ledger, &signature)
                })
                .collect();
            Ok(json!({"context": context, "value": values}))
        }
        "getSignaturesForAddress" => {
            let address = params[0]
                .as_str()
                .ok_or((INVALID_PARAMS, "expected address".into()))?;
            let mut rows:Vec<_>=ledger.txs.iter().filter_map(|(signature,record)|{
                let height=record.landed_height?;
                if ledger.height()<height+ledger.scenario.chain.finality_blocks {return None}
                let rendered=record.rendered.as_ref()?;
                if !rendered["transaction"]["message"]["accountKeys"].as_array()?.iter().any(|k|k["pubkey"].as_str()==Some(address)) {return None}
                Some(json!({"signature":signature.to_string(),"slot":rendered["slot"],"err":record.err,"confirmationStatus":"finalized"}))
            }).collect();
            rows.sort_by(|a, b| {
                b["slot"]
                    .as_u64()
                    .cmp(&a["slot"].as_u64())
                    .then_with(|| b["signature"].as_str().cmp(&a["signature"].as_str()))
            });
            if let Some(before) = params[1]["before"].as_str() {
                if let Some(index) = rows
                    .iter()
                    .position(|r| r["signature"].as_str() == Some(before))
                {
                    rows.drain(..=index);
                } else {
                    rows.clear();
                }
            }
            rows.truncate(params[1]["limit"].as_u64().unwrap_or(100) as usize);
            Ok(json!(rows))
        }
        "getTransaction" => {
            let signature = params[0]
                .as_str()
                .and_then(|value| value.parse::<Signature>().ok())
                .ok_or((INVALID_PARAMS, "invalid signature".to_owned()))?;
            let Some(record) = ledger.txs.get(&signature) else {
                return Ok(Value::Null);
            };
            let Some(landed) = record.landed_height else {
                return Ok(Value::Null);
            };
            let lag_blocks =
                ledger.scenario.rpc.metadata_lag_ms / ledger.scenario.chain.block_ms.max(1);
            if ledger.height() < landed + 1 + lag_blocks {
                return Ok(Value::Null);
            }
            Ok(record.rendered.clone().unwrap_or(Value::Null))
        }
        "isBlockhashValid" => {
            let valid = params[0]
                .as_str()
                .and_then(|value| value.parse().ok())
                .is_some_and(|hash| ledger.blockhash_valid(&hash));
            Ok(json!({"context": context, "value": valid}))
        }
        _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
    }
}

pub(super) fn status(ledger: &crate::ledger::Ledger, signature: &Signature) -> Value {
    let Some(record) = ledger.txs.get(signature) else {
        return Value::Null;
    };
    let Some(landed) = record.landed_height else {
        return Value::Null;
    };
    let block_ms = ledger.scenario.chain.block_ms.max(1);
    let lag_blocks = ledger.scenario.rpc.status_lag_ms.div_ceil(block_ms);
    let height = ledger.height();
    if height < landed + lag_blocks {
        return Value::Null;
    }
    let depth = height - landed;
    let confirmation = if depth >= ledger.scenario.chain.finality_blocks {
        "finalized"
    } else if depth >= 1 {
        "confirmed"
    } else {
        "processed"
    };
    json!({
        "slot": landed + 1_000,
        "confirmations": if confirmation == "finalized" { Value::Null } else { json!(depth) },
        "err": record.err,
        "status": match &record.err { Some(e) => json!({"Err": e}), None => json!({"Ok": null}) },
        "confirmationStatus": confirmation,
    })
}

pub fn send(ledger: &mut crate::ledger::Ledger, params: &Value, via: &'static str) -> RpcResult {
    let encoded = params[0].as_str().unwrap_or_default();
    let tx = decode(params)?;
    let latency = Duration::from_millis(ledger.scenario.chain.landing_latency_ms);
    let signature = match ledger.submit(tx, via, latency) {
        Ok(signature) => signature,
        Err(SubmitError::Invalid(message)) => return Err((INVALID_PARAMS, message)),
        Err(SubmitError::InsufficientFeePayer) => {
            return Err((-32002, "Transaction simulation failed: Attempt to debit an account but found no record of a prior credit.".into()));
        }
    };
    let _ = encoded;
    let (drop, error_after) = (
        ledger.scenario.rpc.drop_rate,
        ledger.scenario.rpc.send_error_after_landing_rate,
    );
    if ledger.chance(drop) {
        ledger.count("dropped");
        ledger.drop_pending(&signature);
    } else if ledger.chance(error_after) {
        ledger.count("send_error_after_landing");
        return Err((-32603, "Internal error: connection reset".into()));
    }
    Ok(json!(signature.to_string()))
}

fn decode(params: &Value) -> Result<VersionedTransaction, (i64, String)> {
    let encoded = params[0]
        .as_str()
        .ok_or((INVALID_PARAMS, "expected encoded transaction".to_owned()))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| (INVALID_PARAMS, "invalid base64".to_owned()))?;
    if bytes.len() > PACKET_DATA_SIZE {
        // Same error an RPC node returns for an oversize transaction.
        return Err((
            INVALID_PARAMS,
            format!(
                "base64 encoded solana_transaction::versioned::VersionedTransaction too large: {} bytes (max: encoded/raw 1644/{PACKET_DATA_SIZE})",
                encoded.len()
            ),
        ));
    }
    bincode::deserialize(&bytes).map_err(|_| (INVALID_PARAMS, "invalid transaction".to_owned()))
}

fn pubkey(value: &Value) -> Result<Pubkey, (i64, String)> {
    value
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or((INVALID_PARAMS, "invalid pubkey".to_owned()))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
