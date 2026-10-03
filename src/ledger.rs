//! In-memory chain: blocks, blockhash expiry, landing latency, fees, SOL /
//! wrapped-SOL / SPL balances, token-account rent, and a Trigger vault.
//!
//! Transactions are real signed Solana transactions. The mock understands
//! four instruction kinds: compute budget, system transfer, and two mock
//! programs (swap and Trigger vault) whose instructions this service crafts
//! in its Jupiter endpoints. Failed transactions revert everything except
//! the fee, exactly as on Solana.

use std::{
    collections::{BTreeMap, HashMap},
    str::FromStr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::scenario::{ErrorSide, Scenario, TokenBehavior};

pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_ACCOUNT_RENT: u64 = 2_039_280;
/// SPL Token `CloseAccount` instruction data.
pub const CLOSE_ACCOUNT: u8 = 9;
pub const BASE_FEE_PER_SIGNATURE: u64 = 5_000;
pub const JITO_TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

// Instruction tags of the two mock programs.
pub const SWAP_TAG: u8 = 0xA1;
pub const TRIGGER_TAG: u8 = 0xB1;
pub const TRIGGER_DEPOSIT: u8 = 1;
pub const TRIGGER_WITHDRAW: u8 = 2;
pub const TRIGGER_FILL: u8 = 3;

pub fn derived(label: &str, parts: &[&[u8]]) -> Pubkey {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    for part in parts {
        hasher.update(part);
    }
    Pubkey::new_from_array(hasher.finalize().into())
}

pub fn swap_program() -> Pubkey {
    derived("mock-swap-program", &[])
}
pub fn trigger_program() -> Pubkey {
    derived("mock-trigger-program", &[])
}
pub fn system_program() -> Pubkey {
    Pubkey::default()
}
pub fn compute_budget_program() -> Pubkey {
    solana_compute_budget_interface::ID
}
pub fn sol_mint() -> Pubkey {
    Pubkey::from_str(SOL_MINT).expect("SOL mint")
}
/// Deterministic stand-in for an associated token account address.
pub fn token_account(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    derived("token-account", &[owner.as_ref(), mint.as_ref()])
}
pub fn vault_of(wallet: &Pubkey) -> Pubkey {
    derived("trigger-vault", &[wallet.as_ref()])
}
pub fn dust_mint() -> Pubkey {
    derived("dust-mint", &[])
}
pub fn route_fee_account() -> Pubkey {
    derived("route-fee-account", &[])
}
pub fn keeper() -> Pubkey {
    derived("trigger-keeper", &[])
}

#[derive(Clone, Debug, Default)]
struct Balances {
    lamports: HashMap<Pubkey, u64>,
    /// Token account address -> amount.
    tokens: HashMap<Pubkey, u64>,
}

#[derive(Clone, Debug)]
pub struct TokenAccountInfo {
    pub owner: Pubkey,
    pub mint: Pubkey,
}

#[derive(Clone, Debug)]
pub struct TxRecord {
    pub tx: VersionedTransaction,
    pub land_at: Instant,
    pub landed_height: Option<u64>,
    pub rendered: Option<Value>,
    pub err: Option<Value>,
    /// Never lands (dropped or expired).
    pub dropped: bool,
    pub kind: &'static str,
}

#[derive(Clone, Debug)]
pub struct Order {
    pub id: String,
    pub user: Pubkey,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub amount: u64,
    pub state: &'static str,
    pub deposit_signature: Signature,
    pub fill_signature: Option<Signature>,
    pub fill_at: Option<Instant>,
    pub expire_at: Option<Instant>,
    pub refund_at: Option<Instant>,
    pub filled_stop_loss: bool,
}

pub struct Ledger {
    started: Instant,
    pub scenario: Scenario,
    balances: Balances,
    pub token_accounts: HashMap<Pubkey, TokenAccountInfo>,
    pub decimals: HashMap<Pubkey, u8>,
    blockhashes: HashMap<Hash, u64>,
    pub txs: HashMap<Signature, TxRecord>,
    pub orders: BTreeMap<String, Order>,
    pub counters: BTreeMap<String, u64>,
    /// Swaps that already failed per mint, for `error_first_n`.
    failures_by_mint: HashMap<Pubkey, u32>,
    deposits_crafted: u32,
    rng: u64,
    nonce: u64,
}

pub enum SubmitError {
    Invalid(String),
    InsufficientFeePayer,
}

impl Ledger {
    pub fn new(scenario: Scenario) -> Self {
        Self {
            started: Instant::now(),
            scenario,
            balances: Balances::default(),
            token_accounts: HashMap::new(),
            decimals: HashMap::new(),
            blockhashes: HashMap::new(),
            txs: HashMap::new(),
            orders: BTreeMap::new(),
            counters: BTreeMap::new(),
            failures_by_mint: HashMap::new(),
            deposits_crafted: 0,
            rng: 0x9E37_79B9_7F4A_7C15,
            nonce: 0,
        }
    }

    pub fn count(&mut self, name: &str) {
        *self.counters.entry(name.to_owned()).or_default() += 1;
    }

    pub fn next_nonce(&mut self) -> u64 {
        self.nonce += 1;
        self.nonce
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        if probability <= 0.0 {
            return false;
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        ((x >> 11) as f64 / (1_u64 << 53) as f64) < probability
    }

    pub fn behavior(&self, mint: &Pubkey) -> TokenBehavior {
        self.scenario
            .tokens
            .get(&mint.to_string())
            .cloned()
            .unwrap_or_else(|| self.scenario.default_token.clone())
    }

    fn height_at(&self, at: Instant) -> u64 {
        (at.saturating_duration_since(self.started).as_millis() as u64)
            / self.scenario.chain.block_ms.max(1)
    }
    pub fn height(&self) -> u64 {
        self.height_at(Instant::now())
    }
    pub fn slot(&self) -> u64 {
        self.height() + 1_000
    }

    /// A fresh blockhash valid for `blockhash_validity_blocks`.
    pub fn issue_blockhash(&mut self) -> (Hash, u64) {
        let height = self.height();
        let hash = Hash::new_from_array(
            Sha256::digest(format!("blockhash-{height}-{}", self.next_nonce())).into(),
        );
        let last_valid = height + self.scenario.chain.blockhash_validity_blocks;
        self.blockhashes.insert(hash, last_valid);
        (hash, last_valid)
    }

    pub fn blockhash_valid(&self, hash: &Hash) -> bool {
        self.blockhashes
            .get(hash)
            .is_some_and(|last_valid| self.height() <= *last_valid)
    }

    pub fn ensure_wallet(&mut self, wallet: &Pubkey) {
        if !self.balances.lamports.contains_key(wallet) {
            self.balances
                .lamports
                .insert(*wallet, self.scenario.chain.starting_lamports);
            // A persistent wrapped-SOL account, as Jupiter wallets have.
            let wsol = token_account(wallet, &sol_mint());
            self.register_token_account(wsol, *wallet, sol_mint(), 9);
            self.balances.tokens.insert(wsol, 0);
            self.balances.lamports.insert(wsol, TOKEN_ACCOUNT_RENT);
        }
    }

    pub fn lamports(&mut self, account: &Pubkey) -> u64 {
        self.ensure_wallet(account);
        self.balances.lamports.get(account).copied().unwrap_or(0)
    }

    /// (token account, mint, amount, decimals) of every token account of `owner`.
    pub fn all_token_accounts_of(&self, owner: &Pubkey) -> Vec<(Pubkey, Pubkey, u64, u8)> {
        self.token_accounts
            .iter()
            .filter(|(_, info)| info.owner == *owner)
            .map(|(address, info)| {
                (
                    *address,
                    info.mint,
                    self.balances.tokens.get(address).copied().unwrap_or(0),
                    self.decimals.get(&info.mint).copied().unwrap_or(6),
                )
            })
            .collect()
    }

    pub fn account_lamports(&self, account: &Pubkey) -> u64 {
        self.balances.lamports.get(account).copied().unwrap_or(0)
    }

    /// (token account, amount, decimals) of every account `owner` holds in `mint`.
    pub fn token_accounts_of(&self, owner: &Pubkey, mint: &Pubkey) -> Vec<(Pubkey, u64, u8)> {
        self.token_accounts
            .iter()
            .filter(|(_, info)| info.owner == *owner && info.mint == *mint)
            .map(|(address, _)| {
                (
                    *address,
                    self.balances.tokens.get(address).copied().unwrap_or(0),
                    self.decimals.get(mint).copied().unwrap_or(6),
                )
            })
            .collect()
    }

    pub fn token_balance(&self, owner: &Pubkey, mint: &Pubkey) -> u64 {
        self.balances
            .tokens
            .get(&token_account(owner, mint))
            .copied()
            .unwrap_or(0)
    }

    pub fn register_token_account(
        &mut self,
        address: Pubkey,
        owner: Pubkey,
        mint: Pubkey,
        decimals: u8,
    ) {
        self.token_accounts
            .insert(address, TokenAccountInfo { owner, mint });
        self.decimals.entry(mint).or_insert(decimals);
    }

    /// Queue a signed transaction. Returns its signature; resubmitting the
    /// same bytes is idempotent.
    pub fn submit(
        &mut self,
        tx: VersionedTransaction,
        kind: &'static str,
        delay: Duration,
    ) -> Result<Signature, SubmitError> {
        let signature = *tx
            .signatures
            .first()
            .ok_or_else(|| SubmitError::Invalid("transaction has no signatures".into()))?;
        if signature == Signature::default() {
            return Err(SubmitError::Invalid("transaction is not signed".into()));
        }
        if self.txs.contains_key(&signature) {
            return Ok(signature);
        }
        let payer = *tx
            .message
            .static_account_keys()
            .first()
            .ok_or_else(|| SubmitError::Invalid("transaction has no fee payer".into()))?;
        self.ensure_wallet(&payer);
        if self.balances.lamports.get(&payer).copied().unwrap_or(0) < BASE_FEE_PER_SIGNATURE {
            return Err(SubmitError::InsufficientFeePayer);
        }
        self.txs.insert(
            signature,
            TxRecord {
                tx,
                land_at: Instant::now() + delay,
                landed_height: None,
                rendered: None,
                err: None,
                dropped: false,
                kind,
            },
        );
        Ok(signature)
    }

    pub fn drop_pending(&mut self, signature: &Signature) {
        if let Some(record) = self.txs.get_mut(signature) {
            record.dropped = true;
        }
    }

    /// Land due transactions and run due Trigger keeper events.
    pub fn settle(&mut self) {
        let now = Instant::now();
        let mut due: Vec<(Instant, Signature)> = self
            .txs
            .iter()
            .filter(|(_, record)| {
                !record.dropped && record.landed_height.is_none() && record.land_at <= now
            })
            .map(|(signature, record)| (record.land_at, *signature))
            .collect();
        due.sort();
        for (_, signature) in due {
            self.land(signature);
        }
        self.run_keeper(now);
    }

    fn land(&mut self, signature: Signature) {
        let record = self.txs.get(&signature).expect("due").clone();
        let height = self.height_at(record.land_at);
        let blockhash = *record.tx.message.recent_blockhash();
        if let Some(last_valid) = self.blockhashes.get(&blockhash)
            && height > *last_valid
        {
            self.count("expired_before_landing");
            self.txs.get_mut(&signature).expect("record").dropped = true;
            return;
        }
        let (balances, rendered, err) = self.execute(&record.tx, &signature, true);
        self.balances = balances;
        let record = self.txs.get_mut(&signature).expect("record");
        record.landed_height = Some(height);
        record.rendered = Some(rendered);
        record.err = err;
    }

    /// Dry-run against current state: (error, wallet post-lamports).
    pub fn simulate(&mut self, tx: &VersionedTransaction, wallet: &Pubkey) -> (Option<Value>, u64) {
        self.ensure_wallet(wallet);
        let (balances, _, err) = self.execute(tx, &Signature::default(), false);
        let lamports = if err.is_some() {
            self.balances.lamports.get(wallet).copied().unwrap_or(0)
        } else {
            balances.lamports.get(wallet).copied().unwrap_or(0)
        };
        (err, lamports)
    }

    fn swap_error(
        &mut self,
        mint: &Pubkey,
        buy: bool,
        instruction_index: usize,
        commit: bool,
    ) -> Option<Value> {
        let behavior = self.behavior(mint);
        let kind = behavior.onchain_error.as_deref()?;
        let applies = match behavior.error_on {
            ErrorSide::Both => true,
            ErrorSide::Buy => buy,
            ErrorSide::Sell => !buy,
        };
        // Forced errors model execution-time conditions (price moved,
        // route state changed), so simulation does not see them.
        if !applies || !commit {
            return None;
        }
        let failed = self.failures_by_mint.entry(*mint).or_default();
        if behavior.error_first_n > 0 && *failed >= behavior.error_first_n {
            return None;
        }
        *failed += 1;
        self.count(&format!("onchain_error:{kind}"));
        Some(onchain_error(kind, instruction_index))
    }

    /// Execute on a copy of the balances. Returns (new balances, rendered
    /// getTransaction JSON, error). On error only the fee is charged.
    fn execute(
        &mut self,
        tx: &VersionedTransaction,
        signature: &Signature,
        commit: bool,
    ) -> (Balances, Value, Option<Value>) {
        let keys = tx.message.static_account_keys().to_vec();
        let payer = keys[0];
        let pre = self.balances.clone();
        let mut post = pre.clone();
        let signatures = u64::from(tx.message.header().num_required_signatures);
        let mut unit_limit: Option<u64> = None;
        let mut unit_price = 0_u64;
        let mut non_budget = 0_u64;
        let mut wsol_funded: HashMap<Pubkey, u64> = HashMap::new();
        let mut err: Option<Value> = None;
        let mut new_accounts: Vec<(Pubkey, Pubkey, Pubkey, u8)> = Vec::new();
        let mut closed_accounts: Vec<(Pubkey, u64)> = Vec::new();
        let mut closes_accounts = false;
        if tx
            .message
            .address_table_lookups()
            .is_some_and(|lookups| !lookups.is_empty())
        {
            err = Some(json!({"InstructionError": [0, "MissingAccount"]}));
        }
        for (index, instruction) in tx.message.instructions().iter().enumerate() {
            if err.is_some() {
                break;
            }
            let program = keys
                .get(usize::from(instruction.program_id_index))
                .copied()
                .unwrap_or_default();
            let account = |position: usize| -> Pubkey {
                instruction
                    .accounts
                    .get(position)
                    .and_then(|index| keys.get(usize::from(*index)))
                    .copied()
                    .unwrap_or_default()
            };
            let data = instruction.data.as_slice();
            if program == compute_budget_program() {
                match data {
                    [2, bytes @ ..] if bytes.len() == 4 => {
                        unit_limit = Some(u64::from(u32::from_le_bytes(bytes.try_into().unwrap())));
                    }
                    [3, bytes @ ..] if bytes.len() == 8 => {
                        unit_price = u64::from_le_bytes(bytes.try_into().unwrap());
                    }
                    _ => {}
                }
                continue;
            }
            non_budget += 1;
            if program == system_program() {
                if data.len() == 12 && data[..4] == 2_u32.to_le_bytes() {
                    let lamports = u64::from_le_bytes(data[4..12].try_into().unwrap());
                    let (from, to) = (account(0), account(1));
                    let balance = post.lamports.entry(from).or_default();
                    if *balance < lamports {
                        err = Some(json!({"InstructionError": [index, {"Custom": 1}]}));
                        continue;
                    }
                    *balance -= lamports;
                    *post.lamports.entry(to).or_default() += lamports;
                    if self
                        .token_accounts
                        .get(&to)
                        .is_some_and(|info| info.mint == sol_mint())
                    {
                        *wsol_funded.entry(to).or_default() += lamports;
                    }
                }
                continue;
            }
            if program == swap_program() {
                err = self.execute_swap(
                    data,
                    &account,
                    index,
                    &mut post,
                    &mut wsol_funded,
                    &mut new_accounts,
                    commit,
                );
                continue;
            }
            if program == trigger_program() {
                err = self.execute_trigger(data, &account, index, &mut post, &mut new_accounts);
                continue;
            }
            if program.to_string() == TOKEN_PROGRAM && data == [CLOSE_ACCOUNT] {
                // SPL Token CloseAccount: [account, destination, owner].
                closes_accounts = true;
                let (closing, destination, owner) = (account(0), account(1), account(2));
                let info = self.token_accounts.get(&closing).cloned();
                let signer = tx.message.static_account_keys()
                    [..usize::from(tx.message.header().num_required_signatures)]
                    .contains(&owner);
                match (info, post.tokens.get(&closing).copied()) {
                    (Some(info), Some(0)) if info.owner == owner && signer => {
                        let rent = post.lamports.remove(&closing).unwrap_or(0);
                        post.tokens.remove(&closing);
                        *post.lamports.entry(destination).or_default() += rent;
                        closed_accounts.push((closing, rent));
                    }
                    (Some(_), Some(_)) => {
                        // 11 = NonNativeHasBalance, 4 = OwnerMismatch.
                        let code = if post.tokens.get(&closing).copied().unwrap_or(0) > 0 {
                            11
                        } else {
                            4
                        };
                        err = Some(json!({"InstructionError": [index, {"Custom": code}]}));
                    }
                    _ => err = Some(json!({"InstructionError": [index, "InvalidAccountData"]})),
                }
                continue;
            }
        }
        let limit = unit_limit.unwrap_or((non_budget * 200_000).min(1_400_000));
        let priority =
            u64::try_from((u128::from(limit) * u128::from(unit_price)).div_ceil(1_000_000))
                .unwrap_or(u64::MAX);
        let fee = BASE_FEE_PER_SIGNATURE * signatures + priority;
        let mut outcome = if err.is_some() { pre.clone() } else { post };
        let payer_balance = outcome.lamports.entry(payer).or_default();
        *payer_balance = payer_balance.saturating_sub(fee);
        // Simulations create nothing on chain.
        if err.is_none() && commit {
            for (address, owner, mint, decimals) in &new_accounts {
                self.register_token_account(*address, *owner, *mint, *decimals);
            }
        }
        let rendered = self.render(tx, signature, &pre, &outcome, fee, err.as_ref(), limit);
        if commit && closes_accounts && payer != keeper() {
            *self
                .counters
                .entry("token_close_fees_lamports".to_owned())
                .or_default() += fee;
        }
        if err.is_none() && commit {
            for (closed, rent) in closed_accounts {
                self.token_accounts.remove(&closed);
                self.count("token_accounts_closed");
                *self
                    .counters
                    .entry("token_rent_refunded_lamports".to_owned())
                    .or_default() += rent;
            }
        }
        (outcome, rendered, err)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_swap(
        &mut self,
        data: &[u8],
        account: &dyn Fn(usize) -> Pubkey,
        index: usize,
        post: &mut Balances,
        wsol_funded: &mut HashMap<Pubkey, u64>,
        new_accounts: &mut Vec<(Pubkey, Pubkey, Pubkey, u8)>,
        commit: bool,
    ) -> Option<Value> {
        // [tag][side 0=buy 1=sell][mint 32][amount u64][min_out u64][nonce u64]
        if data.len() != 1 + 1 + 32 + 24 || data[0] != SWAP_TAG {
            return Some(json!({"InstructionError": [index, "InvalidInstructionData"]}));
        }
        let buy = data[1] == 0;
        let mint = Pubkey::new_from_array(data[2..34].try_into().unwrap());
        let amount = u64::from_le_bytes(data[34..42].try_into().unwrap());
        let minimum_out = u64::from_le_bytes(data[42..50].try_into().unwrap());
        let wallet = account(0);
        let behavior = self.behavior(&mint);
        if let Some(error) = self.swap_error(&mint, buy, index, commit) {
            return Some(error);
        }
        let drift = 1.0 + behavior.execution_drift_bps as f64 / 10_000.0;
        let token_account_address = token_account(&wallet, &mint);
        if buy {
            let wsol = token_account(&wallet, &sol_mint());
            let funded = wsol_funded.get(&wsol).copied().unwrap_or(0);
            if funded < amount {
                return Some(json!({"InstructionError": [index, {"Custom": 6036}]}));
            }
            *wsol_funded.entry(wsol).or_default() -= amount;
            let pool = post.lamports.entry(wsol).or_default();
            *pool = pool.saturating_sub(amount);
            let out = if behavior.zero_fill {
                0
            } else {
                (amount as f64 * behavior.tokens_per_lamport * drift) as u64
            };
            if out < minimum_out {
                return Some(json!({"InstructionError": [index, {"Custom": 6001}]}));
            }
            if !post.tokens.contains_key(&token_account_address) {
                let payer = post.lamports.entry(wallet).or_default();
                if *payer < TOKEN_ACCOUNT_RENT {
                    return Some(json!({"InsufficientFundsForRent": {"account_index": 2}}));
                }
                *payer -= TOKEN_ACCOUNT_RENT;
                post.lamports
                    .insert(token_account_address, TOKEN_ACCOUNT_RENT);
                post.tokens.insert(token_account_address, 0);
                new_accounts.push((token_account_address, wallet, mint, behavior.decimals));
            }
            *post.tokens.entry(token_account_address).or_default() += out;
            if behavior.auxiliary_debit_lamports > 0 {
                let payer = post.lamports.entry(wallet).or_default();
                *payer = payer.saturating_sub(behavior.auxiliary_debit_lamports);
                *post.lamports.entry(route_fee_account()).or_default() +=
                    behavior.auxiliary_debit_lamports;
            }
        } else {
            let held = post
                .tokens
                .get(&token_account_address)
                .copied()
                .unwrap_or(0);
            if held < amount {
                return Some(json!({"InstructionError": [index, {"Custom": 6024}]}));
            }
            let proceeds = if behavior.zero_fill {
                0
            } else {
                (amount as f64 / behavior.tokens_per_lamport * drift) as u64
            };
            if proceeds < minimum_out {
                return Some(json!({"InstructionError": [index, {"Custom": 6001}]}));
            }
            *post.tokens.get_mut(&token_account_address).expect("held") -= amount;
            *post.lamports.entry(wallet).or_default() += proceeds;
        }
        if behavior.dust_second_mint {
            let dust = token_account(&wallet, &dust_mint());
            if let std::collections::hash_map::Entry::Vacant(entry) = post.tokens.entry(dust) {
                entry.insert(0);
                post.lamports.insert(dust, 0);
                new_accounts.push((dust, wallet, dust_mint(), 6));
            }
            *post.tokens.entry(dust).or_default() += 1;
        }
        None
    }

    fn execute_trigger(
        &mut self,
        data: &[u8],
        account: &dyn Fn(usize) -> Pubkey,
        index: usize,
        post: &mut Balances,
        new_accounts: &mut Vec<(Pubkey, Pubkey, Pubkey, u8)>,
    ) -> Option<Value> {
        // [tag][op][order nonce u64][mint 32][amount u64][proceeds u64]
        if data.len() != 1 + 1 + 8 + 32 + 16 || data[0] != TRIGGER_TAG {
            return Some(json!({"InstructionError": [index, "InvalidInstructionData"]}));
        }
        let op = data[1];
        let mint = Pubkey::new_from_array(data[10..42].try_into().unwrap());
        let amount = u64::from_le_bytes(data[42..50].try_into().unwrap());
        let proceeds = u64::from_le_bytes(data[50..58].try_into().unwrap());
        let wallet = account(1);
        let wallet_tokens = token_account(&wallet, &mint);
        let vault = vault_of(&wallet);
        let vault_tokens = token_account(&vault, &mint);
        if let std::collections::hash_map::Entry::Vacant(entry) = post.tokens.entry(vault_tokens) {
            entry.insert(0);
            post.lamports.insert(vault_tokens, TOKEN_ACCOUNT_RENT);
            let available = post.lamports.entry(wallet).or_default();
            if *available < TOKEN_ACCOUNT_RENT {
                return Some(json!({"InstructionError":[index,"InsufficientFunds"]}));
            }
            *available -= TOKEN_ACCOUNT_RENT;
            let decimals = self.decimals.get(&mint).copied().unwrap_or(6);
            new_accounts.push((vault_tokens, vault, mint, decimals));
        }
        let (from, to) = match op {
            TRIGGER_DEPOSIT => (wallet_tokens, vault_tokens),
            TRIGGER_WITHDRAW => (vault_tokens, wallet_tokens),
            TRIGGER_FILL => (vault_tokens, Pubkey::default()),
            _ => return Some(json!({"InstructionError": [index, "InvalidInstructionData"]})),
        };
        let held = post.tokens.get(&from).copied().unwrap_or(0);
        if held < amount {
            return Some(json!({"InstructionError": [index, {"Custom": 6024}]}));
        }
        *post.tokens.get_mut(&from).expect("held") -= amount;
        if op == TRIGGER_FILL {
            *post.lamports.entry(wallet).or_default() += proceeds;
        } else {
            *post.tokens.entry(to).or_default() += amount;
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        tx: &VersionedTransaction,
        _signature: &Signature,
        pre: &Balances,
        post: &Balances,
        fee: u64,
        err: Option<&Value>,
        unit_limit: u64,
    ) -> Value {
        let keys = tx.message.static_account_keys();
        let header = tx.message.header();
        let signers = usize::from(header.num_required_signatures);
        let account_keys: Vec<Value> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                json!({
                    "pubkey": key.to_string(),
                    "signer": index < signers,
                    "writable": tx.message.is_maybe_writable(index, None),
                    "source": "transaction",
                })
            })
            .collect();
        let lamports =
            |balances: &Balances, key: &Pubkey| balances.lamports.get(key).copied().unwrap_or(0);
        let token_balances = |balances: &Balances| -> Vec<Value> {
            keys.iter()
                .enumerate()
                .filter_map(|(index, key)| {
                    let amount = balances.tokens.get(key)?;
                    let info = self.token_accounts.get(key).cloned()?;
                    let decimals = self.decimals.get(&info.mint).copied().unwrap_or(6);
                    Some(json!({
                        "accountIndex": index,
                        "mint": info.mint.to_string(),
                        "owner": info.owner.to_string(),
                        "programId": TOKEN_PROGRAM,
                        "uiTokenAmount": {
                            "amount": amount.to_string(),
                            "decimals": decimals,
                            "uiAmount": *amount as f64 / 10_f64.powi(i32::from(decimals)),
                            "uiAmountString": (*amount as f64 / 10_f64.powi(i32::from(decimals))).to_string(),
                        }
                    }))
                })
                .collect()
        };
        let instructions: Vec<Value> = tx
            .message
            .instructions()
            .iter()
            .map(|instruction| {
                let program = keys
                    .get(usize::from(instruction.program_id_index))
                    .copied()
                    .unwrap_or_default();
                let accounts: Vec<String> = instruction
                    .accounts
                    .iter()
                    .filter_map(|index| keys.get(usize::from(*index)))
                    .map(ToString::to_string)
                    .collect();
                let data = instruction.data.as_slice();
                if program.to_string()==TOKEN_PROGRAM && data==[CLOSE_ACCOUNT] {
                    return json!({"program":"spl-token","programId":program.to_string(),"parsed":{"type":"closeAccount","info":{"account":accounts.first(),"destination":accounts.get(1),"owner":accounts.get(2)}}});
                }
                if program == system_program()
                    && data.len() == 12
                    && data[..4] == 2_u32.to_le_bytes()
                {
                    return json!({
                        "program": "system",
                        "programId": program.to_string(),
                        "parsed": {
                            "type": "transfer",
                            "info": {
                                "source": accounts.first(),
                                "destination": accounts.get(1),
                                "lamports": u64::from_le_bytes(data[4..12].try_into().unwrap()),
                            }
                        },
                        "stackHeight": null,
                    });
                }
                json!({
                    "programId": program.to_string(),
                    "accounts": accounts,
                    "data": bs58::encode(data).into_string(),
                    "stackHeight": null,
                })
            })
            .collect();
        // Token-balance snapshots need the post-state registry, which is
        // updated only after a successful execution; render with both.
        let pre_tokens = token_balances(pre);
        let post_tokens = if err.is_some() {
            token_balances(pre)
        } else {
            self.post_token_balances(keys, post)
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        json!({
            "slot": self.slot(),
            "blockTime": now,
            "version": 0,
            "meta": {
                "err": err,
                "status": match err { Some(error) => json!({"Err": error}), None => json!({"Ok": null}) },
                "fee": fee,
                "preBalances": keys.iter().map(|key| lamports(pre, key)).collect::<Vec<_>>(),
                "postBalances": keys.iter().map(|key| lamports(post, key)).collect::<Vec<_>>(),
                "preTokenBalances": pre_tokens,
                "postTokenBalances": post_tokens,
                "computeUnitsConsumed": unit_limit.min(60_000),
                "innerInstructions": [],
                "logMessages": [],
                "loadedAddresses": {"writable": [], "readonly": []},
            },
            "transaction": {
                "signatures": tx.signatures.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "message": {
                    "accountKeys": account_keys,
                    "recentBlockhash": tx.message.recent_blockhash().to_string(),
                    "instructions": instructions,
                }
            }
        })
    }

    /// Post-state token balances, including accounts created in this
    /// transaction (registered by the caller only after rendering).
    fn post_token_balances(&self, keys: &[Pubkey], post: &Balances) -> Vec<Value> {
        keys.iter()
            .enumerate()
            .filter_map(|(index, key)| {
                let amount = post.tokens.get(key)?;
                let info = self.token_accounts.get(key)?;
                let decimals = self.decimals.get(&info.mint).copied().unwrap_or(6);
                Some(json!({
                    "accountIndex": index,
                    "mint": info.mint.to_string(),
                    "owner": info.owner.to_string(),
                    "programId": TOKEN_PROGRAM,
                    "uiTokenAmount": {"amount": amount.to_string(), "decimals": decimals},
                }))
            })
            .collect()
    }

    /// Trigger keeper: fill or expire armed orders when due.
    fn run_keeper(&mut self, now: Instant) {
        let due: Vec<String> = self
            .orders
            .values()
            .filter(|order| {
                order.state == "open"
                    && (order.fill_at.is_some_and(|at| at <= now)
                        || order.expire_at.is_some_and(|at| at <= now))
            })
            .map(|order| order.id.clone())
            .collect();
        for id in due {
            let order = self.orders.get(&id).expect("order").clone();
            let filling = order.fill_at.is_some_and(|at| at <= now);
            let behavior = self.behavior(&order.input_mint);
            let proceeds = (order.amount as f64 / behavior.tokens_per_lamport) as u64;
            let op = if filling {
                TRIGGER_FILL
            } else {
                TRIGGER_WITHDRAW
            };
            let keeper = keeper();
            self.ensure_wallet(&keeper);
            let tx = crate::providers::jupiter::trigger_transaction(
                self,
                &keeper,
                &order.user,
                op,
                order_nonce(&order.id),
                &order.input_mint,
                order.amount,
                proceeds,
                0,
            );
            let mut tx = tx;
            let mut raw = [0_u8; 64];
            raw[..8].copy_from_slice(&self.next_nonce().to_le_bytes());
            raw[8..40].copy_from_slice(keeper.as_ref());
            tx.signatures = vec![Signature::from(raw)];
            let Ok(signature) = self.submit(
                tx,
                if filling {
                    "trigger_fill"
                } else {
                    "trigger_expiry"
                },
                Duration::ZERO,
            ) else {
                continue;
            };
            self.land(signature);
            let order = self.orders.get_mut(&id).expect("order");
            if filling {
                order.state = "filled";
                order.fill_signature = Some(signature);
                order.refund_at =
                    Some(now + Duration::from_millis(self.scenario.trigger.refund_after_ms));
                self.count("trigger_fills");
            } else {
                order.state = "expired";
                self.count("trigger_expiries");
            }
        }
        let refunds: Vec<_> = self
            .orders
            .values()
            .filter(|o| o.refund_at.is_some_and(|at| at <= now))
            .cloned()
            .collect();
        for order in refunds {
            let payer = keeper();
            let vault = vault_of(&order.user);
            let account = token_account(&vault, &order.input_mint);
            let instruction = solana_instruction::Instruction {
                program_id: Pubkey::from_str(TOKEN_PROGRAM).unwrap(),
                accounts: vec![
                    solana_instruction::AccountMeta::new(account, false),
                    solana_instruction::AccountMeta::new(order.user, false),
                    solana_instruction::AccountMeta::new_readonly(vault, true),
                ],
                data: vec![CLOSE_ACCOUNT],
            };
            let (hash, _) = self.issue_blockhash();
            let mut tx = crate::providers::jupiter::unsigned(&payer, &[instruction], hash);
            let mut raw = [0_u8; 64];
            raw[..8].copy_from_slice(&self.next_nonce().to_le_bytes());
            raw[8..40].copy_from_slice(payer.as_ref());
            tx.signatures = vec![Signature::from(raw)];
            if let Ok(sig) = self.submit(tx, "trigger_refund", Duration::ZERO) {
                self.land(sig);
                self.orders.get_mut(&order.id).unwrap().refund_at = None;
                self.count("trigger_rent_refunds");
            }
        }
    }

    pub fn state_summary(&self) -> Value {
        let wallets: BTreeMap<String, u64> = self
            .balances
            .lamports
            .iter()
            .filter(|(key, _)| !self.token_accounts.contains_key(key))
            .map(|(key, lamports)| (key.to_string(), *lamports))
            .collect();
        let tokens: Vec<Value> = self
            .balances
            .tokens
            .iter()
            .filter(|(_, amount)| **amount > 0)
            .filter_map(|(address, amount)| {
                let info = self.token_accounts.get(address)?;
                Some(json!({"owner": info.owner.to_string(), "mint": info.mint.to_string(), "amount": amount}))
            })
            .collect();
        let transactions: Vec<Value> = self
            .txs
            .iter()
            .map(|(signature, record)| {
                json!({
                    "signature": signature.to_string(),
                    "kind": record.kind,
                    "landed": record.landed_height.is_some(),
                    "dropped": record.dropped,
                    "err": record.err,
                    "fee": record.rendered.as_ref().map(|r| r["meta"]["fee"].clone()),
                })
            })
            .collect();
        let orders: Vec<Value> = self
            .orders
            .values()
            .map(|order| json!({"id": order.id, "state": order.state, "mint": order.input_mint.to_string(), "amount": order.amount}))
            .collect();
        json!({
            "height": self.height(),
            "wallet_lamports": wallets,
            "token_balances": tokens,
            "transactions": transactions,
            "orders": orders,
            "counters": self.counters,
        })
    }

    pub fn deposits_crafted(&mut self) -> u32 {
        self.deposits_crafted += 1;
        self.deposits_crafted
    }
}

pub fn order_nonce(id: &str) -> u64 {
    id.rsplit('-')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn onchain_error(kind: &str, index: usize) -> Value {
    match kind {
        "slippage" => json!({"InstructionError": [index, {"Custom": 6001}]}),
        "route_6024" => json!({"InstructionError": [index, {"Custom": 6024}]}),
        "route_6036" => json!({"InstructionError": [index, {"Custom": 6036}]}),
        "program_failed_to_complete" => {
            json!({"InstructionError": [index, "ProgramFailedToComplete"]})
        }
        "missing_account" => json!({"InstructionError": [index, "MissingAccount"]}),
        "insufficient_funds_for_rent" => json!({"InsufficientFundsForRent": {"account_index": 2}}),
        _ => json!({"InstructionError": [index, {"Custom": 1}]}),
    }
}
