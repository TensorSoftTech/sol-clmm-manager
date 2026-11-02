//! Multi-venue write facade (feature `write`). Always builds **tx v1**, estimates
//! CU/tip when budget mode is Auto, simulates, then sends.

#![cfg(feature = "write")]

use anyhow::{Context, Result, bail};
use sol_clmm_manager::store::AppSettings;
use sol_clmm_manager::{
    BudgetMode, CreatePoolParams, CreatePositionParams, DepositShape, DepositSizing, DlmmStrategy,
    ExpectedDeltas, LiquidityDeltaPlan, PositionDetail, PreparedTx, SendOutcome, SolLegMode,
    SwapMode, SwapPlan, TxBudget, Venue, WriteOp, WritePlan, amounts_for_liquidity_delta,
    clamp_balanced_amounts, compound_fee_plan, dedupe_ata_create_idempotent, dlmm_swap_to_match_hint,
    estimate_budget_signers, fetch_swap_instructions, is_wsol, liquidity_delta_for_open,
    liquidity_delta_from_amounts, maybe_unwrap_after, maybe_wrap_before_deposit, pool_has_wsol,
    prepare_estimate_simulate_send, prepare_estimate_simulate_send_signers, prepare_v1_tx_signers,
    simulate_and_send_v1, simulate_v1_tx, swap_to_match_hint, wrap_sol_ixs,
};
use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::Client;

/// DLMM create autoswap context (strategy-weight ratio instead of CLMM √P).
struct DlmmCreateSwapCtx {
    active_id: i32,
    bin_step: u16,
    strategy: DlmmStrategy,
}

/// Native SOL to wrap from the wallet before deposit — 0 when the WSOL side is unfunded
/// (e.g. single-side USDC on SOL/USDC). Swap output arrives as WSOL via the router
/// (`wrapAndUnwrapSol=false`); we unwrap/close the ATA after the funded create.
fn wrap_lamports_from_wallet(
    params: &CreatePositionParams,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
) -> u64 {
    if params.sol_leg != SolLegMode::NativeSol {
        return 0;
    }
    if is_wsol(mint_a) {
        params.amount_a
    } else if is_wsol(mint_b) {
        params.amount_b
    } else {
        0
    }
}

/// Close WSOL ATA → native SOL after create when `sol_leg` is NativeSol.
/// Shared by Orca / Raydium / Meteora — same suffix for all venues.
fn finish_native_sol(
    mut plan: WritePlan,
    authority: Pubkey,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
    sol_leg: SolLegMode,
) -> WritePlan {
    plan.instructions
        .extend(maybe_unwrap_after(sol_leg, &authority, mint_a, mint_b));
    plan
}

/// Post-swap A/B budgets from the router quote (falls back to wallet amounts).
fn budgets_after_swap(
    params: &CreatePositionParams,
    mint_a: &Pubkey,
    swap: &Option<SwapPlan>,
) -> (u128, u128) {
    let a = u128::from(params.amount_a);
    let b = u128::from(params.amount_b);
    let Some(s) = swap else {
        return (a, b);
    };
    let q = &s.quote;
    if q.in_mint == *mint_a {
        (
            a.saturating_sub(u128::from(q.in_amount)),
            b.saturating_add(u128::from(q.out_amount)),
        )
    } else {
        (
            a.saturating_add(u128::from(q.out_amount)),
            b.saturating_sub(u128::from(q.in_amount)),
        )
    }
}

fn token_maxes(
    params: &CreatePositionParams,
    plan_l: &LiquidityDeltaPlan,
    after_swap: bool,
    post_swap_budget: Option<(u128, u128)>,
) -> (u64, u64) {
    let pa = plan_l.amount_a.min(u128::from(u64::MAX)) as u64;
    let pb = plan_l.amount_b.min(u128::from(u64::MAX)) as u64;
    if after_swap {
        // Full post-swap wallet as tokenMax (L haircut in size_create covers ceil).
        match post_swap_budget {
            Some((ba, bb)) => (
                (ba.min(u128::from(u64::MAX)) as u64).max(pa),
                (bb.min(u128::from(u64::MAX)) as u64).max(pb),
            ),
            None => (
                pa.saturating_add(pa / 50).saturating_add(2),
                pb.saturating_add(pb / 50).saturating_add(2),
            ),
        }
    } else {
        // User budgets are the hard cap; L−3% keeps on-chain ceil under these.
        (params.amount_a.max(pa), params.amount_b.max(pb))
    }
}

/// Shrink ΔL so on-chain ceil(token deltas) still fit under tokenMax (0x1781).
fn haircut_liquidity_for_ceil(
    mut plan: LiquidityDeltaPlan,
    sqrt_price: u128,
    tick_lower: i32,
    tick_upper: i32,
) -> LiquidityDeltaPlan {
    if plan.liquidity_delta < 2 {
        return plan;
    }
    let cut = plan.liquidity_delta.saturating_mul(97) / 100;
    if cut == 0 || cut >= plan.liquidity_delta {
        return plan;
    }
    let (a, b) = amounts_for_liquidity_delta(cut, sqrt_price, tick_lower, tick_upper);
    plan.note = format!("L−3% ceil-buffer · {}", plan.note);
    plan.liquidity_delta = cut;
    plan.amount_a = a;
    plan.amount_b = b;
    plan
}

fn size_create_liquidity(
    params: &CreatePositionParams,
    sqrt_price: u128,
    mint_a: &Pubkey,
    swap: &Option<SwapPlan>,
) -> LiquidityDeltaPlan {
    let plan = if swap.is_some() {
        let (ba, bb) = budgets_after_swap(params, mint_a, swap);
        // Haircut quote budgets: router outAmount can be optimistic vs execution,
        // and our ΔL math can undershoot on-chain token needs (TokenMaxExceeded).
        let ba = ba.saturating_mul(97) / 100;
        let bb = bb.saturating_mul(97) / 100;
        let mut plan = liquidity_delta_from_amounts(
            ba,
            bb,
            sqrt_price,
            params.tick_lower,
            params.tick_upper,
            DepositShape::Dual,
        );
        plan.note = format!("post-swap (−3%) · {}", plan.note);
        plan
    } else {
        liquidity_delta_for_open(
            u128::from(params.amount_a),
            u128::from(params.amount_b),
            sqrt_price,
            params.tick_lower,
            params.tick_upper,
            params.shape,
            false,
        )
    };
    haircut_liquidity_for_ceil(plan, sqrt_price, params.tick_lower, params.tick_upper)
}

/// Refuse open-only creates when the user funded amounts but ΔL came out 0.
fn ensure_create_liquidity(params: &CreatePositionParams, plan: &LiquidityDeltaPlan) -> Result<()> {
    if params.amount_a == 0 && params.amount_b == 0 {
        bail!("create needs amount A and/or B (or swap-to-match)");
    }
    if plan.liquidity_delta == 0 {
        let hint = if !plan.note.is_empty() && plan.note != "ΔL 0 · needs ~0 A + 0 B" {
            plan.note.as_str()
        } else {
            match params.shape {
                DepositShape::Dual => {
                    "dual in-range needs both sides · try single-side when price is outside range, \
                     enable swap-to-match, or raise amounts"
                }
                DepositShape::SingleA => {
                    "single-A only works when price is below the range (token A only) · \
                     enable swap-to-match or move range above current price"
                }
                DepositShape::SingleB => {
                    "single-B only works when price is above the range (token B only) · \
                     enable swap-to-match or move range below current price"
                }
            }
        };
        bail!(
            "computed liquidity is 0 · {hint} (got A={} B={} ticks [{}, {}])",
            params.amount_a,
            params.amount_b,
            params.tick_lower,
            params.tick_upper
        );
    }
    Ok(())
}

impl Client {
    pub fn tx_budget_from_settings(settings: &AppSettings) -> TxBudget {
        let mut b = TxBudget::default();
        if let Some(ref m) = settings.tx_budget_mode {
            b.mode = BudgetMode::from_setting(m);
        }
        if let Some(v) = settings.tx_cu_limit {
            b.compute_unit_limit = v;
        }
        if let Some(v) = settings.tx_tip_lamports {
            // `0` = off · `u64::MAX` = auto sample · else fixed tip.
            b.priority_fee_lamports = v;
        }
        if let Some(v) = settings.tx_loaded_data {
            b.loaded_accounts_data_size_limit = v;
        }
        if let Some(v) = settings.tx_heap {
            b.heap_size = v;
        }
        if let Some(v) = settings.tx_cu_margin_bps {
            b.cu_margin_bps = v;
        }
        if let Some(v) = settings.tx_tip_floor {
            b.tip_floor_lamports = v;
        }
        if let Some(v) = settings.tx_tip_ceiling {
            b.tip_ceiling_lamports = v;
        }
        b
    }

    pub fn swap_mode_from_settings(settings: &AppSettings) -> SwapMode {
        settings
            .swap_mode
            .as_deref()
            .map(SwapMode::from_setting)
            .unwrap_or_default()
    }

    pub fn dlmm_strategy_from_settings(settings: &AppSettings) -> DlmmStrategy {
        DlmmStrategy::from_setting(settings.dlmm_strategy.as_deref().unwrap_or("CurveBalanced"))
    }

    /// Plan claim fees + rewards for the position’s venue.
    /// SOL pools: unwrap WSOL → native SOL at the end.
    pub fn plan_claim(&self, authority: Pubkey, detail: &PositionDetail) -> Result<WritePlan> {
        let mut plan = match detail.row.venue {
            Venue::OrcaWhirlpool => orca_whirlpool::write::plan_claim(authority, detail)?,
            Venue::RaydiumClmm => raydium_clmm::write::plan_claim(authority, detail)?,
            Venue::MeteoraDlmm => meteora_dlmm::write::plan_claim(authority, detail)?,
        };
        plan.instructions.extend(maybe_unwrap_after(
            SolLegMode::NativeSol,
            &authority,
            &detail.token_a.mint,
            &detail.token_b.mint,
        ));
        Ok(plan)
    }

    pub fn plan_deposit(
        &self,
        authority: Pubkey,
        detail: &PositionDetail,
        _shape: DepositShape,
        _sizing: DepositSizing,
        strategy: Option<DlmmStrategy>,
        sol_leg: SolLegMode,
    ) -> Result<WritePlan> {
        // Legacy/compound stub — prefer [`Self::assemble_manage_deposit`].
        let max_a = detail.amount_a.min(u64::MAX as u128) as u64;
        let max_b = detail.amount_b.min(u64::MAX as u128) as u64;
        let liq = detail.position_liquidity.max(1) / 10;
        match detail.row.venue {
            Venue::MeteoraDlmm => meteora_dlmm::write::plan_deposit_with_strategy(
                authority,
                detail,
                strategy.unwrap_or_default(),
                max_a,
                max_b,
                sol_leg,
            ),
            Venue::OrcaWhirlpool => {
                orca_whirlpool::write::plan_deposit(authority, detail, liq, max_a, max_b, sol_leg)
            }
            Venue::RaydiumClmm => {
                raydium_clmm::write::plan_deposit(authority, detail, liq, max_a, max_b, sol_leg)
            }
        }
    }

    /// Manage → deposit: size ΔL from budgets · 1-side always swap-to-match · L−3% ceil buffer.
    /// Returns `(plan, optional router tip)`.
    pub fn assemble_manage_deposit(
        &self,
        authority: Pubkey,
        detail: &PositionDetail,
        amount_a: u64,
        amount_b: u64,
        shape: DepositShape,
        swap_to_match: bool,
        swap_slippage_bps: u16,
        sol_leg: SolLegMode,
        settings: &AppSettings,
        strategy: Option<DlmmStrategy>,
    ) -> Result<(WritePlan, Option<u64>)> {
        anyhow::ensure!(amount_a > 0 || amount_b > 0, "deposit needs amount A and/or B");
        let sqrt = detail.pool.sqrt_price;
        let lower = detail.row.tick_lower;
        let upper = detail.row.tick_upper;
        let mint_a = detail.token_a.mint;
        let mint_b = detail.token_b.mint;

        // Out-of-range matching 1-side: hint is None → no Jupiter (even if flag set).
        let swap = if swap_to_match {
            let shape_eff = sol_clmm_manager::effective_deposit_shape(
                u128::from(amount_a),
                u128::from(amount_b),
                shape,
            );
            let hint = if sqrt > 0 {
                swap_to_match_hint(amount_a, amount_b, sqrt, lower, upper, shape_eff)
            } else {
                // DLMM Balanced: strategy-weight autofill (not half-swap).
                let strat = strategy.unwrap_or_default();
                let bin_step = match detail.pool.index_space {
                    sol_clmm_manager::IndexSpace::Bin { bin_step } => bin_step,
                    _ => 0,
                };
                if bin_step == 0 {
                    None
                } else {
                    dlmm_swap_to_match_hint(
                        amount_a,
                        amount_b,
                        detail.pool.tick_current_index,
                        bin_step,
                        lower,
                        upper,
                        strat,
                        shape_eff,
                    )
                }
            };
            match hint {
                Some(h) if h.swap_in_amount > 0 => {
                    let (in_mint, out_mint) = if h.in_is_a {
                        (mint_a, mint_b)
                    } else {
                        (mint_b, mint_a)
                    };
                    Some(self.fetch_swap_for_match(
                        settings,
                        &authority,
                        &in_mint,
                        &out_mint,
                        h.swap_in_amount,
                        swap_slippage_bps,
                    )?)
                }
                _ => None,
            }
        } else {
            None
        };

        // Full post-swap wallets (tokenMax) vs conservative sizing budgets.
        // Same split as create: size L from haircut, tokenMax = full quote budgets
        // so Orca ceil(deltas) still fits (0x1781 TokenMaxExceeded).
        let (full_a, full_b, size_a, size_b) = if let Some(ref s) = swap {
            let q = &s.quote;
            let (raw_a, raw_b) = if q.in_mint == mint_a {
                (
                    u128::from(amount_a).saturating_sub(u128::from(q.in_amount)),
                    u128::from(amount_b).saturating_add(u128::from(q.out_amount)),
                )
            } else {
                (
                    u128::from(amount_a).saturating_add(u128::from(q.out_amount)),
                    u128::from(amount_b).saturating_sub(u128::from(q.in_amount)),
                )
            };
            // 10% buffer: quote out can be optimistic vs execution + floor→ceil gap.
            let size_a = raw_a.saturating_mul(90) / 100;
            let size_b = raw_b.saturating_mul(90) / 100;
            (raw_a, raw_b, size_a, size_b)
        } else {
            let a = u128::from(amount_a);
            let b = u128::from(amount_b);
            (a, b, a, b)
        };

        let mut plan_l = if swap.is_some() {
            let mut p = liquidity_delta_from_amounts(
                size_a,
                size_b,
                sqrt.max(1),
                lower,
                upper,
                DepositShape::Dual,
            );
            p.note = format!("post-swap size (−10%) · {}", p.note);
            p
        } else {
            liquidity_delta_for_open(size_a, size_b, sqrt.max(1), lower, upper, shape, false)
        };
        plan_l = haircut_liquidity_for_ceil(plan_l, sqrt.max(1), lower, upper);
        anyhow::ensure!(
            plan_l.liquidity_delta > 0,
            "computed ΔL is 0 · {}",
            plan_l.note
        );

        let max_a = if swap.is_some() {
            full_a.min(u128::from(u64::MAX)) as u64
        } else {
            amount_a.max(plan_l.amount_a.min(u128::from(u64::MAX)) as u64)
        };
        let max_b = if swap.is_some() {
            full_b.min(u128::from(u64::MAX)) as u64
        } else {
            amount_b.max(plan_l.amount_b.min(u128::from(u64::MAX)) as u64)
        };

        // Venue deposit without wrap/unwrap — we own SOL wrap (prefix) + unwrap (suffix).
        // Jupiter wrapAndUnwrapSol=false; wrap before swap, unwrap after deposit.
        let mut plan = match detail.row.venue {
            Venue::MeteoraDlmm => {
                let strat = strategy.unwrap_or_default();
                let (dep_a, dep_b) = match detail.pool.index_space {
                    sol_clmm_manager::IndexSpace::Bin { bin_step } if strat.is_balanced() => {
                        clamp_balanced_amounts(
                            max_a,
                            max_b,
                            strat,
                            detail.pool.tick_current_index,
                            bin_step,
                            lower,
                            upper,
                        )
                    }
                    _ => (max_a, max_b),
                };
                meteora_dlmm::write::plan_deposit_with_strategy(
                    authority,
                    detail,
                    strat,
                    dep_a,
                    dep_b,
                    SolLegMode::Wsol,
                )?
            }
            Venue::OrcaWhirlpool => orca_whirlpool::write::plan_deposit(
                authority,
                detail,
                plan_l.liquidity_delta,
                max_a,
                max_b,
                SolLegMode::Wsol,
            )?,
            Venue::RaydiumClmm => raydium_clmm::write::plan_deposit(
                authority,
                detail,
                plan_l.liquidity_delta,
                max_a,
                max_b,
                SolLegMode::Wsol,
            )?,
        };
        if swap.is_some() {
            plan.op = WriteOp::DepositWithSwap;
        }

        let wrap_amt = if sol_leg == SolLegMode::NativeSol {
            if is_wsol(&mint_a) {
                amount_a
            } else if is_wsol(&mint_b) {
                amount_b
            } else {
                0
            }
        } else {
            0
        };
        let wrap = maybe_wrap_before_deposit(
            sol_leg,
            &authority,
            &mint_a,
            &mint_b,
            wrap_amt,
        );
        let wrap_n = wrap.len();
        if wrap_n > 0 {
            let mut merged = wrap;
            merged.append(&mut plan.instructions);
            plan.instructions = merged;
        }
        let (mut plan, tip) = splice_swap(plan, wrap_n, swap);
        if sol_leg == SolLegMode::NativeSol {
            plan.instructions.extend(maybe_unwrap_after(
                SolLegMode::NativeSol,
                &authority,
                &mint_a,
                &mint_b,
            ));
        }
        Ok((plan, tip))
    }

    pub fn plan_withdraw(
        &self,
        authority: Pubkey,
        detail: &PositionDetail,
        liquidity_delta: u128,
        sol_leg: SolLegMode,
    ) -> Result<WritePlan> {
        match detail.row.venue {
            Venue::OrcaWhirlpool => orca_whirlpool::write::plan_withdraw(
                authority,
                detail,
                liquidity_delta,
                0,
                0,
                sol_leg,
            ),
            Venue::RaydiumClmm => {
                raydium_clmm::write::plan_withdraw(authority, detail, liquidity_delta, sol_leg)
            }
            Venue::MeteoraDlmm => {
                // DLMM remove uses per-bin bps (1..=10_000), not raw L.
                let bps = if detail.position_liquidity == 0 {
                    10_000u16
                } else {
                    ((liquidity_delta.saturating_mul(10_000) / detail.position_liquidity)
                        .min(10_000)) as u16
                };
                anyhow::ensure!(bps >= 1, "withdraw bps is 0");
                meteora_dlmm::write::plan_withdraw(authority, detail, bps, sol_leg)
            }
        }
    }

    /// Claim fees (+ rewards) → redeposit into the same position.
    ///
    /// - `with_swap = false`: on-ratio slice of fees only; residual stays in wallet.
    /// - `with_swap = true`: Dual + swap-to-match on full fee budgets (residual swapped in).
    ///
    /// Fees land in ATAs first (no mid-flow unwrap); SOL unwrap once at the end.
    /// Returns `(plan, optional router tip)`.
    pub fn assemble_compound(
        &self,
        authority: Pubkey,
        detail: &PositionDetail,
        with_swap: bool,
        swap_slippage_bps: u16,
        settings: &AppSettings,
    ) -> Result<(WritePlan, Option<u64>)> {
        let fee_a = detail.fee_a;
        let fee_b = detail.fee_b;
        anyhow::ensure!(
            fee_a > 0 || fee_b > 0,
            "no claimable fees to compound"
        );

        // Venue claim only — Client::plan_claim would unwrap before deposit.
        let claim = match detail.row.venue {
            Venue::OrcaWhirlpool => orca_whirlpool::write::plan_claim(authority, detail)?,
            Venue::RaydiumClmm => raydium_clmm::write::plan_claim(authority, detail)?,
            Venue::MeteoraDlmm => meteora_dlmm::write::plan_claim(authority, detail)?,
        };

        let strategy = Some(Self::dlmm_strategy_from_settings(settings));
        // Fees are already in ATAs after claim (WSOL if SOL pool) — don't wrap native SOL.
        let (amount_a, amount_b, shape, do_swap) = if with_swap {
            (
                fee_a,
                fee_b,
                DepositShape::Dual,
                true,
            )
        } else {
            let split = compound_fee_plan(
                fee_a,
                fee_b,
                detail.pool.sqrt_price,
                detail.row.tick_lower,
                detail.row.tick_upper,
                detail.pool.tick_current_index,
            );
            anyhow::ensure!(
                split.deposit_a > 0 || split.deposit_b > 0,
                "fees cannot form a deposit · {}",
                split.note
            );
            (split.deposit_a, split.deposit_b, split.shape, false)
        };

        let (mut deposit, tip) = self.assemble_manage_deposit(
            authority,
            detail,
            amount_a,
            amount_b,
            shape,
            do_swap,
            swap_slippage_bps,
            SolLegMode::Wsol,
            settings,
            strategy,
        )?;

        // One tx: claim → (swap?) → increase L → unwrap SOL if pool has WSOL.
        let mut ixs = claim.instructions;
        ixs.append(&mut deposit.instructions);
        if pool_has_wsol(&detail.token_a.mint, &detail.token_b.mint) {
            ixs.extend(maybe_unwrap_after(
                SolLegMode::NativeSol,
                &authority,
                &detail.token_a.mint,
                &detail.token_b.mint,
            ));
        }

        let mode = if with_swap {
            "with-swap"
        } else {
            "no-swap"
        };
        let plan = WritePlan {
            op: WriteOp::Compound,
            venue_label: claim.venue_label,
            summary: format!(
                "compound {mode} · claim → deposit {amount_a}/{amount_b} · {}",
                deposit.summary
            ),
            instructions: ixs,
            follow_up: None,
            expected: ExpectedDeltas {
                amount_a: -(i128::from(amount_a)),
                amount_b: -(i128::from(amount_b)),
                fee_a,
                fee_b,
                note: format!("claim fees → redeposit ({mode})"),
            },
            payer: authority,
        };
        Ok((plan, tip))
    }

    /// Legacy stub — prefer [`Self::assemble_compound`].
    pub fn plan_compound(
        &self,
        authority: Pubkey,
        detail: &PositionDetail,
        with_swap: bool,
        settings: &AppSettings,
    ) -> Result<WritePlan> {
        Ok(self
            .assemble_compound(authority, detail, with_swap, 50, settings)?
            .0)
    }

    /// Claim fees+rewards + withdraw `liquidity_delta` — keep position account.
    /// DLMM maps ΔL → per-bin bps (same % across bins).
    pub fn plan_claim_and_withdraw(
        &self,
        authority: Pubkey,
        detail: &PositionDetail,
        liquidity_delta: u128,
        sol_leg: SolLegMode,
    ) -> Result<WritePlan> {
        let liq = liquidity_delta.min(detail.position_liquidity);
        match detail.row.venue {
            Venue::OrcaWhirlpool => orca_whirlpool::write::plan_claim_and_withdraw(
                authority,
                detail,
                liq,
                sol_leg,
            ),
            Venue::RaydiumClmm | Venue::MeteoraDlmm => {
                // Venue claim (ATAs, no unwrap yet) → withdraw (ATAs + decrease + unwrap).
                // Do not use Client::plan_claim — that would unwrap before withdraw.
                let mut claim = match detail.row.venue {
                    Venue::RaydiumClmm => raydium_clmm::write::plan_claim(authority, detail)?,
                    Venue::MeteoraDlmm => meteora_dlmm::write::plan_claim(authority, detail)?,
                    Venue::OrcaWhirlpool => unreachable!(),
                };
                if liq > 0 {
                    let withdraw = self.plan_withdraw(authority, detail, liq, sol_leg)?;
                    claim.instructions.extend(withdraw.instructions);
                    let pct = if detail.position_liquidity > 0 {
                        (liq.saturating_mul(100) / detail.position_liquidity).min(100)
                    } else {
                        0
                    };
                    claim.summary = format!("claim + withdraw {pct}% · ΔL={liq}");
                } else {
                    claim.instructions.extend(maybe_unwrap_after(
                        sol_leg,
                        &authority,
                        &detail.token_a.mint,
                        &detail.token_b.mint,
                    ));
                    claim.summary = "claim only · no liquidity".into();
                }
                claim.op = WriteOp::ClaimAndWithdraw;
                claim.expected.fee_a = detail.fee_a;
                claim.expected.fee_b = detail.fee_b;
                claim.expected.note =
                    "ensure ATAs → claim → withdraw → unwrap? · position stays".into();
                Ok(claim)
            }
        }
    }

    /// Claim fees+rewards + withdraw full L + close position in **one** tx v1.
    /// Optional native-SOL unwrap is appended in the same tx when `sol_leg` says so.
    pub fn plan_close_and_claim(
        &self,
        authority: Pubkey,
        detail: &PositionDetail,
        sol_leg: SolLegMode,
    ) -> Result<WritePlan> {
        match detail.row.venue {
            Venue::OrcaWhirlpool => {
                orca_whirlpool::write::plan_close_and_claim(authority, detail, sol_leg)
            }
            Venue::RaydiumClmm => {
                raydium_clmm::write::plan_close_and_claim(authority, detail, sol_leg)
            }
            Venue::MeteoraDlmm => {
                meteora_dlmm::write::plan_close_and_claim(authority, detail, sol_leg)
            }
        }
    }

    /// Open a new position. Caller must supply [`CreatePositionParams::pool`].
    ///
    /// Shape: dual or single-sided. Optional `swap_to_match` packs swap ixs into the
    /// **same** tx v1 as wrap + open + deposit when size allows.
    pub fn plan_create_position(
        &self,
        authority: Pubkey,
        params: &CreatePositionParams,
        settings: &AppSettings,
    ) -> Result<WritePlan> {
        anyhow::ensure!(
            params.pool != Pubkey::default(),
            "create position requires a pool pubkey from the user"
        );
        anyhow::ensure!(
            params.tick_lower < params.tick_upper,
            "tick/bin lower must be < upper"
        );
        match params.shape {
            DepositShape::Dual => {
                anyhow::ensure!(
                    params.amount_a > 0 || params.amount_b > 0 || params.swap_to_match,
                    "dual open needs amounts and/or swap_to_match"
                );
            }
            DepositShape::SingleA => {
                anyhow::ensure!(params.amount_a > 0, "single-A open needs amount_a");
            }
            DepositShape::SingleB => {
                anyhow::ensure!(params.amount_b > 0, "single-B open needs amount_b");
            }
        }

        let mut plan = match params.venue {
            Venue::OrcaWhirlpool => orca_whirlpool::write::plan_create_position(authority, params)?,
            Venue::RaydiumClmm => raydium_clmm::write::plan_create_position(authority, params)?,
            Venue::MeteoraDlmm => meteora_dlmm::write::plan_create_position(authority, params)?,
        };

        // Swap ixs are appended into the same plan (1 tx v1). Concrete mints/amounts
        // are resolved when the pool is decoded at assemble/send time.
        if params.swap_to_match {
            let _mode = Self::swap_mode_from_settings(settings);
            plan.summary = format!(
                "{} · +swap({}) in same tx v1",
                plan.summary,
                Self::swap_mode_from_settings(settings).short()
            );
            plan.expected.note = format!(
                "{} · wrap? → swap-to-match → open → deposit (1 tx v1)",
                plan.expected.note
            );
        }
        Ok(plan)
    }

    /// Fetch pool state and build a **funded** create plan:
    /// wrap? → **swap-to-match?** → open → deposit (one tx v1).
    /// `position_mint` must also sign (Orca/Raydium NFT mint or Meteora position).
    pub fn assemble_create_position(
        &self,
        rpc: &RpcClient,
        authority: Pubkey,
        params: &CreatePositionParams,
        position_mint: Pubkey,
        settings: &AppSettings,
    ) -> Result<(WritePlan, Option<u64>)> {
        let pool_acc = rpc
            .get_account(&params.pool)
            .with_context(|| format!("fetch pool {}", params.pool))?;

        match params.venue {
            Venue::OrcaWhirlpool => {
                let full = orca_whirlpool::decode::decode_whirlpool_full(
                    params.pool,
                    &pool_acc.data,
                )?;
                let swap = if params.swap_to_match {
                    self.swap_for_create(
                        settings,
                        &authority,
                        full.stats.token_mint_a,
                        full.stats.token_mint_b,
                        params,
                        Some(full.stats.sqrt_price),
                        None,
                    )?
                } else {
                    None
                };
                let plan_l = size_create_liquidity(
                    params,
                    full.stats.sqrt_price,
                    &full.stats.token_mint_a,
                    &swap,
                );
                ensure_create_liquidity(params, &plan_l)?;
                let post = swap
                    .as_ref()
                    .map(|_| budgets_after_swap(params, &full.stats.token_mint_a, &swap));
                let (max_a, max_b) = token_maxes(params, &plan_l, swap.is_some(), post);
                let mut plan = orca_whirlpool::write::plan_create_funded(
                    authority,
                    params,
                    position_mint,
                    full.stats.token_mint_a,
                    full.stats.token_mint_b,
                    full.stats.token_vault_a,
                    full.stats.token_vault_b,
                    full.stats.tick_spacing,
                    plan_l.liquidity_delta,
                    max_a,
                    max_b,
                )?;
                let wrap = maybe_wrap_before_deposit(
                    params.sol_leg,
                    &authority,
                    &full.stats.token_mint_a,
                    &full.stats.token_mint_b,
                    wrap_lamports_from_wallet(
                        params,
                        &full.stats.token_mint_a,
                        &full.stats.token_mint_b,
                    ),
                );
                let wrap_n = wrap.len();
                if wrap_n > 0 {
                    let mut merged = wrap;
                    merged.append(&mut plan.instructions);
                    plan.instructions = merged;
                }
                let (plan, tip) = splice_swap(plan, wrap_n, swap);
                Ok((
                    finish_native_sol(
                        plan,
                        authority,
                        &full.stats.token_mint_a,
                        &full.stats.token_mint_b,
                        params.sol_leg,
                    ),
                    tip,
                ))
            }
            Venue::RaydiumClmm => {
                let header = raydium_clmm::decode::decode_pool_header(&pool_acc.data)?;
                let swap = if params.swap_to_match {
                    self.swap_for_create(
                        settings,
                        &authority,
                        header.mint0,
                        header.mint1,
                        params,
                        Some(header.sqrt_price),
                        None,
                    )?
                } else {
                    None
                };
                let plan_l =
                    size_create_liquidity(params, header.sqrt_price, &header.mint0, &swap);
                ensure_create_liquidity(params, &plan_l)?;
                let post = swap
                    .as_ref()
                    .map(|_| budgets_after_swap(params, &header.mint0, &swap));
                let (max_a, max_b) = token_maxes(params, &plan_l, swap.is_some(), post);
                let mut plan = raydium_clmm::write::plan_create_funded(
                    authority,
                    params,
                    position_mint,
                    header.mint0,
                    header.mint1,
                    header.vault0,
                    header.vault1,
                    header.tick_spacing,
                    plan_l.liquidity_delta,
                    max_a,
                    max_b,
                )?;
                let wrap = maybe_wrap_before_deposit(
                    params.sol_leg,
                    &authority,
                    &header.mint0,
                    &header.mint1,
                    wrap_lamports_from_wallet(params, &header.mint0, &header.mint1),
                );
                let wrap_n = wrap.len();
                if wrap_n > 0 {
                    let mut merged = wrap;
                    merged.append(&mut plan.instructions);
                    plan.instructions = merged;
                }
                let (plan, tip) = splice_swap(plan, wrap_n, swap);
                Ok((
                    finish_native_sol(
                        plan,
                        authority,
                        &header.mint0,
                        &header.mint1,
                        params.sol_leg,
                    ),
                    tip,
                ))
            }
            Venue::MeteoraDlmm => {
                let header = meteora_dlmm::decode::decode_lb_pair_header(&pool_acc.data)?;
                if params.amount_a == 0 && params.amount_b == 0 {
                    bail!("DLMM create needs amount X and/or Y");
                }
                let strategy = params.dlmm_strategy.unwrap_or_else(|| {
                    Self::dlmm_strategy_from_settings(settings)
                });
                let swap = if params.swap_to_match {
                    self.swap_for_create(
                        settings,
                        &authority,
                        header.mint_x,
                        header.mint_y,
                        params,
                        None,
                        Some(DlmmCreateSwapCtx {
                            active_id: header.active_id,
                            bin_step: header.bin_step,
                            strategy,
                        }),
                    )?
                } else {
                    None
                };
                let (amount_x, amount_y) = if let Some(ref s) = swap {
                    let (ba, bb) = budgets_after_swap(params, &header.mint_x, &swap);
                    let _ = s;
                    (ba.min(u128::from(u64::MAX)) as u64, bb.min(u128::from(u64::MAX)) as u64)
                } else {
                    (params.amount_a, params.amount_b)
                };
                // Balanced scales to the limiting side — clamp caps so leftover isn't "lost".
                let (amount_x, amount_y) = if strategy.is_balanced() {
                    clamp_balanced_amounts(
                        amount_x,
                        amount_y,
                        strategy,
                        header.active_id,
                        header.bin_step,
                        params.tick_lower,
                        params.tick_upper,
                    )
                } else {
                    (amount_x, amount_y)
                };
                let mut plan = meteora_dlmm::write::plan_create_funded(
                    authority,
                    params,
                    position_mint,
                    header.mint_x,
                    header.mint_y,
                    header.reserve_x,
                    header.reserve_y,
                    amount_x,
                    amount_y,
                    header.active_id,
                )?;
                let wrap = maybe_wrap_before_deposit(
                    params.sol_leg,
                    &authority,
                    &header.mint_x,
                    &header.mint_y,
                    wrap_lamports_from_wallet(params, &header.mint_x, &header.mint_y),
                );
                let wrap_n = wrap.len();
                if wrap_n > 0 {
                    let mut merged = wrap;
                    merged.append(&mut plan.instructions);
                    plan.instructions = merged;
                }
                let (plan, tip) = splice_swap(plan, wrap_n, swap);
                Ok((
                    finish_native_sol(
                        plan,
                        authority,
                        &header.mint_x,
                        &header.mint_y,
                        params.sol_leg,
                    ),
                    tip,
                ))
            }
        }
    }

    fn swap_for_create(
        &self,
        settings: &AppSettings,
        user: &Pubkey,
        mint_a: Pubkey,
        mint_b: Pubkey,
        params: &CreatePositionParams,
        sqrt_price: Option<u128>,
        dlmm: Option<DlmmCreateSwapCtx>,
    ) -> Result<Option<SwapPlan>> {
        let shape = sol_clmm_manager::effective_deposit_shape(
            params.amount_a as u128,
            params.amount_b as u128,
            params.shape,
        );
        let hint = if let Some(sp) = sqrt_price {
            swap_to_match_hint(
                params.amount_a,
                params.amount_b,
                sp,
                params.tick_lower,
                params.tick_upper,
                shape,
            )
        } else if let Some(ctx) = dlmm {
            dlmm_swap_to_match_hint(
                params.amount_a,
                params.amount_b,
                ctx.active_id,
                ctx.bin_step,
                params.tick_lower,
                params.tick_upper,
                ctx.strategy,
                shape,
            )
        } else {
            None
        };
        let Some(hint) = hint else {
            return Ok(None);
        };
        let (in_mint, out_mint) = if hint.in_is_a {
            (mint_a, mint_b)
        } else {
            (mint_b, mint_a)
        };
        Ok(Some(self.fetch_swap_for_match(
            settings,
            user,
            &in_mint,
            &out_mint,
            hint.swap_in_amount,
            params.swap_slippage_bps,
        )?))
    }

    /// Estimate budget (auto) or use manual, simulate, send. Refuses empty ix lists.
    pub fn execute_plan(
        &self,
        rpc: &RpcClient,
        payer: &Keypair,
        plan: &WritePlan,
        settings: &AppSettings,
        jupiter_tip: Option<u64>,
    ) -> Result<SendOutcome> {
        self.execute_plan_signers(rpc, payer, &[], plan, settings, jupiter_tip)
    }

    /// Same as [`Self::execute_plan`] with extra signers (create position mint).
    pub fn execute_plan_signers(
        &self,
        rpc: &RpcClient,
        payer: &Keypair,
        extra: &[&Keypair],
        plan: &WritePlan,
        settings: &AppSettings,
        jupiter_tip: Option<u64>,
    ) -> Result<SendOutcome> {
        if plan.instructions.is_empty() {
            bail!(
                "{}: instruction accounts not fully assembled yet — rebuild with filled metas",
                plan.op.label()
            );
        }
        let prefs = Self::tx_budget_from_settings(settings);
        // Empty hints → network-wide priority fee sample (avoids hot-vault arb tips).
        let hints: &[Pubkey] = &[];
        if extra.is_empty() {
            prepare_estimate_simulate_send(
                rpc,
                payer,
                &plan.instructions,
                prefs,
                hints,
                jupiter_tip,
            )
        } else {
            prepare_estimate_simulate_send_signers(
                rpc,
                payer,
                extra,
                &plan.instructions,
                prefs,
                hints,
                jupiter_tip,
            )
        }
        .with_context(|| format!("execute {}", plan.op.label()))
    }

    /// Prepare + simulate only (no send) — for confirm UI.
    pub fn prepare_and_simulate(
        &self,
        rpc: &RpcClient,
        payer: &Keypair,
        plan: &WritePlan,
        settings: &AppSettings,
        jupiter_tip: Option<u64>,
    ) -> Result<(PreparedTx, Vec<String>)> {
        self.prepare_and_simulate_signers(rpc, payer, &[], plan, settings, jupiter_tip)
    }

    pub fn prepare_and_simulate_signers(
        &self,
        rpc: &RpcClient,
        payer: &Keypair,
        extra: &[&Keypair],
        plan: &WritePlan,
        settings: &AppSettings,
        jupiter_tip: Option<u64>,
    ) -> Result<(PreparedTx, Vec<String>)> {
        if plan.instructions.is_empty() {
            bail!("{}: no instructions to simulate", plan.op.label());
        }
        let prefs = Self::tx_budget_from_settings(settings);
        let blockhash = rpc.get_latest_blockhash()?;
        let hints: &[Pubkey] = &[];
        let budget = estimate_budget_signers(
            rpc,
            payer,
            extra,
            &plan.instructions,
            blockhash,
            prefs,
            hints,
            jupiter_tip,
        )?;
        let prepared = prepare_v1_tx_signers(payer, extra, &plan.instructions, blockhash, budget)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let logs = simulate_v1_tx(rpc, &prepared)?;
        Ok((prepared, logs))
    }

    pub fn send_prepared(&self, rpc: &RpcClient, prepared: &PreparedTx) -> Result<SendOutcome> {
        // Re-simulate immediately before send (always on).
        simulate_and_send_v1(rpc, prepared)
    }

    pub fn fetch_swap_for_match(
        &self,
        settings: &AppSettings,
        user: &Pubkey,
        input_mint: &Pubkey,
        output_mint: &Pubkey,
        amount: u64,
        slippage_bps: u16,
    ) -> Result<sol_clmm_manager::SwapPlan> {
        let mode = Self::swap_mode_from_settings(settings);
        let key = match mode {
            SwapMode::Jupiter => settings.jupiter_api_key.as_deref(),
            SwapMode::Titan => settings.titan_api_key.as_deref(),
        };
        fetch_swap_instructions(
            mode,
            key,
            user,
            input_mint,
            output_mint,
            amount,
            slippage_bps,
        )
    }

    /// Free-form wallet swap: optional wrap → Jupiter/Titan → optional unwrap.
    ///
    /// When `input_is_native_sol`, `amount` is lamports wrapped into WSOL before the route.
    /// When either mint is WSOL, close the WSOL ATA at the end (native SOL out).
    pub fn assemble_wallet_swap(
        &self,
        settings: &AppSettings,
        authority: Pubkey,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
        slippage_bps: u16,
        input_is_native_sol: bool,
    ) -> Result<(WritePlan, Option<u64>)> {
        anyhow::ensure!(amount > 0, "swap amount must be > 0");
        anyhow::ensure!(input_mint != output_mint, "from and to mints must differ");
        let route_in = if input_is_native_sol {
            sol_clmm_manager::wsol_mint()
        } else {
            input_mint
        };
        let swap = self.fetch_swap_for_match(
            settings,
            &authority,
            &route_in,
            &output_mint,
            amount,
            slippage_bps,
        )?;
        let mut ixs = Vec::new();
        let mut wrap_n = 0usize;
        if input_is_native_sol {
            let wrap = wrap_sol_ixs(&authority, amount);
            wrap_n = wrap.len();
            ixs.extend(wrap);
        }
        ixs.extend(swap.instructions);
        // Always unwrap leftover WSOL after wallet swaps involving SOL.
        if is_wsol(&route_in) || is_wsol(&output_mint) {
            ixs.push(sol_clmm_manager::unwrap_sol_ix(&authority));
        }
        let tip = swap.quote.tip_lamports;
        let mut plan = WritePlan {
            op: WriteOp::WalletSwap,
            venue_label: swap.quote.router.short().into(),
            summary: format!(
                "swap {} → {} · {} in · ~{} out",
                if input_is_native_sol {
                    "SOL".into()
                } else {
                    input_mint.to_string()
                },
                output_mint,
                swap.quote.in_amount,
                swap.quote.out_amount
            ),
            instructions: ixs,
            follow_up: None,
            expected: ExpectedDeltas {
                note: format!(
                    "{} · {} → {} · wrap_prefix={wrap_n}",
                    swap.quote.router.short(),
                    swap.quote.in_amount,
                    swap.quote.out_amount
                ),
                ..Default::default()
            },
            payer: authority,
        };
        dedupe_ata_create_idempotent(&mut plan.instructions);
        Ok((plan, tip))
    }

    /// Create pool (+ optional deposit / swap). Extra signers = Orca vault keypairs.
    pub fn assemble_create_pool(
        &self,
        _rpc: &RpcClient,
        authority: Pubkey,
        params: &CreatePoolParams,
        _settings: &AppSettings,
    ) -> Result<(WritePlan, Option<u64>, Vec<Keypair>)> {
        match params.venue {
            Venue::OrcaWhirlpool => {
                let vault_a = Keypair::new();
                let vault_b = Keypair::new();
                let plan = orca_whirlpool::write::plan_create_pool(
                    authority,
                    params,
                    vault_a.pubkey(),
                    vault_b.pubkey(),
                )?;
                Ok((plan, None, vec![vault_a, vault_b]))
            }
            Venue::RaydiumClmm => {
                let plan = raydium_clmm::write::plan_create_pool(authority, params)?;
                Ok((plan, None, Vec::new()))
            }
            Venue::MeteoraDlmm => {
                let plan = meteora_dlmm::write::plan_create_pool(authority, params)?;
                Ok((plan, None, Vec::new()))
            }
        }
    }
}

/// Insert swap ixs after wrap prefix · returns `(plan, tip_lamports)`.
fn splice_swap(
    mut plan: WritePlan,
    wrap_prefix: usize,
    swap: Option<SwapPlan>,
) -> (WritePlan, Option<u64>) {
    let tip = match swap {
        None => {
            dedupe_ata_create_idempotent(&mut plan.instructions);
            return (plan, None);
        }
        Some(swap) => {
            let tip = swap.quote.tip_lamports;
            let insert_at = wrap_prefix.min(plan.instructions.len());
            let mut merged =
                Vec::with_capacity(plan.instructions.len() + swap.instructions.len());
            merged.extend(plan.instructions.drain(..insert_at));
            merged.extend(swap.instructions);
            merged.append(&mut plan.instructions);
            plan.instructions = merged;
            plan.summary = format!(
                "{} · +{} swap ({})",
                plan.summary,
                swap.quote.router.short(),
                swap.quote.raw_note
            );
            plan.expected.note = format!(
                "{} · {} in→out {}→{}",
                plan.expected.note,
                swap.quote.router.short(),
                swap.quote.in_amount,
                swap.quote.out_amount
            );
            tip
        }
    };
    // Wrap + Jupiter/Titan setup both emit WSOL CreateIdempotent — keep one.
    dedupe_ata_create_idempotent(&mut plan.instructions);
    (plan, tip)
}

/// Convenience: writes enabled + consent + keypair present.
pub fn write_session_ok(settings: &AppSettings, has_keypair: bool) -> bool {
    sol_clmm_manager::WRITE_ENABLED && has_keypair && settings.write_allowed == Some(true)
}
