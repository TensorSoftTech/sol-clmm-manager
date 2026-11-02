//! Build / simulate / send **transaction v1** only (SIMD-0385).
//!
//! Resource limits live in [`TxBudget`] → `v1::TransactionConfig` (not ComputeBudget ixs).
//! Before send we can **estimate** CU + loaded-data via a probe simulation and tip via
//! `getRecentPrioritizationFees` (and Jupiter tip when a swap is present).

use anyhow::{Context, Result, bail};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSimulateTransactionConfig;
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::VersionedMessage;
use solana_message::v1::{Message as V1Message, TransactionConfig};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::{TransactionVersion, VersionedTransaction};
use solana_transaction_status_client_types::UiTransactionEncoding;

/// How tip / CU are chosen before send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetMode {
    /// Probe-sim CU/data + recent prioritization fees (default).
    #[default]
    Auto,
    /// Use fixed [`TxBudget`] from settings only.
    Manual,
}

impl BudgetMode {
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    pub fn from_setting(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "manual" => Self::Manual,
            _ => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto · sim CU + priority fees",
            Self::Manual => "manual · settings floors",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Manual,
            Self::Manual => Self::Auto,
        }
    }
}

/// User-configurable v1 resource budget (settings / confirm screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxBudget {
    pub mode: BudgetMode,
    /// Compute unit limit (v1: unset = 0 — must set).
    pub compute_unit_limit: u32,
    /// Total priority fee in **lamports** (v1 tip is a total, not µ-lamports/CU).
    pub priority_fee_lamports: u64,
    /// Loaded accounts data size limit in bytes.
    pub loaded_accounts_data_size_limit: u32,
    /// Heap frame bytes (multiple of 1024).
    pub heap_size: u32,
    /// Extra CU % on top of simulation (auto mode).
    pub cu_margin_bps: u32,
    /// Floor / ceiling for auto tip (lamports).
    pub tip_floor_lamports: u64,
    pub tip_ceiling_lamports: u64,
}

impl Default for TxBudget {
    fn default() -> Self {
        Self {
            mode: BudgetMode::Auto,
            compute_unit_limit: 400_000,
            // Fixed priority fee (lamports). `0` = off · `u64::MAX` = network auto.
            priority_fee_lamports: 5_000,
            loaded_accounts_data_size_limit: 64 * 1024,
            heap_size: 64 * 1024,
            cu_margin_bps: 1_500, // +15%
            tip_floor_lamports: 0,
            tip_ceiling_lamports: 25_000,
        }
    }
}

impl TxBudget {
    pub fn to_config(self) -> TransactionConfig {
        let heap = if self.heap_size == 0 {
            32 * 1024
        } else {
            self.heap_size
        };
        TransactionConfig::empty()
            .with_compute_unit_limit(self.compute_unit_limit)
            .with_priority_fee(self.priority_fee_lamports)
            .with_loaded_accounts_data_size_limit(self.loaded_accounts_data_size_limit)
            .with_heap_size(heap)
    }

    /// Maxed probe budget so simulation can measure real CU / loaded data.
    pub fn probe() -> Self {
        Self {
            mode: BudgetMode::Manual,
            compute_unit_limit: 1_400_000,
            priority_fee_lamports: 0,
            loaded_accounts_data_size_limit: 64 * 1024 * 1024,
            heap_size: 256 * 1024,
            cu_margin_bps: 0,
            tip_floor_lamports: 0,
            tip_ceiling_lamports: u64::MAX,
        }
    }

    pub fn label(self) -> String {
        format!(
            "{} · CU {} · tip {} lamports · data {} · heap {}",
            self.mode.as_setting(),
            self.compute_unit_limit,
            self.priority_fee_lamports,
            self.loaded_accounts_data_size_limit,
            if self.heap_size == 0 {
                32 * 1024
            } else {
                self.heap_size
            }
        )
    }
}

#[derive(Debug)]
pub enum TxBuildError {
    Compile(String),
    Sign(String),
}

impl std::fmt::Display for TxBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile(s) => write!(f, "v1 message compile: {s}"),
            Self::Sign(s) => write!(f, "sign: {s}"),
        }
    }
}

impl std::error::Error for TxBuildError {}

/// Signed tx v1 ready for simulate / send.
#[derive(Debug, Clone)]
pub struct PreparedTx {
    pub transaction: VersionedTransaction,
    pub budget: TxBudget,
    pub recent_blockhash: Hash,
}

pub fn prepare_v1_tx(
    payer: &Keypair,
    instructions: &[Instruction],
    recent_blockhash: Hash,
    budget: TxBudget,
) -> Result<PreparedTx, TxBuildError> {
    prepare_v1_tx_signers(payer, &[], instructions, recent_blockhash, budget)
}

/// Like [`prepare_v1_tx`] but also signs with `extra` keypairs (e.g. position NFT mint).
pub fn prepare_v1_tx_signers(
    payer: &Keypair,
    extra: &[&Keypair],
    instructions: &[Instruction],
    recent_blockhash: Hash,
    budget: TxBudget,
) -> Result<PreparedTx, TxBuildError> {
    if instructions.is_empty() {
        return Err(TxBuildError::Compile("no instructions".into()));
    }
    let message = V1Message::try_compile_with_config(
        &payer.pubkey(),
        instructions,
        recent_blockhash,
        budget.to_config(),
    )
    .map_err(|e| TxBuildError::Compile(e.to_string()))?;

    let mut signers: Vec<&Keypair> = Vec::with_capacity(1 + extra.len());
    signers.push(payer);
    signers.extend_from_slice(extra);
    let transaction = VersionedTransaction::try_new(VersionedMessage::V1(message), &signers)
        .map_err(|e| TxBuildError::Sign(e.to_string()))?;

    match transaction.version() {
        TransactionVersion::Number(1) => {}
        other => {
            return Err(TxBuildError::Compile(format!(
                "expected tx v1, got {other:?}"
            )));
        }
    }

    Ok(PreparedTx {
        transaction,
        budget,
        recent_blockhash,
    })
}

fn sim_config() -> RpcSimulateTransactionConfig {
    RpcSimulateTransactionConfig {
        sig_verify: false,
        replace_recent_blockhash: true,
        commitment: Some(CommitmentConfig::confirmed()),
        encoding: Some(UiTransactionEncoding::Base64),
        accounts: None,
        min_context_slot: None,
        inner_instructions: false,
    }
}

fn annotate_sim_program_error(logs: &[String]) -> Option<&'static str> {
    let blob = logs.join("\n");
    if blob.contains("0x1781") {
        // Whirlpool / CLMM: increase_liquidity tokenMax too tight vs ceil deltas.
        Some("TokenMaxExceeded (0x1781) · deposit needs more tokens than tokenMax for this L")
    } else if blob.contains("0x1782") {
        Some("TokenMinSubceeded (0x1782) · withdraw below tokenMin")
    } else {
        None
    }
}

/// Simulate a v1 tx (base64). Errors if `err` is set.
pub fn simulate_v1_tx(rpc: &RpcClient, prepared: &PreparedTx) -> Result<Vec<String>> {
    let result = rpc
        .simulate_transaction_with_config(&prepared.transaction, sim_config())
        .context("simulate_transaction (v1)")?;
    if let Some(err) = result.value.err {
        let logs = result.value.logs.clone().unwrap_or_default();
        if let Some(hint) = annotate_sim_program_error(&logs) {
            bail!("simulation failed: {hint}\n{err:?}\n{}", logs.join("\n"));
        }
        bail!("simulation failed: {err:?}\n{}", logs.join("\n"));
    }
    Ok(result.value.logs.unwrap_or_default())
}

/// Estimate optimal [`TxBudget`] for this ix set:
/// 1. Probe-sim with max CU/data → `units_consumed` + `loaded_accounts_data_size`
/// 2. `getRecentPrioritizationFees` on writable accounts → tip (µ-lamports/CU → total lamports)
/// 3. Optional `jupiter_tip_lamports` floor (from Jupiter swap quote)
/// 4. Apply margin / floor / ceiling from `prefs`
pub fn estimate_budget(
    rpc: &RpcClient,
    payer: &Keypair,
    instructions: &[Instruction],
    recent_blockhash: Hash,
    prefs: TxBudget,
    writable_hint: &[Pubkey],
    jupiter_tip_lamports: Option<u64>,
) -> Result<TxBudget> {
    estimate_budget_signers(
        rpc,
        payer,
        &[],
        instructions,
        recent_blockhash,
        prefs,
        writable_hint,
        jupiter_tip_lamports,
    )
}

/// Like [`estimate_budget`] but probe-signs with `extra` (position mint, etc.).
#[allow(clippy::too_many_arguments)]
pub fn estimate_budget_signers(
    rpc: &RpcClient,
    payer: &Keypair,
    extra: &[&Keypair],
    instructions: &[Instruction],
    recent_blockhash: Hash,
    prefs: TxBudget,
    writable_hint: &[Pubkey],
    jupiter_tip_lamports: Option<u64>,
) -> Result<TxBudget> {
    let (cu, loaded_rounded, mode) = if prefs.mode == BudgetMode::Manual {
        (
            prefs.compute_unit_limit,
            prefs.loaded_accounts_data_size_limit,
            BudgetMode::Manual,
        )
    } else {
        let probe = prepare_v1_tx_signers(
            payer,
            extra,
            instructions,
            recent_blockhash,
            TxBudget::probe(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let result = rpc
            .simulate_transaction_with_config(&probe.transaction, sim_config())
            .context("probe simulate for CU")?;

        let units = result.value.units_consumed.unwrap_or(200_000).max(1);
        let loaded = result
            .value
            .loaded_accounts_data_size
            .unwrap_or(prefs.loaded_accounts_data_size_limit)
            .max(64 * 1024);

        // Round loaded data up to next 32 KiB page (block cost model).
        let page = 32u32 * 1024;
        let loaded_rounded = loaded.saturating_add(page - 1) / page * page;

        let margin = (units as u128).saturating_mul(prefs.cu_margin_bps as u128) / 10_000;
        let cu = (units as u128).saturating_add(margin).min(1_400_000) as u32;
        (cu, loaded_rounded, BudgetMode::Auto)
    };

    // Tip: `0` = off · `u64::MAX` = network sample (capped) · else fixed lamports.
    let tip = resolve_priority_fee(rpc, writable_hint, cu, &prefs, jupiter_tip_lamports)?;

    Ok(TxBudget {
        mode,
        compute_unit_limit: cu,
        priority_fee_lamports: tip,
        loaded_accounts_data_size_limit: loaded_rounded,
        heap_size: prefs.heap_size.max(64 * 1024),
        cu_margin_bps: prefs.cu_margin_bps,
        tip_floor_lamports: prefs.tip_floor_lamports,
        tip_ceiling_lamports: prefs.tip_ceiling_lamports,
    })
}

fn resolve_priority_fee(
    rpc: &RpcClient,
    writable_hint: &[Pubkey],
    cu: u32,
    prefs: &TxBudget,
    jupiter_tip_lamports: Option<u64>,
) -> Result<u64> {
    match prefs.priority_fee_lamports {
        0 => Ok(0),
        u64::MAX => {
            let tip_from_rpc = estimate_tip_lamports(rpc, writable_hint, cu)?;
            let mut tip = tip_from_rpc
                .max(prefs.tip_floor_lamports)
                .max(jupiter_tip_lamports.unwrap_or(0));
            if prefs.tip_ceiling_lamports > 0 {
                tip = tip.min(prefs.tip_ceiling_lamports);
            }
            Ok(tip)
        }
        fixed => Ok(fixed),
    }
}

fn estimate_tip_lamports(rpc: &RpcClient, accounts: &[Pubkey], cu: u32) -> Result<u64> {
    // Prefer a **network-wide** sample. Passing every writable account (pool vaults,
    // tick arrays, …) biases toward arb-bot tips on hot accounts and blows up fees.
    // Keep at most one optional hint (e.g. pool); otherwise empty = all recent txs.
    let sample: &[Pubkey] = match accounts.first() {
        Some(pk) => std::slice::from_ref(pk),
        None => &[],
    };
    let fees = rpc
        .get_recent_prioritization_fees(sample)
        .unwrap_or_default();
    if fees.is_empty() {
        return Ok(5_000);
    }
    // Drop zero-fee slots so idle samples don't crush the percentile to 0.
    let mut micros: Vec<u64> = fees
        .iter()
        .map(|f| f.prioritization_fee)
        .filter(|m| *m > 0)
        .collect();
    if micros.is_empty() {
        return Ok(5_000);
    }
    micros.sort_unstable();
    // ~50th percentile of non-zero fees (was 75th — too aggressive on congested pools).
    let idx = (micros.len() / 2).min(micros.len() - 1);
    let micro_per_cu = micros[idx];
    // v1 tip is **total lamports** = micro_lamports/CU × CU / 1e6
    let tip = (micro_per_cu as u128).saturating_mul(cu as u128) / 1_000_000;
    Ok(tip.min(u64::MAX as u128) as u64)
}

/// Build with auto budget (unless Manual), always simulate, then send.
pub fn prepare_estimate_simulate_send(
    rpc: &RpcClient,
    payer: &Keypair,
    instructions: &[Instruction],
    prefs: TxBudget,
    writable_hint: &[Pubkey],
    jupiter_tip_lamports: Option<u64>,
) -> Result<SendOutcome> {
    let blockhash = rpc.get_latest_blockhash().context("get_latest_blockhash")?;
    let budget = estimate_budget(
        rpc,
        payer,
        instructions,
        blockhash,
        prefs,
        writable_hint,
        jupiter_tip_lamports,
    )?;
    let prepared = prepare_v1_tx(payer, instructions, blockhash, budget)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    simulate_and_send_v1(rpc, &prepared)
}

/// Auto-budget + simulate + send, signing with `extra` (position mint, etc.).
pub fn prepare_estimate_simulate_send_signers(
    rpc: &RpcClient,
    payer: &Keypair,
    extra: &[&Keypair],
    instructions: &[Instruction],
    prefs: TxBudget,
    writable_hint: &[Pubkey],
    jupiter_tip_lamports: Option<u64>,
) -> Result<SendOutcome> {
    let blockhash = rpc.get_latest_blockhash().context("get_latest_blockhash")?;
    let budget = estimate_budget_signers(
        rpc,
        payer,
        extra,
        instructions,
        blockhash,
        prefs,
        writable_hint,
        jupiter_tip_lamports,
    )?;
    let prepared = prepare_v1_tx_signers(payer, extra, instructions, blockhash, budget)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    simulate_and_send_v1(rpc, &prepared)
}

#[derive(Debug, Clone)]
pub struct SendOutcome {
    pub signature: String,
    pub simulation_logs: Vec<String>,
    pub budget: TxBudget,
    /// On-chain tx fee (base + priority) when RPC returned meta; else estimated tip + sig rent.
    pub fee_lamports: u64,
    /// Wallet SOL delta across send (fee + rent + wraps). `None` if balances unavailable.
    pub sol_spent_lamports: Option<u64>,
}

impl SendOutcome {
    /// Human summary for confirm UI / status line.
    pub fn paid_label(&self) -> String {
        let tip = self.budget.priority_fee_lamports;
        let fee = self.fee_lamports;
        let fee_sol = fee as f64 / 1e9;
        match self.sol_spent_lamports {
            Some(spent) => format!(
                "tx fee {:.6} SOL ({} lamports · tip {}) · wallet −{:.6} SOL (fee+rent+deposit)",
                fee_sol,
                fee,
                tip,
                spent as f64 / 1e9
            ),
            None => format!("tx fee {:.6} SOL ({} lamports · tip {})", fee_sol, fee, tip),
        }
    }
}

/// Always simulate first; only send if simulation succeeds.
/// Confirmation failures (timeout / dropped) surface as errors — caller should offer retry
/// (fresh blockhash + re-sim) rather than resending the same expired tx.
pub fn simulate_and_send_v1(rpc: &RpcClient, prepared: &PreparedTx) -> Result<SendOutcome> {
    let simulation_logs = simulate_v1_tx(rpc, prepared)?;
    let payer = prepared
        .transaction
        .message
        .static_account_keys()
        .first()
        .copied();
    let bal_before = payer.and_then(|pk| rpc.get_balance(&pk).ok());
    let sig = rpc
        .send_and_confirm_transaction(&prepared.transaction)
        .context("confirmation failed · retry rebuilds with a fresh blockhash")?;
    let bal_after = payer.and_then(|pk| rpc.get_balance(&pk).ok());
    let sol_spent_lamports = match (bal_before, bal_after) {
        (Some(b), Some(a)) if b >= a => Some(b - a),
        _ => None,
    };
    let n_sigs = prepared.transaction.signatures.len().max(1) as u64;
    let estimated_fee = prepared
        .budget
        .priority_fee_lamports
        .saturating_add(5_000u64.saturating_mul(n_sigs));
    let fee_lamports = {
        use solana_client::rpc_config::RpcTransactionConfig;
        let cfg = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::Json),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(1),
            min_context_slot: None,
        };
        rpc.get_transaction_with_config(&sig, cfg)
            .ok()
            .and_then(|tx| tx.transaction.meta.map(|m| m.fee))
            .unwrap_or(estimated_fee)
    };
    Ok(SendOutcome {
        signature: sig.to_string(),
        simulation_logs,
        budget: prepared.budget,
        fee_lamports,
        sol_spent_lamports,
    })
}
