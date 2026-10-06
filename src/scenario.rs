//! Fault and behavior configuration, set at runtime via `POST /__mock/scenario`.
//! Every knob maps to a real-world provider failure mode or a deliberate
//! edge case; the scenario suite that
//! drives them is `transaction-service/scripts/mock_scenarios.py`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scenario {
    pub birdeye: crate::providers::evidence::EvidenceConfig,
    pub telegram: crate::providers::evidence::TelegramConfig,
    /// Behavior for any mint without an entry in `tokens`.
    pub default_token: TokenBehavior,
    /// Per-mint behavior, keyed by base58 mint.
    pub tokens: HashMap<String, TokenBehavior>,
    pub chain: ChainBehavior,
    pub rpc: RpcFaults,
    pub websocket: WebSocketFaults,
    pub jupiter: JupiterFaults,
    pub trigger: TriggerBehavior,
    pub jito: JitoFaults,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebSocketFaults {
    pub reject_subscriptions: bool,
    pub drop_notifications: bool,
    pub disconnect_after_ms: u64,
    pub notification_delay_ms: u64,
    pub duplicate_notifications: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TokenBehavior {
    pub decimals: u8,
    /// Token base units received per lamport on a buy (sell is the inverse).
    pub tokens_per_lamport: f64,
    /// Price move applied per executed swap, in basis points (negative =
    /// execution worse than quote).
    pub execution_drift_bps: i64,
    /// Forced on-chain failure for swaps: `slippage` (Custom 6001),
    /// `route_6024`, `route_6036`, `program_failed_to_complete`,
    /// `missing_account`, `insufficient_funds_for_rent`.
    pub onchain_error: Option<String>,
    /// Apply `onchain_error` only to buys / sells.
    pub error_on: ErrorSide,
    /// Fail only the first N matching swaps, then succeed (0 = always).
    pub error_first_n: u32,
    /// Swap succeeds but delivers zero output (honeypot / 100% tax).
    pub zero_fill: bool,
    /// Swap also leaves dust of a second token in the wallet.
    pub dust_second_mint: bool,
    /// Extra non-refundable lamports the route debits on a buy, beyond the
    /// swap input (a route that costs more SOL than quoted).
    pub auxiliary_debit_lamports: u64,
    /// Both Jupiter build endpoints return "no route".
    pub no_route: bool,
    /// Only sells have no route (a dead token that cannot be exited).
    pub sell_no_route: bool,
    /// Fast build fails (forces the /order fallback).
    pub fast_build_fails: bool,
    /// Fast build returns a route too large to fit a transaction without
    /// lookup tables (the transaction exceeds Solana's packet size).
    pub fast_build_oversize: bool,
    /// Jupiter Price has a USD price (required for hosted OCO).
    pub price_supported: bool,
    pub usd_price: f64,
}

impl Default for TokenBehavior {
    fn default() -> Self {
        Self {
            decimals: 6,
            tokens_per_lamport: 0.3,
            execution_drift_bps: -10,
            onchain_error: None,
            error_on: ErrorSide::Both,
            error_first_n: 0,
            zero_fill: false,
            dust_second_mint: false,
            auxiliary_debit_lamports: 0,
            no_route: false,
            sell_no_route: false,
            fast_build_fails: false,
            fast_build_oversize: false,
            price_supported: true,
            usd_price: 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSide {
    #[default]
    Both,
    Buy,
    Sell,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChainBehavior {
    pub block_ms: u64,
    /// Blocks a blockhash stays valid (mainnet: 150).
    pub blockhash_validity_blocks: u64,
    pub landing_latency_ms: u64,
    /// Blocks after landing until `finalized`.
    pub finality_blocks: u64,
    pub sol_usd: f64,
    pub starting_lamports: u64,
}

impl Default for ChainBehavior {
    fn default() -> Self {
        Self {
            block_ms: 400,
            blockhash_validity_blocks: 150,
            landing_latency_ms: 500,
            finality_blocks: 32,
            sol_usd: 150.0,
            starting_lamports: 5_000_000_000,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RpcFaults {
    /// Accepted by sendTransaction but never lands.
    pub drop_rate: f64,
    /// Lands, but sendTransaction returns an error.
    pub send_error_after_landing_rate: f64,
    /// Any RPC call returns HTTP 429.
    pub rate_limit_rate: f64,
    /// Any RPC call returns HTTP 503.
    pub server_error_rate: f64,
    /// Any RPC call stalls this long before answering (status timeouts).
    pub stall_ms: u64,
    pub stall_rate: f64,
    /// Landed signatures report as absent for this long (lagging node).
    pub status_lag_ms: u64,
    /// getTransaction returns null this long after confirmation.
    pub metadata_lag_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JupiterFaults {
    /// Jupiter returns 429 with x-ratelimit-reset.
    pub rate_limit_rate: f64,
    /// Jupiter returns 503.
    pub server_error_rate: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriggerBehavior {
    /// Compute-unit price (micro-lamports) Jupiter puts on deposits; set it
    /// high to simulate a priority-fee spike.
    pub deposit_cu_price: u64,
    pub cancel_cu_price: u64,
    /// Order create/cancel executes but the submit call returns 5xx.
    pub submit_error_after_landing_rate: f64,
    /// Order create/cancel submit returns 5xx and does not execute.
    pub submit_error_not_landed_rate: f64,
    /// Armed orders fill on their own after this many ms (0 = never).
    pub fill_after_ms: u64,
    /// Delay between a keeper fill and its separate empty-account close.
    pub refund_after_ms: u64,
    pub fill_stop_loss: bool,
    /// Armed orders expire (funds returned) after this many ms (0 = never).
    pub expire_after_ms: u64,
    /// Only the first N crafted deposits use `deposit_cu_price`; later
    /// ones use the normal price (a short-lived fee spike).
    pub spike_first_n: u32,
    /// Order history keeps reporting an expired order as `open` this long
    /// after expiry (while cancellation is already refused).
    pub history_lag_ms: u64,
}

impl Default for TriggerBehavior {
    fn default() -> Self {
        Self {
            deposit_cu_price: 100_000,
            cancel_cu_price: 100_000,
            submit_error_after_landing_rate: 0.0,
            submit_error_not_landed_rate: 0.0,
            fill_after_ms: 0,
            refund_after_ms: 1000,
            fill_stop_loss: false,
            expire_after_ms: 0,
            spike_first_n: 0,
            history_lag_ms: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JitoFaults {
    /// Jito block engine unavailable (HTTP 503).
    pub down: bool,
}

/// Deterministic, per-endpoint fault schedule. on_call=0 applies to every call.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SnapshotFault {
    pub on_call: u64,
    pub status: Option<u16>,
    pub delay_ms: u64,
    pub malformed: bool,
    pub body: Option<serde_json::Value>,
}
