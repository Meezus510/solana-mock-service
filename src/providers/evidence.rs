//! Fixture-backed external market/social providers. Availability uses wall time.
use crate::Shared;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvidenceConfig {
    pub tokens: HashMap<String, Value>,
    pub responses: HashMap<String, Value>,
    pub faults: HashMap<String, Vec<Fault>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelegramConfig {
    #[serde(default = "default_head_message_id")]
    pub head_message_id: i64,
    pub channels: Vec<Value>,
    pub messages: Vec<Value>,
    pub faults: HashMap<String, Vec<Fault>>,
}
fn default_head_message_id() -> i64 { 100 }
impl Default for TelegramConfig {
    fn default() -> Self {
        Self { head_message_id: 100, channels: Vec::new(), messages: Vec::new(), faults: HashMap::new() }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Fault {
    pub on_call: u64,
    #[serde(deserialize_with = "deserialize_fault_status")]
    pub status: u16,
    pub delay_ms: u64,
    pub body: Option<Value>,
    pub malformed: bool,
}
fn deserialize_fault_status<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    Ok(Option::<u16>::deserialize(deserializer)?.unwrap_or(0))
}
#[derive(Default)]
pub struct EvidenceState {
    pub requests: Vec<Value>,
    pub messages: Vec<Value>,
    pub calls: HashMap<String, u64>,
}

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/defi/price", get(birdeye))
        .route("/defi/ohlcv", get(birdeye))
        .route("/defi/v3/ohlcv", get(birdeye))
        .route("/defi/token_overview", get(birdeye))
        .route("/defi/token_security", get(birdeye))
        .route("/token/v1/holder-profile", get(birdeye))
        .route("/token/v1/holder/chart", get(birdeye))
        .route("/defi/v3/token/meme/list", get(birdeye))
        .route("/defi/token_creation_info", get(birdeye))
        .route("/__mock/requests", get(requests))
        .route("/__mock/telegram/messages", post(publish))
        .route("/__mock/telegram/channels", get(channels))
        .route("/__mock/telegram/{op}", post(telegram))
}
async fn finish(body: Value, fault: Option<Fault>) -> Response {
    if let Some(f) = fault {
        if f.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(f.delay_ms)).await;
        }
        let status = StatusCode::from_u16(if f.status == 0 { 200 } else { f.status })
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        if f.malformed {
            return (status, "{invalid json").into_response();
        }
        return (status, Json(f.body.unwrap_or(body))).into_response();
    }
    Json(body).into_response()
}
fn record(
    state: &mut EvidenceState,
    key: &str,
    query: Value,
    faults: &HashMap<String, Vec<Fault>>,
) -> Option<Fault> {
    let call = state.calls.entry(key.to_owned()).or_default();
    *call += 1;
    let fault = faults
        .get(key)
        .and_then(|fs| fs.iter().find(|f| f.on_call == 0 || f.on_call == *call))
        .cloned();
    state.requests.push(json!({"provider_request":key,"call":*call,"query":query,"observed_at":now(),"fault":fault}));
    fault
}
fn visible(v: &Value) -> bool {
    v.get("visible_at").and_then(Value::as_f64).unwrap_or(0.0) <= now()
}
async fn birdeye(
    State(shared): State<Shared>,
    uri: axum::http::Uri,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let path = uri.path();
    let (body, fault) = {
        let mut ledger = shared.lock().unwrap();
        let cfg = ledger.scenario.birdeye.clone();
        let mint = q
            .get("address")
            .or_else(|| q.get("token_address"))
            .cloned()
            .unwrap_or_default();
        let scoped = format!("{path}:{mint}");
        let key = if cfg.faults.contains_key(&scoped) {
            scoped.as_str()
        } else {
            path
        };
        let fault = record(&mut ledger.evidence, key, json!(q), &cfg.faults);
        let token = cfg.tokens.get(&mint).cloned().unwrap_or(json!({}));
        let t = now() as i64;
        let price = token
            .get("prices")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter().filter(|v| visible(v)).max_by(|a, b| {
                    a["visible_at"]
                        .as_f64()
                        .unwrap_or(0.0)
                        .total_cmp(&b["visible_at"].as_f64().unwrap_or(0.0))
                })
            })
            .map(|v| v["value"].clone())
            .unwrap_or_else(|| token.get("price").cloned().unwrap_or(Value::Null));
        let data = match path {
            "/defi/price" => {
                json!({"value":price,"updateUnixTime":token.get("update_unix_time").and_then(Value::as_i64).unwrap_or(t),"isScaledUiToken":false})
            }
            "/defi/ohlcv" | "/defi/v3/ohlcv" => {
                let from = q
                    .get("time_from")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(t - 300);
                let to = q
                    .get("time_to")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(t);
                let bars = token
                    .get("candles")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter(|v| {
                                visible(v)
                                    && v["unix_time"]
                                        .as_i64()
                                        .is_some_and(|s| s >= from && s <= to && s + 60 <= t)
                            })
                            .map(|v| {
                                let mut bar = v.clone();
                                bar["address"] = json!(mint);
                                bar["type"] = json!("1m");
                                bar["currency"] = json!("usd");
                                if let Some(o) = bar.as_object_mut() {
                                    o.remove("visible_at");
                                }
                                bar
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                json!({"is_scaled_ui_token":false,"items":bars})
            }
            "/defi/token_overview" => token.get("overview").cloned().unwrap_or(json!({})),
            "/defi/token_security" => token.get("security").cloned().unwrap_or(json!({})),
            "/token/v1/holder-profile" => token.get("holder_profile").cloned().unwrap_or(json!({})),
            "/token/v1/holder/chart" => {
                let from = q
                    .get("time_from")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(i64::MIN);
                let to = q
                    .get("time_to")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(t);
                let count = q
                    .get("count")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(100)
                    .min(100);
                json!(
                    token
                        .get("holder_chart")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|v| visible(v)
                            && v["timestamp"]
                                .as_i64()
                                .is_some_and(|s| s >= from && s <= to))
                        .take(count)
                        .cloned()
                        .collect::<Vec<_>>()
                )
            }
            "/defi/token_creation_info" => token.get("creation").cloned().unwrap_or(json!({})),
            _ => {
                let min_liquidity = q
                    .get("min_liquidity")
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let min_volume = q
                    .get("min_volume_1h_usd")
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let mut items = cfg
                    .tokens
                    .iter()
                    .filter_map(|(mint, v)| {
                        v.get("meme").map(|x| {
                            let mut x = x.clone();
                            x["address"] = json!(mint);
                            x
                        })
                    })
                    .filter(|v| {
                        visible(v)
                            && v["liquidity"].as_f64().unwrap_or(0.0) >= min_liquidity
                            && v["volume_1h_usd"].as_f64().unwrap_or(0.0) >= min_volume
                    })
                    .collect::<Vec<_>>();
                let field = q
                    .get("sort_by")
                    .map(String::as_str)
                    .unwrap_or("volume_1h_usd");
                items.sort_by(|a, b| {
                    a[field]
                        .as_f64()
                        .unwrap_or(0.0)
                        .total_cmp(&b[field].as_f64().unwrap_or(0.0))
                        .then_with(|| a["address"].as_str().cmp(&b["address"].as_str()))
                });
                if q.get("sort_type").map(String::as_str) != Some("asc") {
                    items.reverse();
                }
                items.truncate(
                    q.get("limit")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(100)
                        .min(100),
                );
                json!({"items":items})
            }
        };
        (
            cfg.responses.get(path).cloned().unwrap_or_else(|| {
                if path != "/defi/v3/token/meme/list" && !cfg.tokens.contains_key(&mint) {
                    json!({"success":false,"data":null,"message":"unconfigured fixture mint"})
                } else {
                    json!({"success":true,"data":data})
                }
            }),
            fault,
        )
    };
    finish(body, fault).await
}
async fn requests(State(s): State<Shared>) -> Json<Value> {
    Json(json!(s.lock().unwrap().evidence.requests))
}
async fn channels(State(s): State<Shared>) -> Json<Value> {
    Json(json!(s.lock().unwrap().scenario.telegram.channels))
}
async fn publish(State(s): State<Shared>, Json(mut message): Json<Value>) -> Response {
    if message.get("channel").is_none() || message.get("id").and_then(Value::as_i64).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"channel and integer id required"})),
        )
            .into_response();
    }
    let mut ledger = s.lock().unwrap();
    if let Some(existing) = ledger
        .evidence
        .messages
        .iter()
        .chain(ledger.scenario.telegram.messages.iter())
        .find(|v| v["channel"] == message["channel"] && v["id"] == message["id"])
    {
        if message.get("visible_at").is_none() {
            message["visible_at"] = existing["visible_at"].clone();
        }
        return if existing == &message {
            Json(json!({"status":"ok","disposition":"IDEMPOTENT"})).into_response()
        } else {
            (
                StatusCode::CONFLICT,
                Json(json!({"error":"message identity conflict"})),
            )
                .into_response()
        };
    }
    if message.get("visible_at").is_none() {
        message["visible_at"] = json!(now());
    }
    ledger.evidence.messages.push(message);
    Json(json!({"status":"ok"})).into_response()
}
async fn telegram(
    State(s): State<Shared>,
    Path(op): Path<String>,
    Json(q): Json<Value>,
) -> Response {
    if !matches!(op.as_str(), "head" | "page" | "updates") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"unknown Telegram operation"})),
        )
            .into_response();
    }
    let (body, fault) = {
        let mut ledger = s.lock().unwrap();
        let cfg = ledger.scenario.telegram.clone();
        let key = q["consumer"]
            .as_str()
            .map(|c| format!("{op}:{c}"))
            .unwrap_or_else(|| op.clone());
        let fault = record(&mut ledger.evidence, &key, q.clone(), &cfg.faults);
        let channel = q["channel"].as_str().unwrap_or("");
        let after = q["after_message_id"].as_i64().unwrap_or(0);
        let max = q["max_id"].as_i64().unwrap_or(i64::MAX);
        let limit = q["limit"].as_u64().unwrap_or(100) as usize;
        let mut rows = cfg
            .messages
            .iter()
            .chain(ledger.evidence.messages.iter())
            .filter(|v| v["channel"] == channel && visible(v))
            .cloned()
            .collect::<Vec<_>>();
        rows.sort_by_key(|v| v["id"].as_i64().unwrap_or(0));
        rows.dedup_by_key(|v| v["id"].as_i64().unwrap_or(0));
        let head = rows.last().cloned();
        let body = if op == "head" {
            json!({"status":"OK","head_message_id":head.as_ref().map(|v|v["id"].clone()).unwrap_or(json!(0)),"head_event_ts":head.map(|v|v["event_ts"].clone())})
        } else {
            rows.retain(|v| v["id"].as_i64().is_some_and(|id| id > after && id < max));
            let exhausted = rows.len() < limit;
            rows.truncate(limit);
            json!({"status":"OK","messages":rows,"channel_exhausted":exhausted,"flood_wait_seconds":null})
        };
        (body, fault)
    };
    finish(body, fault).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    fn app(config: crate::scenario::Scenario) -> Router {
        router().with_state(Arc::new(Mutex::new(crate::ledger::Ledger::new(config))))
    }
    async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    #[tokio::test]
    async fn meme_sample_respects_filters_sorting_and_limit() {
        let mut cfg = crate::scenario::Scenario::default();
        for (mint, liquidity, volume) in [("a", 3000, 6000), ("b", 3000, 9000), ("c", 1, 12000)] {
            cfg.birdeye.tokens.insert(
                mint.into(),
                json!({"meme":{"liquidity":liquidity,"volume_1h_usd":volume}}),
            );
        }
        let a = app(cfg);
        let (_,body)=call(&a,"GET","/defi/v3/token/meme/list?min_liquidity=2000&min_volume_1h_usd=5000&sort_type=desc&limit=1",Value::Null).await;
        assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"]["items"][0]["address"], "b");
        let (_, creation) = call(
            &a,
            "GET",
            "/defi/token_creation_info?address=a",
            Value::Null,
        )
        .await;
        assert_eq!(creation["data"], json!({}));
    }
    #[tokio::test]
    async fn unknown_mints_fail_closed_and_fractional_timelines_sort() {
        let mut cfg = crate::scenario::Scenario::default();
        cfg.birdeye.tokens.insert(
            "m".into(),
            json!({"prices":[
            {"visible_at":1.9,"value":9},{"visible_at":1.1,"value":1}]}),
        );
        let a = app(cfg);
        let (_, missing) = call(&a, "GET", "/defi/price?address=unknown", Value::Null).await;
        assert_eq!(missing["success"], false);
        let (_, price) = call(&a, "GET", "/defi/price?address=m", Value::Null).await;
        assert_eq!(price["data"]["value"], 9);
    }
    #[tokio::test]
    async fn omitted_visibility_retry_is_idempotent_and_invalid_op_fails() {
        let a = app(Default::default());
        let message =
            json!({"channel":"c","id":1,"event_ts":"2026-10-03T00:00:00Z","text":"hello"});
        assert_eq!(
            call(&a, "POST", "/__mock/telegram/messages", message.clone())
                .await
                .0,
            StatusCode::OK
        );
        let (status, body) = call(&a, "POST", "/__mock/telegram/messages", message).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["disposition"], "IDEMPOTENT");
        assert_eq!(
            call(&a, "POST", "/__mock/telegram/invalid", json!({}))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    #[tokio::test]
    async fn completed_window_and_availability_are_enforced() {
        let t = now() as i64;
        let start = t / 60 * 60 - 60;
        let mut cfg = crate::scenario::Scenario::default();
        cfg.birdeye.tokens.insert(
            "mint".into(),
            json!({"candles":[
            {"unix_time":start,"o":1,"h":2,"l":1,"c":2,"v":5},
            {"unix_time":start-60,"visible_at":now()+100.0,"o":1,"h":2,"l":1,"c":2,"v":5},
            {"unix_time":start+60,"o":1,"h":2,"l":1,"c":2,"v":5}]}),
        );
        let a = app(cfg);
        for path in ["/defi/ohlcv", "/defi/v3/ohlcv"] {
            let (_, v) = call(
                &a,
                "GET",
                &format!(
                    "{path}?address=mint&time_from={}&time_to={}",
                    start - 60,
                    start + 60
                ),
                Value::Null,
            )
            .await;
            assert_eq!(v["data"]["items"].as_array().unwrap().len(), 1);
            assert_eq!(v["data"]["items"][0]["address"], "mint");
        }
    }
    #[tokio::test]
    async fn one_shot_fault_then_success_is_logged() {
        let mut cfg = crate::scenario::Scenario::default();
        cfg.birdeye.faults.insert(
            "/defi/price".into(),
            vec![Fault {
                on_call: 1,
                status: 429,
                ..Default::default()
            }],
        );
        let a = app(cfg);
        assert_eq!(
            call(&a, "GET", "/defi/price?address=m", Value::Null)
                .await
                .0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            call(&a, "GET", "/defi/price?address=m", Value::Null)
                .await
                .0,
            StatusCode::OK
        );
        let (_, logs) = call(&a, "GET", "/__mock/requests", Value::Null).await;
        assert_eq!(logs.as_array().unwrap().len(), 2);
        assert!(!logs[0]["fault"].is_null());
        assert!(logs[1]["fault"].is_null());
    }
    #[tokio::test]
    async fn telegram_history_is_ordered_and_identity_conflicts_fail() {
        let a = app(Default::default());
        for id in [3, 1, 2] {
            assert_eq!(
                call(
                    &a,
                    "POST",
                    "/__mock/telegram/messages",
                    json!({"channel":"test","id":id,"visible_at":0,"text":"x"})
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        let (_, page) = call(
            &a,
            "POST",
            "/__mock/telegram/page",
            json!({"channel":"test","after_message_id":1,"limit":1}),
        )
        .await;
        assert_eq!(page["messages"][0]["id"], 2);
        assert_eq!(page["channel_exhausted"], false);
        assert_eq!(
            call(
                &a,
                "POST",
                "/__mock/telegram/messages",
                json!({"channel":"test","id":2,"visible_at":0,"text":"conflict"})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
}
