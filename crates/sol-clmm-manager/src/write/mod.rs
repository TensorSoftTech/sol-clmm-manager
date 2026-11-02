//! Write-path types and helpers (feature `write` only).
//!
//! - Transactions are always **SIMD-0385 v1** (`VersionedMessage::V1`) — never legacy/v0.
//! - Resource limits live in `v1::TransactionConfig` (CU / priority fee / loaded data / heap).
//! - Every send path **simulates first** (base64); send is refused if simulation fails.

#![cfg(feature = "write")]

mod liquidity;
mod sol_wrap;
mod swap;
mod tx;

#[cfg(test)]
mod pool_scenario_tests;

pub mod dlmm_strategy;

pub use dlmm_strategy::{
    BinWeights, DlmmStrategy, MeteoraStrategyType, auto_fill_x, auto_fill_y, balanced_weight_totals,
    balanced_weights, bin_raw_price, clamp_balanced_amounts, distribute_bins,
    dlmm_swap_to_match_hint,
};
pub use liquidity::{
    CompoundFeePlan, DepositAnalysis, DepositShape, DepositSizing, LiquidityDeltaPlan,
    RangeDepositSide, SwapToMatchHint, amounts_for_liquidity_delta, analyze_deposit,
    compound_fee_plan, deposit_ratio_planner, effective_deposit_shape, liquidity_delta_for_open,
    liquidity_delta_from_amounts, range_deposit_side, swap_to_match_hint,
};
pub use sol_wrap::{
    SolLegMode, associated_token_program_id, create_ata_idempotent_ix, dedupe_ata_create_idempotent,
    ensure_atas_ixs, ensure_pool_atas_ixs, is_wsol, maybe_unwrap_after, maybe_wrap_before_deposit,
    pool_has_wsol, unwrap_sol_ix, wrap_sol_ixs, wsol_ata,
};
pub use swap::{SwapMode, SwapPlan, SwapQuote, fetch_swap_instructions};
pub use tx::{
    BudgetMode, PreparedTx, SendOutcome, TxBudget, TxBuildError, estimate_budget,
    estimate_budget_signers, prepare_estimate_simulate_send,
    prepare_estimate_simulate_send_signers, prepare_v1_tx, prepare_v1_tx_signers,
    simulate_and_send_v1, simulate_v1_tx,
};

use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

/// High-level mutate the TUI / Client can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOp {
    ClaimFeesRewards,
    Withdraw,
    /// Claim fees+rewards then withdraw all liquidity — keep position open.
    ClaimAndWithdraw,
    /// Claim fees+rewards + withdraw all liquidity + close position — **one tx**.
    CloseAndClaim,
    Deposit,
    DepositWithSwap,
    Compound,
    /// Open a new position; user supplies pool pubkey (+ range / amounts).
    CreatePosition,
    /// Native SOL → WSOL ATA (`wrap_sol_ixs`) — wallet standalone.
    WrapSol,
    /// Close WSOL ATA → native SOL (`unwrap_sol_ix`) — wallet standalone.
    UnwrapSol,
    /// Free-form Jupiter/Titan swap (wallet management).
    WalletSwap,
    /// Initialize a new pool / lb-pair (optional seed deposit + swap).
    CreatePool,
}

impl WriteOp {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClaimFeesRewards => "claim fees + rewards",
            Self::Withdraw => "withdraw liquidity",
            Self::ClaimAndWithdraw => "claim + withdraw",
            Self::CloseAndClaim => "close · claim + withdraw + close account",
            Self::Deposit => "deposit",
            Self::DepositWithSwap => "deposit + swap",
            Self::Compound => "claim + deposit (compound)",
            Self::CreatePosition => "create position",
            Self::WrapSol => "wrap SOL",
            Self::UnwrapSol => "unwrap SOL",
            Self::WalletSwap => "wallet swap",
            Self::CreatePool => "create pool",
        }
    }
}

/// Standalone wrap: create WSOL ATA + transfer + SyncNative.
pub fn plan_wrap_sol(payer: Pubkey, lamports: u64) -> WritePlan {
    WritePlan {
        op: WriteOp::WrapSol,
        venue_label: "system + SPL".into(),
        summary: format!("wrap {} lamports → WSOL", lamports),
        instructions: wrap_sol_ixs(&payer, lamports),
        follow_up: None,
        expected: ExpectedDeltas {
            note: format!("native SOL −{lamports} · WSOL +{lamports}"),
            ..Default::default()
        },
        payer,
    }
}

/// Standalone unwrap: CloseAccount on the WSOL ATA.
pub fn plan_unwrap_sol(payer: Pubkey) -> WritePlan {
    WritePlan {
        op: WriteOp::UnwrapSol,
        venue_label: "SPL token".into(),
        summary: "unwrap WSOL ATA → native SOL".into(),
        instructions: vec![unwrap_sol_ix(&payer)],
        follow_up: None,
        expected: ExpectedDeltas {
            note: "close WSOL ATA · lamports → wallet".into(),
            ..Default::default()
        },
        payer,
    }
}

/// Inputs for [`WriteOp::CreatePosition`] (pool is required from the user).
///
/// Prefer packing **wrap → swap → open → deposit** into **one tx v1** (4096 B).
/// Only split to a follow-up leg if the assembled message exceeds the size limit.
#[derive(Debug, Clone)]
pub struct CreatePositionParams {
    pub venue: crate::Venue,
    pub pool: Pubkey,
    pub tick_lower: i32,
    pub tick_upper: i32,
    /// Raw token A (ignored / 0 when [`DepositShape::SingleB`]).
    pub amount_a: u64,
    /// Raw token B (ignored / 0 when [`DepositShape::SingleA`]).
    pub amount_b: u64,
    /// Dual-sided vs single-sided open.
    pub shape: DepositShape,
    pub sizing: DepositSizing,
    pub sol_leg: SolLegMode,
    /// DLMM only.
    pub dlmm_strategy: Option<DlmmStrategy>,
    /// If true, include Jupiter/Titan swap ixs in the **same** tx v1 to match
    /// in-range deposit ratio (single-sided budget → both sides, or rebalance dual).
    pub swap_to_match: bool,
    /// Slippage for the optional swap (bps).
    pub swap_slippage_bps: u16,
    /// Display symbols for summaries (e.g. SOL / USDC). Optional.
    pub symbol_a: Option<String>,
    pub symbol_b: Option<String>,
}

impl CreatePositionParams {
    pub fn shape_label(&self) -> String {
        match (&self.symbol_a, &self.symbol_b) {
            (Some(a), Some(b)) => self.shape.label_with(a, b),
            _ => self.shape.label().to_string(),
        }
    }
}

/// How much liquidity to seed when creating a pool from Wallet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreatePoolMode {
    /// Initialize pool / lb-pair only.
    #[default]
    PoolOnly,
    /// Init + open position + deposit (no Jupiter).
    PoolDeposit,
    /// Init + swap-to-match + open + deposit.
    PoolDepositSwap,
}

impl CreatePoolMode {
    pub const ALL: [CreatePoolMode; 3] = [Self::PoolOnly, Self::PoolDeposit, Self::PoolDepositSwap];

    pub fn label(self) -> &'static str {
        match self {
            Self::PoolOnly => "pool only",
            Self::PoolDeposit => "pool + deposit",
            Self::PoolDepositSwap => "pool + deposit + swap",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::PoolOnly => Self::PoolDeposit,
            Self::PoolDeposit => Self::PoolDepositSwap,
            Self::PoolDepositSwap => Self::PoolOnly,
        }
    }
}

/// Inputs for [`WriteOp::CreatePool`] (wallet management).
#[derive(Debug, Clone)]
pub struct CreatePoolParams {
    pub venue: crate::Venue,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    /// Orca tick spacing / Raydium fee-tier index / Meteora bin step.
    pub fee_param: u16,
    /// Initial human-ish price as B per A (raw ratio uses decimals at assemble).
    pub initial_price: f64,
    pub mode: CreatePoolMode,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub amount_a: u64,
    pub amount_b: u64,
    pub shape: DepositShape,
    pub dlmm_strategy: Option<DlmmStrategy>,
    pub swap_slippage_bps: u16,
    pub sol_leg: SolLegMode,
    pub symbol_a: Option<String>,
    pub symbol_b: Option<String>,
    pub decimals_a: u8,
    pub decimals_b: u8,
}

/// Expected token movement shown in the confirm screen (raw units).
#[derive(Debug, Clone, Default)]
pub struct ExpectedDeltas {
    pub amount_a: i128,
    pub amount_b: i128,
    pub fee_a: u64,
    pub fee_b: u64,
    pub note: String,
}

/// Bundle of instructions ready for tx v1 + simulate + confirm.
#[derive(Debug, Clone)]
pub struct WritePlan {
    pub op: WriteOp,
    pub venue_label: String,
    pub summary: String,
    pub instructions: Vec<Instruction>,
    /// Optional second leg (e.g. deposit after swap) — each leg is sim+confirm separately.
    pub follow_up: Option<Box<WritePlan>>,
    pub expected: ExpectedDeltas,
    pub payer: Pubkey,
}
