//! Real JSON-RPC PubSub transport backed by the same ledger as HTTP.
use crate::Shared;
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use serde_json::{Value, json};
use solana_signature::Signature;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

pub async fn upgrade(State(state): State<Shared>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| serve(state, socket))
}

struct Subscription {
    method: String,
    key: String,
    finalized: bool,
    seen: HashSet<Signature>,
    started: Instant,
}

async fn serve(state: Shared, mut socket: WebSocket) {
    let mut subscriptions: HashMap<u64, Subscription> = HashMap::new();
    let mut next_id = 0_u64;
    let connected = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    state.lock().unwrap().count("ws_connections");
    loop {
        tokio::select! {
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                match message {
                    Message::Text(text) => {
                        let Ok(request) = serde_json::from_str::<Value>(&text) else { continue };
                        let method = request["method"].as_str().unwrap_or("");
                        let result = {
                            let mut ledger = state.lock().unwrap();
                            ledger.count(method);
                            if method.ends_with("Unsubscribe") {
                                json!({"result": subscriptions.remove(&request["params"][0].as_u64().unwrap_or(0)).is_some()})
                            } else if ledger.scenario.websocket.reject_subscriptions {
                                json!({"error":{"code":-32000,"message":"subscription unavailable"}})
                            } else if matches!(method, "signatureSubscribe" | "logsSubscribe") {
                                next_id += 1;
                                let key = if method == "signatureSubscribe" { &request["params"][0] } else { &request["params"][0]["mentions"][0] };
                                // Logs are a live stream, never a historical replay.
                                let seen = ledger.txs.iter().filter(|(_, r)| r.landed_height.is_some()).map(|(s,_)| *s).collect();
                                subscriptions.insert(next_id, Subscription {
                                    method: method.into(), key: key.as_str().unwrap_or("").into(),
                                    finalized: request["params"][1]["commitment"] == "finalized",
                                    seen, started: Instant::now(),
                                });
                                json!({"result":next_id})
                            } else { json!({"error":{"code":-32601,"message":"unsupported subscription"}}) }
                        };
                        let mut response = result;
                        response["jsonrpc"] = json!("2.0");
                        response["id"] = request["id"].clone();
                        if socket.send(Message::Text(response.to_string().into())).await.is_err() { break }
                    }
                    Message::Ping(payload) => { if socket.send(Message::Pong(payload)).await.is_err() { break } }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = tick.tick() => {
                let (close, notifications) = {
                    let mut ledger = state.lock().unwrap();
                    ledger.settle();
                    let faults = ledger.scenario.websocket.clone();
                    let close = faults.disconnect_after_ms > 0 && connected.elapsed().as_millis() >= u128::from(faults.disconnect_after_ms);
                    let mut notifications = Vec::new();
                    let mut done = Vec::new();
                    for (id, sub) in &mut subscriptions {
                        if faults.drop_notifications || sub.started.elapsed().as_millis() < u128::from(faults.notification_delay_ms) { continue }
                        if sub.method == "signatureSubscribe" {
                            let Ok(signature) = sub.key.parse() else { continue };
                            let status = super::solana::status(&ledger, &signature);
                            let commitment = status["confirmationStatus"].as_str().unwrap_or("");
                            if commitment != "finalized" && (sub.finalized || commitment != "confirmed") { continue }
                            notifications.push(json!({"jsonrpc":"2.0","method":"signatureNotification","params":{"subscription":id,"result":{"context":{"slot":status["slot"]},"value":{"err":status["err"]}}}}));
                            done.push(*id);
                        } else {
                            for (signature, record) in &ledger.txs {
                                if sub.seen.contains(signature) || record.land_at < sub.started || !record.tx.message.static_account_keys().iter().any(|k| k.to_string() == sub.key) { continue }
                                let status = super::solana::status(&ledger, signature);
                                if !matches!(status["confirmationStatus"].as_str(), Some("confirmed" | "finalized")) { continue }
                                sub.seen.insert(*signature);
                                notifications.push(json!({"jsonrpc":"2.0","method":"logsNotification","params":{"subscription":id,"result":{"context":{"slot":status["slot"]},"value":{"signature":signature.to_string(),"err":status["err"],"logs":[]}}}}));
                            }
                        }
                    }
                    for id in done { subscriptions.remove(&id); }
                    if faults.duplicate_notifications { notifications.extend(notifications.clone()); }
                    for _ in &notifications { ledger.count("ws_notifications"); }
                    (close, notifications)
                };
                if close { let _ = socket.send(Message::Close(None)).await; break }
                for notification in notifications {
                    if socket.send(Message::Text(notification.to_string().into())).await.is_err() { return }
                }
            }
        }
    }
}
