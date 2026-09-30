//! Jupiter Swap V2 (`/build`, `/order`), Price V3 and Trigger V2 under
//! `/jupiter`. Transactions are built for the mock ledger's swap and vault
//! programs, in the same JSON/bincode shapes Jupiter returns.

use std::{
    collections::HashMap,
    str::FromStr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use solana_instruction::{AccountMeta, Instruction};
use solana_message::{VersionedMessage, v0};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::{
    Shared,
    ledger::{
        JITO_TIP_ACCOUNTS, Ledger, Order, SOL_MINT, SWAP_TAG, TRIGGER_DEPOSIT, TRIGGER_TAG,
        TRIGGER_WITHDRAW, compute_budget_program, derived, dust_mint, route_fee_account, sol_mint,
        swap_program, token_account, trigger_program, vault_of,
    },
};

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/jupiter/swap/v2/build", get(build))
        .route("/jupiter/swap/v2/order", get(order))
        .route("/jupiter/price/v3", get(price))
        .route("/jupiter/trigger/v2/auth/challenge", post(challenge))
        .route("/jupiter/trigger/v2/auth/verify", post(verify))
        .route("/jupiter/trigger/v2/vault", get(vault))
        .route("/jupiter/trigger/v2/vault/register", get(vault))
        .route("/jupiter/trigger/v2/deposit/craft", post(craft_deposit))
        .route("/jupiter/trigger/v2/orders/price", post(create_order))
        .route("/jupiter/trigger/v2/orders/history", get(history))
        .route(
            "/jupiter/trigger/v2/orders/price/cancel/{id}",
            post(craft_cancel),
        )
        .route(
            "/jupiter/trigger/v2/orders/price/confirm-cancel/{id}",
            post(confirm_cancel),
        )
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

/// 429 (with a reset header) or 503 per the scenario.
fn jupiter_faults(ledger: &mut Ledger) -> Option<Response> {
    let faults = ledger.scenario.jupiter.clone();
    if ledger.chance(faults.rate_limit_rate) {
        ledger.count("jupiter_429");
        let reset = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 1;
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", HeaderValue::from(reset));
        headers.insert("x-ratelimit-remaining", HeaderValue::from(0));
        return Some((StatusCode::TOO_MANY_REQUESTS, headers, "rate limited").into_response());
    }
    if ledger.chance(faults.server_error_rate) {
        ledger.count("jupiter_503");
        return Some(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream unavailable",
        ));
    }
    None
}

struct Quote {
    buy: bool,
    mint: Pubkey,
    wallet: Pubkey,
    amount: u64,
    out_amount: u64,
    threshold: u64,
    slippage_bps: u64,
}

#[allow(clippy::result_large_err)] // Axum responses are returned directly.
fn quote(ledger: &mut Ledger, params: &HashMap<String, String>) -> Result<Quote, Response> {
    let get = |name: &str| params.get(name).cloned().unwrap_or_default();
    let input = Pubkey::from_str(&get("inputMint"))
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid inputMint"))?;
    let output = Pubkey::from_str(&get("outputMint"))
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid outputMint"))?;
    let wallet = Pubkey::from_str(&get("taker"))
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid taker"))?;
    let amount: u64 = get("amount")
        .parse()
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid amount"))?;
    let slippage_bps: u64 = get("slippageBps").parse().unwrap_or(50);
    let buy = input == sol_mint();
    if !buy && output != sol_mint() {
        return Err(fail(StatusCode::BAD_REQUEST, "only SOL pairs are mocked"));
    }
    let mint = if buy { output } else { input };
    let behavior = ledger.behavior(&mint);
    if behavior.no_route || (!buy && behavior.sell_no_route) {
        ledger.count("no_route");
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "Could not find any route", "errorCode": "COULD_NOT_FIND_ANY_ROUTE"}))).into_response());
    }
    ledger.decimals.entry(mint).or_insert(behavior.decimals);
    ledger.ensure_wallet(&wallet);
    let out_amount = if buy {
        (amount as f64 * behavior.tokens_per_lamport) as u64
    } else {
        (amount as f64 / behavior.tokens_per_lamport) as u64
    };
    let threshold = out_amount * (10_000 - slippage_bps.min(10_000)) / 10_000;
    Ok(Quote {
        buy,
        mint,
        wallet,
        amount,
        out_amount,
        threshold,
        slippage_bps,
    })
}

fn swap_instructions(
    ledger: &mut Ledger,
    quote: &Quote,
    oversize: bool,
) -> (Vec<Instruction>, Instruction) {
    let behavior = ledger.behavior(&quote.mint);
    let mut setup = Vec::new();
    let wsol = token_account(&quote.wallet, &sol_mint());
    if quote.buy {
        setup.push(solana_system_interface::instruction::transfer(
            &quote.wallet,
            &wsol,
            quote.amount,
        ));
    }
    let mut accounts = vec![
        AccountMeta::new(quote.wallet, true),
        AccountMeta::new(wsol, false),
        AccountMeta::new(token_account(&quote.wallet, &quote.mint), false),
    ];
    if behavior.dust_second_mint {
        accounts.push(AccountMeta::new(
            token_account(&quote.wallet, &dust_mint()),
            false,
        ));
    }
    if behavior.auxiliary_debit_lamports > 0 {
        accounts.push(AccountMeta::new(route_fee_account(), false));
    }
    if oversize {
        // A multi-hop route whose accounts only fit through lookup tables,
        // returned here without them.
        for index in 0..70_u32 {
            accounts.push(AccountMeta::new_readonly(
                derived("route-account", &[&index.to_le_bytes()]),
                false,
            ));
        }
    }
    let mut data = vec![SWAP_TAG, u8::from(!quote.buy)];
    data.extend_from_slice(quote.mint.as_ref());
    data.extend_from_slice(&quote.amount.to_le_bytes());
    data.extend_from_slice(&quote.threshold.to_le_bytes());
    data.extend_from_slice(&ledger.next_nonce().to_le_bytes());
    (
        setup,
        Instruction {
            program_id: swap_program(),
            accounts,
            data,
        },
    )
}

fn instruction_json(instruction: &Instruction) -> Value {
    json!({
        "programId": instruction.program_id.to_string(),
        "accounts": instruction.accounts.iter().map(|meta| json!({
            "pubkey": meta.pubkey.to_string(), "isSigner": meta.is_signer, "isWritable": meta.is_writable,
        })).collect::<Vec<_>>(),
        "data": STANDARD.encode(&instruction.data),
    })
}

async fn build(
    State(state): State<Shared>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let mut ledger = state.lock().expect("ledger");
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let quote = match quote(&mut ledger, &params) {
        Ok(quote) => quote,
        Err(response) => return response,
    };
    let behavior = ledger.behavior(&quote.mint);
    if behavior.fast_build_fails {
        ledger.count("fast_build_failed");
        return fail(
            StatusCode::BAD_REQUEST,
            "fast mode unavailable for this route",
        );
    }
    let (setup, swap) = swap_instructions(&mut ledger, &quote, behavior.fast_build_oversize);
    let (hash, last_valid) = ledger.issue_blockhash();
    Json(json!({
        "inputMint": params.get("inputMint"),
        "outputMint": params.get("outputMint"),
        "inAmount": quote.amount.to_string(),
        "outAmount": quote.out_amount.to_string(),
        "otherAmountThreshold": quote.threshold.to_string(),
        "slippageBps": quote.slippage_bps,
        "setupInstructions": setup.iter().map(instruction_json).collect::<Vec<_>>(),
        "swapInstruction": instruction_json(&swap),
        "cleanupInstruction": null,
        "otherInstructions": [],
        "addressesByLookupTableAddress": {},
        "blockhashWithMetadata": {"blockhash": hash.to_string(), "lastValidBlockHeight": last_valid},
    }))
    .into_response()
}

async fn order(
    State(state): State<Shared>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let mut ledger = state.lock().expect("ledger");
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let quote = match quote(&mut ledger, &params) {
        Ok(quote) => quote,
        Err(response) => return response,
    };
    let priority_fee: u64 = params
        .get("priorityFeeLamports")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let tip: u64 = params
        .get("jitoTipLamports")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let limit = 400_000_u32;
    let mut instructions = vec![
        solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_limit(limit),
        solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_price(
            priority_fee.saturating_mul(1_000_000) / u64::from(limit),
        ),
    ];
    let (setup, swap) = swap_instructions(&mut ledger, &quote, false);
    instructions.extend(setup);
    instructions.push(swap);
    if tip > 0 {
        let tip_account = Pubkey::from_str(JITO_TIP_ACCOUNTS[0]).expect("tip account");
        instructions.push(solana_system_interface::instruction::transfer(
            &quote.wallet,
            &tip_account,
            tip,
        ));
    }
    let (hash, last_valid) = ledger.issue_blockhash();
    let tx = unsigned(&quote.wallet, &instructions, hash);
    Json(json!({
        "transaction": STANDARD.encode(bincode::serialize(&tx).expect("serialize")),
        "requestId": format!("order-{}", ledger.next_nonce()),
        "inAmount": quote.amount.to_string(),
        "outAmount": quote.out_amount.to_string(),
        "otherAmountThreshold": quote.threshold.to_string(),
        "lastValidBlockHeight": last_valid,
    }))
    .into_response()
}

fn unsigned(
    payer: &Pubkey,
    instructions: &[Instruction],
    hash: solana_hash::Hash,
) -> VersionedTransaction {
    let message = v0::Message::try_compile(payer, instructions, &[], hash).expect("compile");
    VersionedTransaction {
        signatures: vec![Signature::default(); usize::from(message.header.num_required_signatures)],
        message: VersionedMessage::V0(message),
    }
}

async fn price(
    State(state): State<Shared>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let mut ledger = state.lock().expect("ledger");
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let mut prices = serde_json::Map::new();
    for id in params
        .get("ids")
        .map(String::as_str)
        .unwrap_or_default()
        .split(',')
    {
        if id == SOL_MINT {
            prices.insert(
                id.to_owned(),
                json!({"usdPrice": ledger.scenario.chain.sol_usd}),
            );
        } else if let Ok(mint) = Pubkey::from_str(id) {
            let behavior = ledger.behavior(&mint);
            if behavior.price_supported {
                prices.insert(
                    id.to_owned(),
                    json!({"usdPrice": behavior.usd_price, "decimals": behavior.decimals}),
                );
            }
        }
    }
    Json(Value::Object(prices)).into_response()
}

async fn challenge() -> Json<Value> {
    Json(json!({"challenge": "mock-trigger-challenge"}))
}

async fn verify() -> Json<Value> {
    Json(json!({"token": "mock-trigger-jwt"}))
}

async fn vault() -> Json<Value> {
    Json(json!({"vaultPubkey": derived("trigger-vault-registry", &[]).to_string()}))
}

/// A Trigger vault transaction for the mock ledger.
#[allow(clippy::too_many_arguments)]
pub fn trigger_transaction(
    ledger: &mut Ledger,
    payer: &Pubkey,
    user: &Pubkey,
    op: u8,
    nonce: u64,
    mint: &Pubkey,
    amount: u64,
    proceeds: u64,
    cu_price: u64,
) -> VersionedTransaction {
    let mut data = vec![TRIGGER_TAG, op];
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(mint.as_ref());
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&proceeds.to_le_bytes());
    let vault = vault_of(user);
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(*user, false),
        AccountMeta::new(token_account(user, mint), false),
        AccountMeta::new(token_account(&vault, mint), false),
    ];
    // Keeper-executed transactions (fills, expiry returns) are co-signed by
    // the vault key that owns the order's tokens, as on mainnet.
    if payer != user {
        accounts.push(AccountMeta::new_readonly(vault, true));
    }
    let instruction = Instruction {
        program_id: trigger_program(),
        accounts,
        data,
    };
    let mut instructions = vec![
        solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_limit(90_000),
    ];
    if cu_price > 0 {
        instructions.push(
            solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_price(
                cu_price,
            ),
        );
    }
    instructions.push(instruction);
    let _ = compute_budget_program();
    let (hash, _) = ledger.issue_blockhash();
    unsigned(payer, &instructions, hash)
}

/// Deposits crafted but not yet submitted: request id -> (nonce, mint, amount).
fn pending_key(request_id: &str) -> u64 {
    request_id
        .trim_start_matches("deposit-")
        .parse()
        .unwrap_or(0)
}

async fn craft_deposit(State(state): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut ledger = state.lock().expect("ledger");
    ledger.settle();
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let (Some(user), Some(mint), Some(amount)) = (
        body["userAddress"]
            .as_str()
            .and_then(|v| Pubkey::from_str(v).ok()),
        body["inputMint"]
            .as_str()
            .and_then(|v| Pubkey::from_str(v).ok()),
        body["amount"].as_str().and_then(|v| v.parse::<u64>().ok()),
    ) else {
        return fail(StatusCode::BAD_REQUEST, "invalid deposit request");
    };
    if ledger.token_balance(&user, &mint) < amount {
        return fail(StatusCode::BAD_REQUEST, "insufficient token balance");
    }
    let crafted = ledger.deposits_crafted();
    let trigger = ledger.scenario.trigger.clone();
    let cu_price = if trigger.spike_first_n == 0 || crafted <= trigger.spike_first_n {
        trigger.deposit_cu_price
    } else {
        100_000
    };
    let nonce = ledger.next_nonce();
    let tx = trigger_transaction(
        &mut ledger,
        &user,
        &user,
        TRIGGER_DEPOSIT,
        nonce,
        &mint,
        amount,
        0,
        cu_price,
    );
    Json(json!({
        "requestId": format!("deposit-{nonce}"),
        "transaction": STANDARD.encode(bincode::serialize(&tx).expect("serialize")),
    }))
    .into_response()
}

fn decode_signed(value: &Value) -> Option<VersionedTransaction> {
    let bytes = STANDARD.decode(value.as_str()?).ok()?;
    bincode::deserialize(&bytes).ok()
}

/// Submit and land a provider transaction immediately; `Err` if it failed.
fn land_now(
    ledger: &mut Ledger,
    tx: VersionedTransaction,
    kind: &'static str,
) -> Result<Signature, String> {
    let signature = ledger
        .submit(tx, kind, Duration::ZERO)
        .map_err(|_| "submission rejected".to_owned())?;
    ledger.settle();
    match ledger
        .txs
        .get(&signature)
        .and_then(|record| record.err.clone())
    {
        Some(error) => Err(error.to_string()),
        None => Ok(signature),
    }
}

async fn create_order(State(state): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut ledger = state.lock().expect("ledger");
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let Some(tx) = decode_signed(&body["depositSignedTx"]) else {
        return fail(StatusCode::BAD_REQUEST, "invalid depositSignedTx");
    };
    let trigger = ledger.scenario.trigger.clone();
    if ledger.chance(trigger.submit_error_not_landed_rate) {
        ledger.count("trigger_create_502_not_landed");
        return fail(StatusCode::BAD_GATEWAY, "upstream error");
    }
    let nonce = pending_key(body["depositRequestId"].as_str().unwrap_or_default());
    let (Some(user), Some(input), Some(output), Some(amount)) = (
        body["userPubkey"]
            .as_str()
            .and_then(|v| Pubkey::from_str(v).ok()),
        body["inputMint"]
            .as_str()
            .and_then(|v| Pubkey::from_str(v).ok()),
        body["outputMint"]
            .as_str()
            .and_then(|v| Pubkey::from_str(v).ok()),
        body["inputAmount"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok()),
    ) else {
        return fail(StatusCode::BAD_REQUEST, "invalid order request");
    };
    let signature = match land_now(&mut ledger, tx, "trigger_deposit") {
        Ok(signature) => signature,
        Err(error) => return fail(StatusCode::BAD_REQUEST, &format!("deposit failed: {error}")),
    };
    let id = format!("mock-oco-{nonce}");
    let now = Instant::now();
    ledger.orders.insert(
        id.clone(),
        Order {
            id: id.clone(),
            user,
            input_mint: input,
            output_mint: output,
            amount,
            state: "open",
            deposit_signature: signature,
            fill_signature: None,
            fill_at: (trigger.fill_after_ms > 0)
                .then(|| now + Duration::from_millis(trigger.fill_after_ms)),
            expire_at: (trigger.expire_after_ms > 0)
                .then(|| now + Duration::from_millis(trigger.expire_after_ms)),
        },
    );
    ledger.count("trigger_orders_created");
    if ledger.chance(trigger.submit_error_after_landing_rate) {
        ledger.count("trigger_create_504_after_landing");
        return fail(StatusCode::GATEWAY_TIMEOUT, "upstream timeout");
    }
    Json(json!({"id": id, "txSignature": signature.to_string(), "depositConfirmed": true}))
        .into_response()
}

async fn history(State(state): State<Shared>) -> Response {
    let mut ledger = state.lock().expect("ledger");
    ledger.settle();
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let lag = Duration::from_millis(ledger.scenario.trigger.history_lag_ms);
    let now = Instant::now();
    let orders: Vec<Value> = ledger
        .orders
        .values()
        .map(|order| {
            let lagging =
                order.state == "expired" && order.expire_at.is_some_and(|at| now < at + lag);
            let mut events = vec![
                json!({"type": "deposit", "txSignature": order.deposit_signature.to_string()}),
            ];
            if let Some(fill) = order.fill_signature {
                events.push(json!({"type": "fill", "txSignature": fill.to_string()}));
            }
            json!({
                "id": order.id,
                "orderType": "oco",
                "userPubkey": order.user.to_string(),
                "inputMint": order.input_mint.to_string(),
                "outputMint": order.output_mint.to_string(),
                "initialInputAmount": order.amount.to_string(),
                "orderState": if lagging { "open" } else { order.state },
                "txSignature": order.fill_signature.map(|s| s.to_string()),
                "events": events,
            })
        })
        .collect();
    Json(json!({"orders": orders})).into_response()
}

async fn craft_cancel(State(state): State<Shared>, Path(id): Path<String>) -> Response {
    let mut ledger = state.lock().expect("ledger");
    ledger.settle();
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let Some(order) = ledger.orders.get(&id).cloned() else {
        return fail(StatusCode::NOT_FOUND, "order not found");
    };
    if order.state != "open" {
        return fail(
            StatusCode::BAD_REQUEST,
            &format!("order is {}", order.state),
        );
    }
    let cu_price = ledger.scenario.trigger.cancel_cu_price;
    let nonce = crate::ledger::order_nonce(&id);
    let tx = trigger_transaction(
        &mut ledger,
        &order.user,
        &order.user,
        TRIGGER_WITHDRAW,
        nonce,
        &order.input_mint,
        order.amount,
        0,
        cu_price,
    );
    Json(json!({
        "requestId": format!("cancel-{}", ledger.next_nonce()),
        "transaction": STANDARD.encode(bincode::serialize(&tx).expect("serialize")),
    }))
    .into_response()
}

async fn confirm_cancel(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut ledger = state.lock().expect("ledger");
    ledger.settle();
    if let Some(response) = jupiter_faults(&mut ledger) {
        return response;
    }
    let Some(tx) = decode_signed(&body["signedTransaction"]) else {
        return fail(StatusCode::BAD_REQUEST, "invalid signedTransaction");
    };
    let trigger = ledger.scenario.trigger.clone();
    if ledger.chance(trigger.submit_error_not_landed_rate) {
        ledger.count("trigger_cancel_502_not_landed");
        return fail(StatusCode::BAD_GATEWAY, "upstream error");
    }
    if ledger.orders.get(&id).map(|order| order.state) != Some("open") {
        return fail(StatusCode::BAD_REQUEST, "order is not open");
    }
    let signature = match land_now(&mut ledger, tx, "trigger_cancel") {
        Ok(signature) => signature,
        Err(error) => {
            return fail(
                StatusCode::BAD_REQUEST,
                &format!("withdrawal failed: {error}"),
            );
        }
    };
    if let Some(order) = ledger.orders.get_mut(&id) {
        order.state = "cancelled";
    }
    ledger.count("trigger_orders_cancelled");
    if ledger.chance(trigger.submit_error_after_landing_rate) {
        ledger.count("trigger_cancel_504_after_landing");
        return fail(StatusCode::GATEWAY_TIMEOUT, "upstream timeout");
    }
    Json(json!({"txSignature": signature.to_string()})).into_response()
}
