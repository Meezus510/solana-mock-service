//! Birdeye public WebSocket (`/socket/solana`) emulating `SUBSCRIBE_PRICE` with
//! 1m OHLCV `PRICE_DATA` frames. Bars are derived from the same scenario
//! `birdeye.tokens[mint].candles` that `/defi/v3/ohlcv` serves, so a healthy
//! stream and REST agree; `birdeye.ws` injects the stream's failure modes.
//!
//! Each REST bar `{o,h,l,c,v}` becomes running-bar frames at
//! `tick_offsets_ms` into its minute: first trade at `h`, then `l`, then `c`,
//! with `v` cumulative. Like the real feed, the frame's `o` is the minute's
//! first trade, not the previous close that REST opens at.
use crate::Shared;
use axum::{
    Json,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BirdeyeWsConfig {
    /// Offsets into the minute at which the running bar is sent.
    pub tick_offsets_ms: Vec<u64>,
    /// Unix seconds at which every open connection is closed (once each).
    pub disconnect_at: Vec<f64>,
    /// Answer subscriptions with an error and send nothing.
    pub reject_subscriptions: bool,
    /// Extra delay applied to every PRICE_DATA frame.
    pub frame_delay_ms: u64,
    /// Unix seconds at which one malformed frame is sent on each connection.
    pub malformed_at: Vec<f64>,
    /// Addresses allowed per SUBSCRIBE_PRICE (Birdeye: 100); more closes the socket.
    pub max_addresses: usize,
    /// Bars the stream never sends although REST has them: `{"mint","unix_time"}`.
    pub skip: Vec<SkipBar>,
}

impl Default for BirdeyeWsConfig {
    fn default() -> Self {
        Self {
            tick_offsets_ms: vec![5_000, 20_000, 45_000],
            disconnect_at: Vec::new(),
            reject_subscriptions: false,
            frame_delay_ms: 0,
            malformed_at: Vec::new(),
            max_addresses: 100,
            skip: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SkipBar {
    pub mint: String,
    pub unix_time: i64,
}

/// Request evidence for the stream, returned by `GET /__mock/birdeye-ws`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WsEvidence {
    pub connections: u64,
    pub open_connections: u64,
    pub subscribe_messages: u64,
    pub rejected_subscriptions: u64,
    pub price_frames: u64,
    pub price_bytes: u64,
    pub malformed_frames: u64,
    pub forced_disconnects: u64,
    pub last_subscription: Vec<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

pub async fn evidence(State(state): State<Shared>) -> Json<WsEvidence> {
    Json(state.lock().expect("ledger").birdeye_ws.clone())
}

pub async fn upgrade(State(state): State<Shared>, ws: WebSocketUpgrade) -> Response {
    ws.protocols(["echo-protocol"])
        .on_upgrade(move |socket| serve(state, socket))
}

/// Mints named in a `SUBSCRIBE_PRICE` frame, simple or complex query.
pub fn subscribed_mints(request: &Value) -> Option<Vec<String>> {
    if request.get("type").and_then(Value::as_str) != Some("SUBSCRIBE_PRICE") {
        return None;
    }
    let data = request.get("data")?;
    if let Some(address) = data.get("address").and_then(Value::as_str) {
        return Some(vec![address.to_owned()]);
    }
    let query = data.get("query").and_then(Value::as_str)?;
    Some(
        query
            .split("address")
            .skip(1)
            .filter_map(|part| {
                let part = part.trim_start().strip_prefix('=')?.trim_start();
                let end = part
                    .find(|c: char| c.is_whitespace() || c == ')')
                    .unwrap_or(part.len());
                (end > 0).then(|| part[..end].to_owned())
            })
            .collect(),
    )
}

/// Running-bar frames due for `bar` at offsets, as (send_at_ms, frame).
pub fn bar_frames(mint: &str, bar: &Value, offsets: &[u64]) -> Vec<(i64, Value)> {
    let Some(start) = bar.get("unix_time").and_then(Value::as_i64) else {
        return Vec::new();
    };
    let f = |k: &str| bar.get(k).and_then(Value::as_f64);
    let (Some(h), Some(l), Some(c), Some(v)) = (f("h"), f("l"), f("c"), f("v")) else {
        return Vec::new();
    };
    let path = [h, l, c];
    let n = offsets.len().max(1);
    offsets
        .iter()
        .enumerate()
        .map(|(i, offset)| {
            let seen = &path[..(i * path.len() / n + 1).min(path.len())];
            let seen = if i + 1 == n { &path[..] } else { seen };
            let close = *seen.last().expect("non-empty");
            let high = seen.iter().copied().fold(f64::MIN, f64::max);
            let low = seen.iter().copied().fold(f64::MAX, f64::min);
            let volume = v * (i + 1) as f64 / n as f64;
            let frame = json!({"type":"PRICE_DATA","data":{
                "o":h,"h":high,"l":low,"c":close,"v":volume,"vUsd":volume*close,
                "type":"1m","eventType":"ohlcv","unixTime":start,"symbol":"MOCK",
                "address":mint,"isScaled":false,"multiplier":null}});
            (start * 1000 + *offset as i64, frame)
        })
        .collect()
}

async fn serve(state: Shared, mut socket: WebSocket) {
    let connected_ms = now_ms();
    {
        let mut ledger = state.lock().expect("ledger");
        ledger.birdeye_ws.connections += 1;
        ledger.birdeye_ws.open_connections += 1;
    }
    let mut subscribed: Vec<String> = Vec::new();
    // Frames already sent: (mint, bar start, tick index). Only frames due after
    // the subscription are sent; the feed never replays history.
    let mut sent: BTreeSet<(String, i64, usize)> = BTreeSet::new();
    let mut subscribed_at: HashMap<String, i64> = HashMap::new();
    let mut fired_disconnects: BTreeSet<i64> = BTreeSet::new();
    let mut fired_malformed: BTreeSet<i64> = BTreeSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    'outer: loop {
        tokio::select! {
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                let Message::Text(text) = message else { continue };
                let Ok(request) = serde_json::from_str::<Value>(&text) else { continue };
                let Some(mints) = subscribed_mints(&request) else { continue };
                let (reject, limit) = {
                    let mut ledger = state.lock().expect("ledger");
                    ledger.birdeye_ws.subscribe_messages += 1;
                    let cfg = &ledger.scenario.birdeye.ws;
                    (cfg.reject_subscriptions, cfg.max_addresses)
                };
                if reject {
                    state.lock().expect("ledger").birdeye_ws.rejected_subscriptions += 1;
                    let _ = socket.send(Message::Text(json!({"type":"ERROR","data":{"message":"subscription rejected"}}).to_string().into())).await;
                    continue;
                }
                if mints.len() > limit {
                    let _ = socket.send(Message::Text(json!({"type":"ERROR","data":{"message":"too many addresses"}}).to_string().into())).await;
                    break;
                }
                // Each SUBSCRIBE_PRICE replaces the connection's previous set.
                let at = now_ms();
                subscribed_at.retain(|m, _| mints.contains(m));
                for mint in &mints {
                    subscribed_at.entry(mint.clone()).or_insert(at);
                }
                subscribed = mints;
                state.lock().expect("ledger").birdeye_ws.last_subscription = subscribed.clone();
                let _ = socket.send(Message::Text(json!({"type":"SUBSCRIBE_PRICE_DATA","data":{"count":subscribed.len()}}).to_string().into())).await;
            }
            _ = tick.tick() => {
                let now = now_ms();
                let (cfg, tokens) = {
                    let ledger = state.lock().expect("ledger");
                    (ledger.scenario.birdeye.ws.clone(), ledger.scenario.birdeye.tokens.clone())
                };
                for at in &cfg.disconnect_at {
                    let at_ms = (*at * 1000.0) as i64;
                    if at_ms > connected_ms && at_ms <= now && fired_disconnects.insert(at_ms) {
                        state.lock().expect("ledger").birdeye_ws.forced_disconnects += 1;
                        break 'outer;
                    }
                }
                for at in &cfg.malformed_at {
                    let at_ms = (*at * 1000.0) as i64;
                    if at_ms > connected_ms && at_ms <= now && fired_malformed.insert(at_ms) {
                        state.lock().expect("ledger").birdeye_ws.malformed_frames += 1;
                        if socket.send(Message::Text("{\"type\":\"PRICE_DATA\",\"data\":{\"c\":".into())).await.is_err() {
                            break 'outer;
                        }
                    }
                }
                let mut due = Vec::new();
                for mint in &subscribed {
                    let Some(bars) = tokens.get(mint).and_then(|t| t.get("candles")).and_then(Value::as_array) else { continue };
                    let since = subscribed_at.get(mint).copied().unwrap_or(now);
                    for bar in bars {
                        let start = bar.get("unix_time").and_then(Value::as_i64).unwrap_or(0);
                        if start * 1000 > now || (start + 60) * 1000 < since {
                            continue;
                        }
                        if cfg.skip.iter().any(|s| s.mint == *mint && s.unix_time == start) {
                            continue;
                        }
                        for (index, (send_at, frame)) in bar_frames(mint, bar, &cfg.tick_offsets_ms).into_iter().enumerate() {
                            let send_at = send_at + cfg.frame_delay_ms as i64;
                            if send_at <= now && send_at >= since && sent.insert((mint.clone(), start, index)) {
                                due.push((send_at, frame));
                            }
                        }
                    }
                }
                due.sort_by_key(|(at, _)| *at);
                for (_, frame) in due {
                    let wire = frame.to_string();
                    {
                        let mut ledger = state.lock().expect("ledger");
                        ledger.birdeye_ws.price_frames += 1;
                        ledger.birdeye_ws.price_bytes += wire.len() as u64;
                    }
                    if socket.send(Message::Text(wire.into())).await.is_err() {
                        break 'outer;
                    }
                }
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
    let mut ledger = state.lock().expect("ledger");
    ledger.birdeye_ws.open_connections = ledger.birdeye_ws.open_connections.saturating_sub(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complex_and_simple_subscriptions_name_their_mints() {
        let complex = json!({"type":"SUBSCRIBE_PRICE","data":{"queryType":"complex",
            "query":"(address = A1 AND chartType = 1m AND currency = usd) OR (address = B2 AND chartType = 1m AND currency = usd)"}});
        assert_eq!(subscribed_mints(&complex), Some(vec!["A1".into(), "B2".into()]));
        let simple = json!({"type":"SUBSCRIBE_PRICE","data":{"queryType":"simple","address":"C3","chartType":"1m"}});
        assert_eq!(subscribed_mints(&simple), Some(vec!["C3".into()]));
        assert_eq!(subscribed_mints(&json!({"type":"SUBSCRIBE_TXS"})), None);
    }

    #[test]
    fn final_frame_carries_the_rest_bar_and_first_frame_opens_at_first_trade() {
        let bar = json!({"unix_time":600,"o":1.0,"h":1.5,"l":0.8,"c":1.2,"v":30.0});
        let frames = bar_frames("M", &bar, &[5_000, 20_000, 45_000]);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].0, 605_000);
        let first = &frames[0].1["data"];
        assert_eq!((first["o"].as_f64(), first["c"].as_f64()), (Some(1.5), Some(1.5)));
        let last = &frames[2].1["data"];
        assert_eq!(
            (last["o"].as_f64(), last["h"].as_f64(), last["l"].as_f64(), last["c"].as_f64(), last["v"].as_f64()),
            (Some(1.5), Some(1.5), Some(0.8), Some(1.2), Some(30.0))
        );
        assert_eq!(last["unixTime"].as_i64(), Some(600));
    }
}
