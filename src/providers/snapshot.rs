//! Deterministic provider evidence fixtures. No outbound I/O or credentials.
use std::{collections::BTreeMap, sync::atomic::{AtomicU64,Ordering}, time::{Duration, SystemTime, UNIX_EPOCH}};
static REQUEST_SEQUENCE:AtomicU64=AtomicU64::new(1);
use axum::{extract::{Query, State}, http::StatusCode, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use crate::{Shared, scenario::SnapshotFault};

pub fn router() -> Router<Shared> {
    let mut router = Router::new();
    for path in ["/defi/price", "/defi/ohlcv", "/defi/v3/ohlcv", "/defi/token_overview", "/defi/token_security", "/token/v1/holder-profile", "/token/v1/holder/chart", "/defi/v3/token/meme/list", "/defi/token_creation_info"] {
        router = router.route(path, get(birdeye));
    }
    router.route("/telegram/provider", post(telegram))
}
fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs() as i64 }
fn number(q: &BTreeMap<String,String>, key: &str, fallback: i64) -> i64 { q.get(key).and_then(|s|s.parse().ok()).unwrap_or(fallback) }

fn default_body(path: &str, q: &BTreeMap<String,String>) -> Value {
    let mint=q.get("address").map(String::as_str).unwrap_or("fixture-mint");
    let at=now();
    let data=match path {
        "/defi/price" => json!({"value":1.25,"updateUnixTime":at}),
        "/defi/ohlcv" | "/defi/v3/ohlcv" => {
            let from=number(q,"time_from",at/60*60-300);
            let to=number(q,"time_to",at/60*60-1);
            let first=(from+59).div_euclid(60)*60;
            let count=((to-first).div_euclid(60)+1).clamp(0,1000);
            let items:Vec<_>=(0..count).map(|i| {
                let price=1.0+i as f64/1000.0;
                let mut item=json!({"address":mint,"type":"1m","currency":"usd","o":price,"h":price+0.2,"l":price-0.1,"c":price+0.1,"v":10.0});
                item[if path=="/defi/ohlcv" {"unixTime"} else {"unix_time"}]=json!(first+i*60);item
            }).collect();
            json!({"items":items})
        },
        "/defi/token_overview" => json!({"marketCap":10000.0,"liquidity":5000.0,"vBuy5mUSD":10.0,"vSell5mUSD":2.0,"buy5m":8,"sell5m":3,"uniqueWallet5m":4}),
        "/defi/token_security" => json!({"isRugged":false}),
        "/token/v1/holder-profile" => json!({"token":{"top10_holder":{"percent_of_supply":12.5}},"holder_summary":{"total_holder":100},"tags":[]}),
        "/token/v1/holder/chart" => {
            let from=number(q,"time_from",at-900);let to=number(q,"time_to",at);
            let first=(from+59).div_euclid(60)*60;
            let count=((to-first).div_euclid(60)+1).clamp(0,1000);
            json!((0..count).map(|i|json!({"timestamp":first+i*60,"holder":100.0+i as f64})).collect::<Vec<_>>())
        },
        "/defi/v3/token/meme/list" => json!({"items":[{"address":mint,"price":1.0,"liquidity":10000.0,"volume_1h_usd":9000.0,"price_change_5m_percent":90.0,"price_change_1h_percent":3.0}]}),
        "/defi/token_creation_info" => json!({"tokenAddress":mint,"blockUnixTime":at-3600,"owner":null,"txHash":null}),
        _=>Value::Null,
    };
    json!({"success":true,"data":data})
}
async fn respond(state: Shared, path: String, scope: Value, body: Value, faults: Vec<SnapshotFault>) -> Response {
    let sequence=REQUEST_SEQUENCE.fetch_add(1,Ordering::Relaxed);
    let (call, fault, index)={
        let mut ledger=state.lock().expect("ledger");
        let counter=ledger.snapshot_counters.entry(path.clone()).or_default();*counter+=1;let call=*counter;
        let fault=faults.into_iter().find(|f|f.on_call==0 || f.on_call==call);
        let index=ledger.snapshot_requests.len();
        assert!(index<10000,"fixture request log exceeded bounded run");
        ledger.snapshot_requests.push(json!({"request_id":sequence,"path":path,"scope":scope,"call":call,"requested_unix":now(),"fault":fault,"response":null}));
        (call,fault,index)
    };
    let _=call;
    let status=StatusCode::from_u16(fault.as_ref().and_then(|f|f.status).unwrap_or(200)).unwrap_or(StatusCode::BAD_REQUEST);
    let body=fault.as_ref().and_then(|f|f.body.clone()).unwrap_or_else(||if status.is_success(){body}else{json!({"success":false,"message":"injected provider failure"})});
    let malformed=fault.as_ref().is_some_and(|f|f.malformed);
    let wire=if malformed {"{invalid-json".to_owned()}else{body.to_string()};
    if let Some(f)=&fault { tokio::time::sleep(Duration::from_millis(f.delay_ms)).await; }
    let count=body.pointer("/data/items").or_else(||body.get("data")).or_else(||body.get("messages")).and_then(Value::as_array).map(Vec::len);
    {
        let mut ledger=state.lock().expect("ledger");
        if let Some(entry)=ledger.snapshot_requests.get_mut(index) {
            // A timed-out request may finish after the next scenario resets its log.
            // Never corrupt that scenario's receipt at a reused vector index.
            if entry["request_id"]==json!(sequence) {
                entry["response"]=json!({"status":status.as_u16(),"returned_rows":count,"body":if malformed {Value::Null}else{body},"malformed":malformed,"body_sha256":hex_digest(&wire),"responded_unix":now()});
            }
        }
    }
    (status,[("content-type","application/json")],wire).into_response()
}
fn snapshot_faults(faults: Vec<crate::providers::evidence::Fault>) -> Vec<SnapshotFault> {
    faults.into_iter().map(|f| SnapshotFault {
        on_call: f.on_call, status: if f.status == 0 { None } else { Some(f.status) },
        delay_ms: f.delay_ms, malformed: f.malformed, body: f.body,
    }).collect()
}
fn hex_digest(wire: &str) -> String { Sha256::digest(wire.as_bytes()).iter().map(|b|format!("{b:02x}")).collect() }
async fn birdeye(State(state): State<Shared>, uri: axum::http::Uri, Query(q): Query<BTreeMap<String,String>>) -> Response {
    let path=uri.path().to_owned();
    let faults=state.lock().expect("ledger").scenario.birdeye.faults.get(&path).cloned().unwrap_or_default();
    respond(state,path.clone(),json!(q),default_body(&path,&q),snapshot_faults(faults)).await
}
async fn telegram(State(state): State<Shared>, Json(request): Json<Value>) -> Response {
    let scenario=state.lock().expect("ledger").scenario.telegram.clone();
    let op=request["op"].as_str().unwrap_or("invalid");let path=format!("/telegram/{op}");
    let body=match op {
        "head"=>json!({"status":"OK","head_message_id":scenario.head_message_id}),
        "page"=>{
            let after=request["after_message_id"].as_i64().unwrap_or(0);
            let limit=request["limit"].as_u64().unwrap_or(100).min(100) as usize;
            let remaining:Vec<_>=scenario.messages.iter().filter(|m|m["external_message_id"].as_i64().is_some_and(|id|id>after)).cloned().collect();
            json!({"status":"OK","messages":remaining.iter().take(limit).collect::<Vec<_>>(),"channel_exhausted":remaining.len()<=limit,"flood_wait_seconds":null})
        },
        _=>json!({"status":"ERROR","error_class":"INVALID_FIXTURE_OPERATION","messages":[],"channel_exhausted":false}),
    };
    let faults=scenario.faults.get(&path).cloned().unwrap_or_default();
    respond(state,path,request,body,snapshot_faults(faults)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn snapshot_reset_cannot_attach_late_response_to_next_scenario() {
        let state=std::sync::Arc::new(std::sync::Mutex::new(crate::ledger::Ledger::new(crate::scenario::Scenario::default())));
        let old=state.clone();
        let task=tokio::spawn(async move {respond(old,"/defi/price".into(),json!({}),json!({"data":{"value":1}}),vec![SnapshotFault{delay_ms:100,on_call:0,..Default::default()}]).await});
        while state.lock().unwrap().snapshot_requests.is_empty(){tokio::task::yield_now().await;}
        *state.lock().unwrap()=crate::ledger::Ledger::new(crate::scenario::Scenario::default());
        respond(state.clone(),"/defi/price".into(),json!({}),json!({"data":{"value":2}}),vec![]).await;
        task.await.unwrap();
        assert_eq!(state.lock().unwrap().snapshot_requests[0]["response"]["body"]["data"]["value"],2);
    }

    #[test]
    fn snapshot_fixture_preserves_legacy_timestamp_and_exact_scoping() {
        let q=BTreeMap::from([("address".into(),"mint-a".into()),("time_from".into(),"1725000001".into()),("time_to".into(),"1725000299".into())]);
        let v=default_body("/defi/ohlcv",&q);let rows=v["data"]["items"].as_array().unwrap();
        assert_eq!(rows.len(),4);assert_eq!(rows[0]["unixTime"],1725000060);assert_eq!(rows[3]["unixTime"],1725000240);
        assert!(rows.iter().all(|r|r["address"]=="mint-a" && r.get("unix_time").is_none()));
    }
    #[test]
    fn snapshot_meme_does_not_invent_fifteen_minute_provider_fields() {
        let data=default_body("/defi/v3/token/meme/list",&BTreeMap::new());
        assert!(data["data"]["items"][0].get("price_change_15m_percent").is_none());
        assert_eq!(data["data"]["items"][0]["price_change_5m_percent"],90.0);
    }
}
