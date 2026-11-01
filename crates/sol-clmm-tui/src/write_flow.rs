//! Create-position wizard + write confirm / retry (feature `write`).

use sol_clmm_manager::{
    CreatePositionParams, DepositAnalysis, DepositShape, DepositSizing, DlmmStrategy, PoolPreview,
    RangeDepositSide, SolLegMode, Venue, WalletPoolBalances, WritePlan, analyze_deposit,
    format_amount, format_price, is_wsol, parse_ui_amount, pool_has_wsol, range_deposit_side,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateStep {
    Venue,
    Pool,
    /// Pool fetched · liquidity chart · ←→ nudge · 1/2/3 = ±5/10/25% presets.
    Range,
    /// After ticks: 2-side or 1-side (which token) · type amount · % presets.
    Deposit,
    /// Jupiter/Titan swap · router + slippage bps (only when autoswap on).
    SwapSlippage,
    /// Only when the user is funding the WSOL mint from the wallet (not USDC→swap).
    SolLeg,
    DlmmStrategy,
    Review,
}

/// Fixed Jupiter / Titan `slippageBps` presets (0.10% … 5%).
pub const SWAP_SLIPPAGE_PRESETS: [u16; 5] = [10, 50, 100, 300, 500];

/// Sentinel for Jupiter RTSE (`slippageBps=rtse` on `/build`).
pub const SWAP_SLIPPAGE_RTSE: u16 = u16::MAX;

/// One row on the autoswap slippage step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlippagePick {
    /// Jupiter RTSE · Titan falls back to 50 bps.
    Recommended,
    /// Fixed 50 bps (Jupiter `/build` default when not using RTSE).
    Default50,
    Fixed(u16),
}

impl SlippagePick {
    pub fn bps(self) -> u16 {
        match self {
            Self::Recommended => SWAP_SLIPPAGE_RTSE,
            Self::Default50 => 50,
            Self::Fixed(b) => b,
        }
    }

    pub fn label(self, router: &str) -> String {
        match self {
            Self::Recommended if router == "jupiter" => {
                "use recommended · Jupiter RTSE (auto)".into()
            }
            Self::Recommended => "use recommended · Titan → 50 bps".into(),
            Self::Default50 => format!(
                "use default · {}",
                CreateWizard::slippage_label(50)
            ),
            Self::Fixed(b) => CreateWizard::slippage_label(b),
        }
    }
}

/// 2-side vs 1-side (pick token). Our plus vs web UIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepositMode {
    /// Both tokens · type primary · auto-fill the other.
    Dual,
    /// Token A only · autoswap if range needs both.
    SingleA,
    /// Token B only · autoswap if range needs both.
    SingleB,
}

impl DepositMode {
    pub const ALL: [DepositMode; 3] = [DepositMode::Dual, DepositMode::SingleA, DepositMode::SingleB];

    pub fn from_idx(i: usize) -> Self {
        Self::ALL[i % 3]
    }

    pub fn label(self, sa: &str, sb: &str) -> String {
        match self {
            Self::Dual => format!("2-side · {sa} + {sb}"),
            Self::SingleA => format!("1-side · {sa} only"),
            Self::SingleB => format!("1-side · {sb} only"),
        }
    }

    pub fn to_shape(self) -> DepositShape {
        match self {
            Self::Dual => DepositShape::Dual,
            Self::SingleA => DepositShape::SingleA,
            Self::SingleB => DepositShape::SingleB,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeFocus {
    Lower,
    Upper,
}

impl RangeFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::Lower => Self::Upper,
            Self::Upper => Self::Lower,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Lower => "lower",
            Self::Upper => "upper",
        }
    }
}

#[derive(Debug)]
pub struct CreateWizard {
    pub step: CreateStep,
    pub venue_idx: usize,
    pub pool_input: String,
    pub preview: Option<PoolPreview>,
    pub range_focus: RangeFocus,
    pub shape: DepositShape,
    pub amount_a_input: String,
    pub amount_b_input: String,
    /// Primary amount typed on the Deposit step.
    pub deposit_amount_input: String,
    pub swap_to_match: bool,
    /// `jupiter` | `titan` — same as settings; overridable on SwapSlippage.
    pub swap_router: String,
    /// Index into [`Self::slippage_picks`] (recommended / default / fixed).
    pub swap_slippage_idx: usize,
    pub sol_leg: SolLegMode,
    pub dlmm_idx: usize,
    pub error: Option<String>,
    pub choice: usize,
    pub fetching: bool,
}

impl Default for CreateWizard {
    fn default() -> Self {
        Self::new()
    }
}

impl CreateWizard {
    pub fn new() -> Self {
        Self {
            step: CreateStep::Venue,
            venue_idx: 0,
            pool_input: String::new(),
            preview: None,
            range_focus: RangeFocus::Lower,
            shape: DepositShape::Dual,
            amount_a_input: String::new(),
            amount_b_input: String::new(),
            deposit_amount_input: String::new(),
            swap_to_match: false,
            swap_router: "jupiter".into(),
            swap_slippage_idx: 0, // recommended
            sol_leg: SolLegMode::Wsol,
            dlmm_idx: 0,
            error: None,
            choice: 0,
            fetching: false,
        }
    }

    pub fn with_swap_router(mut self, mode: &str) -> Self {
        self.swap_router = if mode.eq_ignore_ascii_case("titan") {
            "titan".into()
        } else {
            "jupiter".into()
        };
        self
    }

    pub fn deposit_mode(&self) -> DepositMode {
        DepositMode::from_idx(self.choice)
    }

    /// Rows: recommended · default 50 · fixed presets (skip duplicate 50).
    pub fn slippage_picks(&self) -> Vec<SlippagePick> {
        let mut out = vec![SlippagePick::Recommended, SlippagePick::Default50];
        for &b in &SWAP_SLIPPAGE_PRESETS {
            if b != 50 {
                out.push(SlippagePick::Fixed(b));
            }
        }
        out
    }

    pub fn swap_slippage_pick(&self) -> SlippagePick {
        let picks = self.slippage_picks();
        picks[self.swap_slippage_idx % picks.len()]
    }

    pub fn swap_slippage_bps(&self) -> u16 {
        self.swap_slippage_pick().bps()
    }

    pub fn slippage_label(bps: u16) -> String {
        if bps == SWAP_SLIPPAGE_RTSE {
            "Jupiter RTSE · auto".into()
        } else {
            format!("{bps} bps · {:.2}%", f64::from(bps) / 100.0)
        }
    }

    pub fn slippage_choice_label(&self) -> String {
        self.swap_slippage_pick().label(&self.swap_router)
    }

    pub fn cycle_swap_router(&mut self) {
        self.swap_router = if self.swap_router == "titan" {
            "jupiter".into()
        } else {
            "titan".into()
        };
    }

    pub fn venue(&self) -> Venue {
        Venue::ALL[self.venue_idx.min(2)]
    }

    pub fn dlmm_strategy(&self) -> DlmmStrategy {
        match self.dlmm_idx % 5 {
            1 => DlmmStrategy::BidAsk,
            2 => DlmmStrategy::SpotConcentrated,
            3 => DlmmStrategy::SpotSpread,
            4 => DlmmStrategy::SpotWide,
            _ => DlmmStrategy::Curve,
        }
    }

    pub fn dlmm_label(idx: usize) -> &'static str {
        match idx % 5 {
            1 => "bid-ask",
            2 => "spot-tight",
            3 => "spot-spread",
            4 => "spot-wide",
            _ => "curve",
        }
    }

    pub fn step_title(&self) -> String {
        match self.step {
            CreateStep::Venue => "venue".into(),
            CreateStep::Pool => "pool pubkey".into(),
            CreateStep::Range => "range · chart".into(),
            CreateStep::Deposit => "side · amount".into(),
            CreateStep::SwapSlippage => "swap · slippage".into(),
            CreateStep::SolLeg => "SOL wrap".into(),
            CreateStep::DlmmStrategy => "DLMM strategy".into(),
            CreateStep::Review => "review · create".into(),
        }
    }

    /// Index into the wizard breadcrumb (0 = venue … 4 = review).
    pub fn progress_index(&self) -> usize {
        match self.step {
            CreateStep::Venue => 0,
            CreateStep::Pool => 1,
            CreateStep::Range => 2,
            CreateStep::Deposit
            | CreateStep::SwapSlippage
            | CreateStep::SolLeg
            | CreateStep::DlmmStrategy => 3,
            CreateStep::Review => 4,
        }
    }

    pub const PROGRESS_STAGES: [&'static str; 5] =
        ["venue", "pool", "range", "side", "review"];

    pub fn needs_amount_a(&self) -> bool {
        matches!(self.shape, DepositShape::Dual | DepositShape::SingleA)
    }

    pub fn needs_amount_b(&self) -> bool {
        matches!(self.shape, DepositShape::Dual | DepositShape::SingleB)
    }

    fn sym_pair(&self) -> (&str, &str) {
        match &self.preview {
            Some(p) => (p.token_a.symbol.as_str(), p.token_b.symbol.as_str()),
            None => ("A", "B"),
        }
    }

    fn use_native_sol(&self) -> bool {
        if self.sol_leg == SolLegMode::NativeSol {
            return true;
        }
        // Dual typed on USDC (etc.): include native SOL in pair autofill max without asking wrap.
        self.step == CreateStep::Deposit
            && self.deposit_mode() == DepositMode::Dual
            && !self.funding_wsol_from_wallet()
            && self.pool_has_wsol_mint()
    }

    fn pool_has_wsol_mint(&self) -> bool {
        self.preview
            .as_ref()
            .map(|p| pool_has_wsol(&p.pool.token_mint_a, &p.pool.token_mint_b))
            .unwrap_or(false)
    }

    /// True only when the user is funding the WSOL mint from the wallet.
    /// USDC-only (or dual typed on USDC) → never ask wrap/unwrap.
    fn pool_needs_sol_leg(&self) -> bool {
        self.funding_wsol_from_wallet()
    }

    /// User's typed deposit side is WSOL (not the auto-filled / swapped other side).
    fn funding_wsol_from_wallet(&self) -> bool {
        let Some(p) = &self.preview else {
            return false;
        };
        let a_wsol = is_wsol(&p.pool.token_mint_a);
        let b_wsol = is_wsol(&p.pool.token_mint_b);
        if !a_wsol && !b_wsol {
            return false;
        }
        // Prefer live mode on Deposit; else committed shape + which amount was typed.
        let mode = if self.step == CreateStep::Deposit {
            self.deposit_mode()
        } else {
            match self.shape {
                DepositShape::Dual => DepositMode::Dual,
                DepositShape::SingleA => DepositMode::SingleA,
                DepositShape::SingleB => DepositMode::SingleB,
            }
        };
        match mode {
            DepositMode::SingleA => a_wsol,
            DepositMode::SingleB => b_wsol,
            DepositMode::Dual => {
                // Dual types the non-WSOL side when possible (see dual_primary_is_a).
                if self.dual_primary_is_a() {
                    a_wsol
                } else {
                    b_wsol
                }
            }
        }
    }

    /// Which amount Dual types first — prefer the non-WSOL token on SOL pairs.
    fn dual_primary_is_a(&self) -> bool {
        let Some(p) = &self.preview else {
            return true;
        };
        let a_wsol = is_wsol(&p.pool.token_mint_a);
        let b_wsol = is_wsol(&p.pool.token_mint_b);
        match (a_wsol, b_wsol) {
            (true, false) => false, // type B (e.g. USDC)
            (false, true) => true,  // type A
            _ => true,
        }
    }

    fn max_a(&self) -> Option<u64> {
        self.preview
            .as_ref()?
            .balances
            .as_ref()
            .map(|b| b.max_depositable(true, self.use_native_sol()))
    }

    fn max_b(&self) -> Option<u64> {
        self.preview
            .as_ref()?
            .balances
            .as_ref()
            .map(|b| b.max_depositable(false, self.use_native_sol()))
    }

    fn balance_line(&self) -> Option<String> {
        let p = self.preview.as_ref()?;
        let bal = p.balances.as_ref()?;
        Some(format!(
            "wallet · {}",
            bal.summary_line(
                &p.token_a.symbol,
                &p.token_b.symbol,
                p.token_a.decimals,
                p.token_b.decimals,
            )
        ))
    }

    fn decimals_for(&self, side_a: bool) -> u8 {
        match &self.preview {
            Some(p) if side_a => p.token_a.decimals,
            Some(p) => p.token_b.decimals,
            None => 0,
        }
    }

    fn range_side(&self) -> Option<RangeDepositSide> {
        let p = self.preview.as_ref()?;
        Some(range_deposit_side(
            p.pool.sqrt_price,
            p.tick_lower,
            p.tick_upper,
            p.pool.tick_current_index,
        ))
    }

    /// Raw A/B from current inputs (empty → 0; invalid → None).
    fn raw_amounts_opt(&self) -> Option<(u64, u64)> {
        let a = if self.needs_amount_a() && !self.amount_a_input.trim().is_empty() {
            self.parse_amount_clamped(&self.amount_a_input, true).ok()?
        } else {
            0
        };
        let b = if self.needs_amount_b() && !self.amount_b_input.trim().is_empty() {
            self.parse_amount_clamped(&self.amount_b_input, false).ok()?
        } else {
            0
        };
        Some((a, b))
    }

    /// Shape under the ↑↓ cursor on Deposit; otherwise the committed shape.
    fn analysis_shape(&self) -> DepositShape {
        if self.step == CreateStep::Deposit {
            self.deposit_mode().to_shape()
        } else {
            self.shape
        }
    }

    /// Live A/B while typing on Deposit (autofill pair / zero unused side).
    fn draft_amounts(&self) -> (u64, u64) {
        if self.step != CreateStep::Deposit {
            return self.raw_amounts_opt().unwrap_or((0, 0));
        }
        let ui = self.deposit_amount_input.trim();
        if ui.is_empty() {
            return (0, 0);
        }
        let Some(p) = &self.preview else {
            return (0, 0);
        };
        match self.deposit_mode() {
            DepositMode::Dual if self.dual_primary_is_a() => {
                let Ok(a) = parse_ui_amount(ui, p.token_a.decimals) else {
                    return (0, 0);
                };
                let an = analyze_deposit(
                    a as u128,
                    0,
                    p.pool.sqrt_price,
                    p.tick_lower,
                    p.tick_upper,
                    p.pool.tick_current_index,
                    DepositShape::Dual,
                );
                let b = an
                    .paired_b_for_a
                    .unwrap_or(0)
                    .min(u128::from(self.max_b().unwrap_or(u64::MAX)))
                    .min(u128::from(u64::MAX)) as u64;
                (a, b)
            }
            DepositMode::Dual => {
                let Ok(b) = parse_ui_amount(ui, p.token_b.decimals) else {
                    return (0, 0);
                };
                let an = analyze_deposit(
                    0,
                    b as u128,
                    p.pool.sqrt_price,
                    p.tick_lower,
                    p.tick_upper,
                    p.pool.tick_current_index,
                    DepositShape::Dual,
                );
                let a = an
                    .paired_a_for_b
                    .unwrap_or(0)
                    .min(u128::from(self.max_a().unwrap_or(u64::MAX)))
                    .min(u128::from(u64::MAX)) as u64;
                (a, b)
            }
            DepositMode::SingleA => {
                let Ok(a) = parse_ui_amount(ui, p.token_a.decimals) else {
                    return (0, 0);
                };
                (a, 0)
            }
            DepositMode::SingleB => {
                let Ok(b) = parse_ui_amount(ui, p.token_b.decimals) else {
                    return (0, 0);
                };
                (0, b)
            }
        }
    }

    fn deposit_analysis(&self) -> Option<DepositAnalysis> {
        let p = self.preview.as_ref()?;
        let shape = self.analysis_shape();
        let (a, b) = self.draft_amounts();
        Some(analyze_deposit(
            a as u128,
            b as u128,
            p.pool.sqrt_price,
            p.tick_lower,
            p.tick_upper,
            p.pool.tick_current_index,
            shape,
        ))
    }

    /// Commit Deposit mode + typed amount → shape / amounts / swap_to_match.
    fn commit_deposit(&mut self) -> Result<(), String> {
        let mode = self.deposit_mode();
        let ui = self.deposit_amount_input.trim();
        if ui.is_empty() {
            return Err("enter an amount".into());
        }
        let primary_a = match mode {
            DepositMode::Dual => self.dual_primary_is_a(),
            DepositMode::SingleA => true,
            DepositMode::SingleB => false,
        };
        let primary = self.parse_amount_clamped(ui, primary_a)?;
        self.deposit_amount_input =
            format_amount(primary as u128, self.decimals_for(primary_a));
        let (a, b) = self.draft_amounts();
        if mode == DepositMode::Dual {
            let (other, other_a) = if primary_a { (b, false) } else { (a, true) };
            if let Some(max) = if other_a { self.max_a() } else { self.max_b() }
                && other > max
            {
                let sym = if other_a {
                    self.sym_pair().0
                } else {
                    self.sym_pair().1
                };
                return Err(format!(
                    "auto-fill {sym} > wallet max {} · lower amount or pick 1-side + autoswap",
                    if other_a {
                        self.max_ui(true)
                    } else {
                        self.max_ui(false)
                    },
                ));
            }
        }
        self.shape = mode.to_shape();
        // 1-side in a both-token range → autoswap. Dual never forces swap.
        self.swap_to_match = match mode {
            DepositMode::Dual => false,
            DepositMode::SingleA | DepositMode::SingleB => self
                .deposit_analysis()
                .map(|an| an.swap_needed || !an.shape_ok_without_swap)
                .unwrap_or(true),
        };
        self.amount_a_input = if a > 0 {
            format_amount(a as u128, self.decimals_for(true))
        } else {
            String::new()
        };
        self.amount_b_input = if b > 0 {
            format_amount(b as u128, self.decimals_for(false))
        } else {
            String::new()
        };
        Ok(())
    }

    fn recommended_deposit_idx(&self) -> usize {
        match self.range_side() {
            Some(RangeDepositSide::TokenAOnly) => 1, // SingleA
            Some(RangeDepositSide::TokenBOnly) => 2, // SingleB
            _ => 0,                                  // Dual
        }
    }

    /// Fill deposit amount from wallet % (25 / 50 / 100).
    pub fn apply_amount_pct(&mut self, pct: u8) {
        if self.step != CreateStep::Deposit {
            return;
        }
        let side_a = match self.deposit_mode() {
            DepositMode::Dual => self.dual_primary_is_a(),
            DepositMode::SingleA => true,
            DepositMode::SingleB => false,
        };
        let Some(max) = (if side_a { self.max_a() } else { self.max_b() }) else {
            return;
        };
        let raw = (u128::from(max) * u128::from(pct) / 100).min(u128::from(u64::MAX)) as u64;
        if raw == 0 {
            return;
        }
        self.deposit_amount_input = format_amount(raw as u128, self.decimals_for(side_a));
    }

    /// ±pct price band around current tick (Orca/Raydium-style presets).
    pub fn apply_pct_range(&mut self, pct: f64) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let spacing = p.pool.tick_spacing.max(1);
        let cur = PoolPreview::align_index(p.pool.tick_current_index, spacing);
        let step = spacing as i32;
        let lo_ticks = price_factor_to_ticks(1.0 - pct / 100.0);
        let hi_ticks = price_factor_to_ticks(1.0 + pct / 100.0);
        let mut lo = PoolPreview::align_index(cur + lo_ticks, spacing);
        let mut hi = PoolPreview::align_index(cur + hi_ticks, spacing);
        if lo >= hi {
            lo = cur - step;
            hi = cur + step;
        }
        if lo >= hi {
            hi = lo + step;
        }
        p.apply_range(lo, hi);
    }

    fn fmt_raw(&self, raw: u128, side_a: bool) -> String {
        format_amount(raw, self.decimals_for(side_a))
    }

    /// Guidance lines for range / amounts / swap steps.
    fn deposit_hint_lines(&self) -> Vec<String> {
        let Some(p) = &self.preview else {
            return Vec::new();
        };
        let sa = p.token_a.symbol.as_str();
        let sb = p.token_b.symbol.as_str();
        let Some(an) = self.deposit_analysis() else {
            return Vec::new();
        };
        let mut lines = vec![format!("range · {}", an.side.label(sa, sb))];

        match an.side {
            RangeDepositSide::Both if an.unit_a > 0 && an.unit_b > 0 => {
                // Value split at spot (≠ market FX). Near upper → B%; near lower → A%.
                let one_a = 10u128.saturating_pow(u32::from(p.token_a.decimals));
                let one_b = 10u128.saturating_pow(u32::from(p.token_b.decimals));
                let spot = p.pool.index_space.human_price_at(
                    p.pool.tick_current_index,
                    p.token_a.decimals,
                    p.token_b.decimals,
                );
                let (pct_a, pct_b) = if spot.is_finite() && spot > 0.0 {
                    let va = (an.unit_a as f64 / one_a as f64) * spot;
                    let vb = an.unit_b as f64 / one_b as f64;
                    let sum = va + vb;
                    if sum > 0.0 {
                        (
                            ((va / sum) * 100.0).round() as i32,
                            ((vb / sum) * 100.0).round() as i32,
                        )
                    } else {
                        (0, 0)
                    }
                } else {
                    (0, 0)
                };
                lines.push(format!("mix · ~{pct_a}% {sa} / ~{pct_b}% {sb}"));
            }
            RangeDepositSide::TokenAOnly => {
                lines.push(format!("deposit · {sa} only · {sb} not used at this price"));
            }
            RangeDepositSide::TokenBOnly => {
                lines.push(format!("deposit · {sb} only · {sa} not used at this price"));
            }
            _ => {}
        }

        if let Some(need_b) = an.paired_b_for_a.filter(|&v| v > 0) {
            lines.push(format!(
                "for your {sa} · pair needs ~{} {sb}",
                self.fmt_raw(need_b, false)
            ));
        }
        if let Some(need_a) = an.paired_a_for_b.filter(|&v| v > 0) {
            lines.push(format!(
                "for your {sb} · pair needs ~{} {sa}",
                self.fmt_raw(need_a, true)
            ));
        }

        if self.step == CreateStep::Deposit {
            let mode = self.deposit_mode();
            match mode {
                DepositMode::Dual => {
                    if an.shape_ok_without_swap {
                        lines.push("ok · 2-side auto-fill matches range".into());
                    } else {
                        lines.push(
                            "! range is 1-token · pick matching 1-side or move ticks".into(),
                        );
                    }
                }
                DepositMode::SingleA | DepositMode::SingleB => {
                    if an.swap_needed || !an.shape_ok_without_swap {
                        if let Some(h) = an.swap {
                            let (from, to) = if h.in_is_a { (sa, sb) } else { (sb, sa) };
                            lines.push(format!(
                                "will autoswap · ~{} {from} → {to} · ΔL {}",
                                self.fmt_raw(h.swap_in_amount as u128, h.in_is_a),
                                an.liquidity_with_swap,
                            ));
                        } else {
                            lines.push(format!("will autoswap · {}", an.note));
                        }
                    } else if an.liquidity_no_swap > 0 {
                        lines.push(format!(
                            "ok · 1-side fits · ΔL {}",
                            an.liquidity_no_swap
                        ));
                    }
                }
            }
            if an.liquidity_no_swap > 0 || an.liquidity_with_swap > 0 {
                let (da, db) = self.draft_amounts();
                if da > 0 || db > 0 {
                    lines.push(format!(
                        "spend · {} {sa} + {} {sb}",
                        self.fmt_raw(da as u128, true),
                        self.fmt_raw(db as u128, false),
                    ));
                }
            }
            return lines;
        }

        if !an.shape_ok_without_swap {
            lines.push(format!(
                "! {} cannot fill this range alone · use autoswap deposit mode",
                self.analysis_shape().label_with(sa, sb)
            ));
        }

        if an.swap_needed {
            if let Some(h) = an.swap {
                let (from, to) = if h.in_is_a { (sa, sb) } else { (sb, sa) };
                let from_a = h.in_is_a;
                lines.push(format!(
                    "! imbalance · swap ~{} {from} → {to} · ΔL {} → {}",
                    self.fmt_raw(h.swap_in_amount as u128, from_a),
                    an.liquidity_no_swap,
                    an.liquidity_with_swap,
                ));
            } else if an.shape_ok_without_swap {
                lines.push(format!("! {}", an.note));
            }
        } else if an.liquidity_no_swap > 0 {
            lines.push(format!(
                "ok · ΔL {} · spend ~{} {sa} + {} {sb}",
                an.liquidity_no_swap,
                self.fmt_raw(an.spend_a_no_swap, true),
                self.fmt_raw(an.spend_b_no_swap, false),
            ));
        } else if self.step == CreateStep::Review {
            lines.push(an.note.clone());
        }

        lines
    }

    /// Parse human UI amount → raw, clamp to wallet max.
    fn parse_amount_clamped(&self, ui: &str, side_a: bool) -> Result<u64, String> {
        let sym = if side_a {
            self.sym_pair().0
        } else {
            self.sym_pair().1
        };
        let decimals = self.decimals_for(side_a);
        let amount = parse_ui_amount(ui, decimals)?;
        if let Some(max) = if side_a { self.max_a() } else { self.max_b() }
            && amount > max
        {
            return Err(format!(
                "{sym} {} > wallet max {} · lower amount or change SOL leg",
                format_amount(amount as u128, decimals),
                format_amount(max as u128, decimals),
            ));
        }
        Ok(amount)
    }

    fn max_ui(&self, side_a: bool) -> String {
        let decimals = self.decimals_for(side_a);
        match if side_a { self.max_a() } else { self.max_b() } {
            Some(m) => format_amount(m as u128, decimals),
            None => "?".into(),
        }
    }

    pub fn active_input_mut(&mut self) -> Option<&mut String> {
        match self.step {
            CreateStep::Pool => Some(&mut self.pool_input),
            CreateStep::Deposit => Some(&mut self.deposit_amount_input),
            _ => None,
        }
    }

    /// Header / status lines above the chart (range step draws chart separately).
    /// Lines starting with `▸▸` are the active text field (UI draws caret).
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(err) = &self.error {
            out.push(format!("! {err}"));
        }
        if self.fetching {
            out.push("fetching pool + tick/bin arrays…".into());
        }
        match self.step {
            CreateStep::Venue => {
                out.push("choose CLMM venue · ↑↓ · Enter".into());
                out.push(String::new());
                for (i, v) in Venue::ALL.iter().enumerate() {
                    let mark = if i == self.venue_idx { "▸" } else { " " };
                    out.push(format!("{mark} {}", v.full()));
                }
            }
            CreateStep::Pool => {
                out.push(format!("venue · {}", self.venue().full()));
                out.push(String::new());
                out.push("pool address (base58 pubkey)".into());
                out.push(format!("▸▸{}", self.pool_input));
                out.push(String::new());
                out.push("paste pubkey · Enter fetches pool + ticks · Esc back".into());
                if self.pool_input.trim().is_empty() {
                    out.push("example · 7qbRF6YsyGuLUVs6Y1q64bdVrfe4ZcUUz1JRdoVNUJnm".into());
                } else if self.pool_input.trim().parse::<Pubkey>().is_err() {
                    out.push("! not a valid pubkey yet".into());
                } else {
                    out.push("pubkey ok · Enter to fetch".into());
                }
            }
            CreateStep::Range => {
                if let Some(p) = &self.preview {
                    let idx = p.pool.index_space.label();
                    let fee_bps = p.pool.fee_rate as f64 / 100.0;
                    let px = format_price(p.pool.index_space.human_price_at(
                        p.pool.tick_current_index,
                        p.token_a.decimals,
                        p.token_b.decimals,
                    ));
                    out.push(format!(
                        "{} / {} · fee {fee_bps} bps · spacing {} · {}={} · price {} {}/{}",
                        p.token_a.symbol,
                        p.token_b.symbol,
                        p.pool.tick_spacing,
                        idx.trim_end_matches('s'),
                        p.pool.tick_current_index,
                        px,
                        p.token_b.symbol,
                        p.token_a.symbol,
                    ));
                    out.push(format!(
                        "arrays {} · init {} · range bounds shown as prices on chart",
                        p.arrays_loaded, p.initialized_ticks,
                    ));
                    if let Some(line) = self.balance_line() {
                        out.push(line);
                    }
                    let lo_p = format_price(p.pool.index_space.human_price_at(
                        p.tick_lower,
                        p.token_a.decimals,
                        p.token_b.decimals,
                    ));
                    let hi_p = format_price(p.pool.index_space.human_price_at(
                        p.tick_upper,
                        p.token_a.decimals,
                        p.token_b.decimals,
                    ));
                    out.push(format!(
                        "range [{lo_p} … {hi_p}] {}/{} · ticks [{}, {}] · focus {}",
                        p.token_b.symbol,
                        p.token_a.symbol,
                        p.tick_lower,
                        p.tick_upper,
                        self.range_focus.label(),
                    ));
                    out.push(format!(
                        "←→ nudge · Tab · 1/2/3 ±% · +/- zoom (±{}/{}) · 0 fit · Enter",
                        p.effective_half_spacings(),
                        p.max_chart_half(),
                    ));
                    out.extend(self.deposit_hint_lines());
                    out.push(format!(
                        "┃ = active {} · colored = in range · dim = outside · zoom for SOL/USDC depth",
                        idx.trim_end_matches('s')
                    ));
                } else {
                    out.push("no preview · Esc back".into());
                }
            }
            CreateStep::Deposit => {
                let (sa, sb) = self.sym_pair();
                out.push(format!("↑↓ 2-side or 1-side · type amount · {sa}/{sb}"));
                if let Some(line) = self.balance_line() {
                    out.push(line);
                }
                let rec = self.recommended_deposit_idx();
                for (i, mode) in DepositMode::ALL.iter().enumerate() {
                    let mark = if i == self.choice % 3 { "▸" } else { " " };
                    let tag = if i == rec { " · recommended" } else { "" };
                    out.push(format!("{mark} {}{tag}", mode.label(sa, sb)));
                }
                let side_a = match self.deposit_mode() {
                    DepositMode::Dual => self.dual_primary_is_a(),
                    DepositMode::SingleA => true,
                    DepositMode::SingleB => false,
                };
                let sym = if side_a { sa } else { sb };
                let other = if side_a { sb } else { sa };
                out.push(String::new());
                if self.deposit_mode() == DepositMode::Dual {
                    out.push(format!(
                        "type {sym} · auto-fills {other} · max {} · 4/5/6 = 25%/50%/max",
                        self.max_ui(side_a),
                    ));
                } else {
                    out.push(format!(
                        "amount {sym} · max {} · 4/5/6 = 25%/50%/max",
                        self.max_ui(side_a),
                    ));
                }
                out.push(format!("▸▸{}", self.deposit_amount_input));
                out.push(String::new());
                out.extend(self.deposit_hint_lines());
                if matches!(
                    self.deposit_mode(),
                    DepositMode::SingleA | DepositMode::SingleB
                ) {
                    out.push(format!(
                        "1-side · autoswap if needed · {} · {}",
                        self.swap_router,
                        Self::slippage_label(self.swap_slippage_bps()),
                    ));
                }
                if !self.funding_wsol_from_wallet() && self.pool_has_wsol_mint() {
                    out.push("no SOL wrap prompt · you are not funding WSOL".into());
                }
                out.push("human units e.g. 1.5 · Enter".into());
            }
            CreateStep::SwapSlippage => {
                out.push(format!(
                    "autoswap slippage · router {} · Tab Jupiter ↔ Titan",
                    self.swap_router
                ));
                out.push(
                    "use recommended / default · or change to a fixed bps · ↑↓ · Enter".into(),
                );
                if self.swap_router == "jupiter" {
                    out.push(
                        "Jupiter RTSE estimates optimal slip at quote time (recommended)".into(),
                    );
                } else {
                    out.push(
                        "Titan: no RTSE string · recommended maps to 50 bps fixed".into(),
                    );
                }
                out.push(String::new());
                let picks = self.slippage_picks();
                let cur = self.choice % picks.len();
                for (i, pick) in picks.iter().enumerate() {
                    let mark = if i == cur { "▸" } else { " " };
                    out.push(format!("{mark} {}", pick.label(&self.swap_router)));
                }
            }
            CreateStep::SolLeg => {
                out.push("WSOL pool · fund from WSOL ATA or wrap native SOL".into());
                if let Some(line) = self.balance_line() {
                    out.push(line);
                }
                if let Some(p) = &self.preview
                    && let Some(b) = &p.balances
                {
                    let side_a = b.a.is_wsol;
                    out.push(format!(
                        "max WSOL-only {} · max SOL(−0.01)+WSOL {}",
                        b.max_depositable(side_a, false),
                        b.max_depositable(side_a, true),
                    ));
                }
                out.push("↑↓ · Enter".into());
                out.push(format!(
                    "{} {}",
                    if self.choice.is_multiple_of(2) {
                        "▸"
                    } else {
                        " "
                    },
                    SolLegMode::Wsol.label()
                ));
                out.push(format!(
                    "{} {}",
                    if self.choice % 2 == 1 { "▸" } else { " " },
                    SolLegMode::NativeSol.label()
                ));
            }
            CreateStep::DlmmStrategy => {
                out.push("↑↓ · Enter".into());
                for i in 0..5 {
                    let mark = if i == self.dlmm_idx % 5 { "▸" } else { " " };
                    out.push(format!("{mark} {}", Self::dlmm_label(i)));
                }
            }
            CreateStep::Review => {
                if let Some(p) = &self.preview {
                    let sa = p.token_a.symbol.as_str();
                    let sb = p.token_b.symbol.as_str();
                    out.push(format!(
                        "{} · {}/{} · range [{}, {}] · {}",
                        self.venue().full(),
                        sa,
                        sb,
                        p.tick_lower,
                        p.tick_upper,
                        self.shape.label_with(sa, sb),
                    ));
                    let sol_bit = if self.pool_needs_sol_leg() {
                        format!(" · {}", self.sol_leg.label())
                    } else if self.pool_has_wsol_mint() {
                        " · no SOL wrap (WSOL side unfunded)".into()
                    } else {
                        String::new()
                    };
                    out.push(format!(
                        "{sa} {} · {sb} {} · {}{sol_bit}",
                        if self.amount_a_input.is_empty() {
                            "—"
                        } else {
                            self.amount_a_input.trim()
                        },
                        if self.amount_b_input.is_empty() {
                            "—"
                        } else {
                            self.amount_b_input.trim()
                        },
                        if self.swap_to_match {
                            format!(
                                "autoswap · {} · {}",
                                self.swap_router,
                                self.slippage_choice_label(),
                            )
                        } else {
                            "autoswap off".into()
                        },
                    ));
                    // Show raw conversion for clarity.
                    if let (Ok(ra), Ok(rb)) = (
                        self.parse_amount_clamped(
                            if self.needs_amount_a() {
                                &self.amount_a_input
                            } else {
                                "0"
                            },
                            true,
                        ),
                        self.parse_amount_clamped(
                            if self.needs_amount_b() {
                                &self.amount_b_input
                            } else {
                                "0"
                            },
                            false,
                        ),
                    ) {
                        out.push(format!("raw {sa}={ra} · {sb}={rb}"));
                    }
                    out.extend(self.deposit_hint_lines());
                }
                out.push("Enter → simulate + confirm · Esc back".into());
            }
        }
        out
    }

    /// After pool pubkey: App fetches preview then calls this.
    pub fn on_preview_loaded(&mut self, preview: PoolPreview) {
        self.preview = Some(preview);
        self.fetching = false;
        self.error = None;
        self.range_focus = RangeFocus::Lower;
        self.step = CreateStep::Range;
    }

    pub fn on_preview_failed(&mut self, err: String) {
        self.fetching = false;
        self.preview = None;
        self.error = Some(err);
        self.step = CreateStep::Pool;
    }

    /// Advance wizard. `NeedFetch` when pool entered · `Ready` at Review confirm.
    pub fn on_enter(&mut self) -> CreateAdvance {
        self.error = None;
        match self.step {
            CreateStep::Venue => {
                self.step = CreateStep::Pool;
                CreateAdvance::Continue
            }
            CreateStep::Pool => {
                if self.pool_input.trim().parse::<Pubkey>().is_err() {
                    self.error = Some("invalid pool pubkey".into());
                    return CreateAdvance::Continue;
                }
                self.fetching = true;
                self.preview = None;
                CreateAdvance::NeedFetch
            }
            CreateStep::Range => {
                let Some(p) = &self.preview else {
                    self.error = Some("pool not loaded".into());
                    return CreateAdvance::Continue;
                };
                if p.tick_lower >= p.tick_upper {
                    self.error = Some("lower must be < upper".into());
                    return CreateAdvance::Continue;
                }
                self.step = CreateStep::Deposit;
                self.choice = self.recommended_deposit_idx();
                CreateAdvance::Continue
            }
            CreateStep::Deposit => {
                if let Err(e) = self.commit_deposit() {
                    self.error = Some(e);
                    return CreateAdvance::Continue;
                }
                if self.swap_to_match {
                    self.step = CreateStep::SwapSlippage;
                    let n = self.slippage_picks().len();
                    self.choice = self.swap_slippage_idx % n;
                } else {
                    self.goto_sol_leg_or_after();
                }
                CreateAdvance::Continue
            }
            CreateStep::SwapSlippage => {
                let n = self.slippage_picks().len();
                self.swap_slippage_idx = self.choice % n;
                self.goto_sol_leg_or_after();
                CreateAdvance::Continue
            }
            CreateStep::SolLeg => {
                self.sol_leg = if self.choice.is_multiple_of(2) {
                    SolLegMode::Wsol
                } else {
                    SolLegMode::NativeSol
                };
                self.advance_after_deposit();
                CreateAdvance::Continue
            }
            CreateStep::DlmmStrategy => {
                self.dlmm_idx = self.choice % 5;
                self.step = CreateStep::Review;
                CreateAdvance::Continue
            }
            CreateStep::Review => CreateAdvance::Ready,
        }
    }

    fn goto_sol_leg_or_after(&mut self) {
        // Create: SOL pools always use native SOL (wrap start / unwrap end).
        // No SolLeg prompt — same policy as manage.
        self.sol_leg = if self.pool_has_wsol_mint() {
            SolLegMode::NativeSol
        } else {
            SolLegMode::Wsol
        };
        self.advance_after_deposit();
    }

    fn advance_after_deposit(&mut self) {
        if self.venue() == Venue::MeteoraDlmm {
            self.step = CreateStep::DlmmStrategy;
            self.choice = self.dlmm_idx;
        } else {
            self.step = CreateStep::Review;
        }
    }

    /// Step before SolLeg / Dlmm / Review (Deposit or SwapSlippage).
    fn step_before_sol_leg(&self) -> CreateStep {
        if self.swap_to_match {
            CreateStep::SwapSlippage
        } else {
            CreateStep::Deposit
        }
    }

    pub fn on_back(&mut self) -> bool {
        self.error = None;
        match self.step {
            CreateStep::Venue => true,
            CreateStep::Pool => {
                self.step = CreateStep::Venue;
                false
            }
            CreateStep::Range => {
                self.preview = None;
                self.step = CreateStep::Pool;
                false
            }
            CreateStep::Deposit => {
                self.step = CreateStep::Range;
                false
            }
            CreateStep::SwapSlippage => {
                self.step = CreateStep::Deposit;
                false
            }
            CreateStep::SolLeg => {
                self.step = self.step_before_sol_leg();
                if self.step == CreateStep::SwapSlippage {
                    let n = self.slippage_picks().len();
                    self.choice = self.swap_slippage_idx % n;
                }
                false
            }
            CreateStep::DlmmStrategy => {
                self.step = if self.pool_needs_sol_leg() {
                    CreateStep::SolLeg
                } else {
                    self.step_before_sol_leg()
                };
                false
            }
            CreateStep::Review => {
                self.step = if self.venue() == Venue::MeteoraDlmm {
                    CreateStep::DlmmStrategy
                } else if self.pool_needs_sol_leg() {
                    CreateStep::SolLeg
                } else {
                    self.step_before_sol_leg()
                };
                false
            }
        }
    }

    pub fn on_up(&mut self) {
        match self.step {
            CreateStep::Venue => {
                self.venue_idx = if self.venue_idx == 0 {
                    2
                } else {
                    self.venue_idx - 1
                };
            }
            CreateStep::Deposit => {
                self.choice = if self.choice == 0 { 2 } else { self.choice - 1 };
            }
            CreateStep::SwapSlippage => {
                let n = self.slippage_picks().len();
                self.choice = if self.choice == 0 { n - 1 } else { self.choice - 1 };
                self.swap_slippage_idx = self.choice;
            }
            CreateStep::SolLeg => {
                self.choice = self.choice.saturating_sub(1);
            }
            CreateStep::DlmmStrategy => {
                self.dlmm_idx = if self.dlmm_idx == 0 {
                    4
                } else {
                    self.dlmm_idx - 1
                };
                self.choice = self.dlmm_idx;
            }
            CreateStep::Range => {
                self.nudge_range(-1, false);
            }
            _ => {}
        }
    }

    /// Chart zoom on the range step (`+` in · `-` out · `0` fit draft).
    /// Returns a short status hint (incl. min/max when zoom stops).
    pub fn on_chart_zoom(&mut self, dir: i8) -> String {
        if self.step != CreateStep::Range {
            return String::new();
        }
        let Some(p) = self.preview.as_mut() else {
            return String::new();
        };
        let changed = match dir {
            1 => p.zoom_in(),
            -1 => p.zoom_out(),
            _ => {
                p.zoom_fit_range();
                true
            }
        };
        let half = p.effective_half_spacings();
        let max = p.max_chart_half();
        if !changed && dir == 1 {
            format!("zoom min ±{half} · - wider · 0 fit range")
        } else if !changed && dir == -1 {
            format!("zoom full ±{half} · + tighter · 0 fit range")
        } else if half >= max {
            format!("chart full ±{half} · + zoom in · 0 fit range")
        } else {
            format!("chart ±{half}/{max} · +/- zoom · 0 fit range")
        }
    }

    pub fn on_down(&mut self) {
        match self.step {
            CreateStep::Venue => {
                self.venue_idx = (self.venue_idx + 1) % 3;
            }
            CreateStep::Deposit => {
                self.choice = (self.choice + 1) % 3;
            }
            CreateStep::SwapSlippage => {
                let n = self.slippage_picks().len();
                self.choice = (self.choice + 1) % n;
                self.swap_slippage_idx = self.choice;
            }
            CreateStep::SolLeg => {
                self.choice = (self.choice + 1) % 2;
            }
            CreateStep::DlmmStrategy => {
                self.dlmm_idx = (self.dlmm_idx + 1) % 5;
                self.choice = self.dlmm_idx;
            }
            CreateStep::Range => {
                self.nudge_range(1, false);
            }
            _ => {}
        }
    }

    pub fn on_left(&mut self, shift: bool) {
        if self.step == CreateStep::Range {
            self.nudge_range(-1, shift);
        } else if self.step == CreateStep::SwapSlippage {
            self.cycle_swap_router();
        }
    }

    pub fn on_right(&mut self, shift: bool) {
        if self.step == CreateStep::Range {
            self.nudge_range(1, shift);
        } else if self.step == CreateStep::SwapSlippage {
            self.cycle_swap_router();
        }
    }

    pub fn on_tab(&mut self) {
        match self.step {
            CreateStep::Range => {
                self.range_focus = self.range_focus.toggle();
            }
            CreateStep::SwapSlippage => {
                self.cycle_swap_router();
            }
            _ => {}
        }
    }

    fn nudge_range(&mut self, delta_steps: i32, shift_both: bool) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        if shift_both {
            p.shift_range(delta_steps);
            return;
        }
        match self.range_focus {
            RangeFocus::Lower => p.nudge_lower(delta_steps),
            RangeFocus::Upper => p.nudge_upper(delta_steps),
        }
    }

    pub fn build_params(&self) -> Result<CreatePositionParams, String> {
        let pool: Pubkey = self
            .pool_input
            .trim()
            .parse()
            .map_err(|_| "invalid pool pubkey".to_string())?;
        let preview = self.preview.as_ref().ok_or_else(|| "pool not loaded".to_string())?;
        let tick_lower = preview.tick_lower;
        let tick_upper = preview.tick_upper;
        if tick_lower >= tick_upper {
            return Err("lower must be < upper".into());
        }
        let amount_a: u64 = if self.needs_amount_a() {
            self.parse_amount_clamped(&self.amount_a_input, true)?
        } else {
            0
        };
        let amount_b: u64 = if self.needs_amount_b() {
            self.parse_amount_clamped(&self.amount_b_input, false)?
        } else {
            0
        };
        if !self.swap_to_match {
            match self.shape {
                DepositShape::Dual if amount_a == 0 && amount_b == 0 => {
                    return Err("dual needs amounts or swap-to-match".into());
                }
                DepositShape::SingleA if amount_a == 0 => {
                    return Err("single-A needs amount_a".into());
                }
                DepositShape::SingleB if amount_b == 0 => {
                    return Err("single-B needs amount_b".into());
                }
                _ => {}
            }
        }
        let sol_leg = if self.pool_needs_sol_leg() {
            self.sol_leg
        } else {
            SolLegMode::Wsol
        };
        Ok(CreatePositionParams {
            venue: self.venue(),
            pool,
            tick_lower,
            tick_upper,
            amount_a,
            amount_b,
            shape: self.shape,
            sizing: DepositSizing::Exact,
            sol_leg,
            dlmm_strategy: if self.venue() == Venue::MeteoraDlmm {
                Some(self.dlmm_strategy())
            } else {
                None
            },
            swap_to_match: self.swap_to_match,
            swap_slippage_bps: self.swap_slippage_bps(),
            symbol_a: Some(preview.token_a.symbol.clone()),
            symbol_b: Some(preview.token_b.symbol.clone()),
        })
    }
}

/// Tick delta for a multiplicative price factor (1.0001^tick ≈ Uniswap/Orca).
fn price_factor_to_ticks(factor: f64) -> i32 {
    if factor <= 0.0 {
        return 0;
    }
    (factor.ln() / 1.0001_f64.ln()).round() as i32
}

// ── manage · deposit (existing position) ─────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageDepositStep {
    /// 2-side · 1-side A · 1-side B
    Mode,
    Amount,
    /// Always shown for 1-side (autoswap).
    SwapSlippage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageDepositAdvance {
    Continue,
    Ready,
}

/// Add liquidity to an open position · 2-side or 1-side (+ swap only when in-range).
#[derive(Debug)]
pub struct ManageDepositWizard {
    pub step: ManageDepositStep,
    pub choice: usize,
    pub mode: DepositMode,
    pub amount_input: String,
    pub amount_a_input: String,
    pub amount_b_input: String,
    pub swap_slippage_choice: usize,
    pub swap_router: String,
    /// Set in [`Self::commit_amount`] — true only when autoswap is actually required.
    pub swap_needed: bool,
    pub error: Option<String>,
    // Snapshot from PositionDetail
    pub symbol_a: String,
    pub symbol_b: String,
    pub decimals_a: u8,
    pub decimals_b: u8,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub tick_current: i32,
    pub sqrt_price: u128,
    /// Wallet SOL / WSOL / token balances (fetched when opening deposit).
    pub balances: Option<WalletPoolBalances>,
}

impl ManageDepositWizard {
    pub fn from_detail(
        detail: &sol_clmm_manager::PositionDetail,
        swap_router: String,
        balances: Option<WalletPoolBalances>,
    ) -> Self {
        let side = range_deposit_side(
            detail.pool.sqrt_price,
            detail.row.tick_lower,
            detail.row.tick_upper,
            detail.pool.tick_current_index,
        );
        let choice = side.recommended_shape_idx();
        Self {
            step: ManageDepositStep::Mode,
            choice,
            mode: DepositMode::from_idx(choice),
            amount_input: String::new(),
            amount_a_input: String::new(),
            amount_b_input: String::new(),
            swap_slippage_choice: 0, // RTSE recommended
            swap_router,
            swap_needed: false,
            error: None,
            symbol_a: Self::display_symbol(&detail.token_a.symbol, &detail.token_a.mint),
            symbol_b: Self::display_symbol(&detail.token_b.symbol, &detail.token_b.mint),
            decimals_a: detail.token_a.decimals,
            decimals_b: detail.token_b.decimals,
            mint_a: detail.token_a.mint,
            mint_b: detail.token_b.mint,
            tick_lower: detail.row.tick_lower,
            tick_upper: detail.row.tick_upper,
            tick_current: detail.pool.tick_current_index,
            sqrt_price: detail.pool.sqrt_price,
            balances,
        }
    }

    /// Manage shows native SOL, never "WSOL".
    fn display_symbol(symbol: &str, mint: &Pubkey) -> String {
        if is_wsol(mint) {
            "SOL".into()
        } else {
            symbol.to_string()
        }
    }

    fn range_side(&self) -> RangeDepositSide {
        range_deposit_side(
            self.sqrt_price,
            self.tick_lower,
            self.tick_upper,
            self.tick_current,
        )
    }

    /// Autoswap only when the range needs both tokens (in-range 1-side) or the
    /// chosen side can't deposit without a swap. Out-of-range matching 1-side → no swap.
    pub fn swap_to_match(&self) -> bool {
        self.swap_needed
    }

    pub fn shape(&self) -> DepositShape {
        self.mode.to_shape()
    }

    pub fn swap_slippage_bps(&self) -> u16 {
        self.slippage_picks()[self.swap_slippage_choice.min(4)].bps()
    }

    fn slippage_picks(&self) -> [SlippagePick; 5] {
        [
            SlippagePick::Recommended,
            SlippagePick::Default50,
            SlippagePick::Fixed(10),
            SlippagePick::Fixed(100),
            SlippagePick::Fixed(300),
        ]
    }

    /// Manage always treats SOL as native: wrap at start, unwrap/close WSOL at end
    /// (Jupiter `wrapAndUnwrapSol=false`).
    pub fn sol_leg(&self) -> SolLegMode {
        if self.has_wsol() {
            SolLegMode::NativeSol
        } else {
            SolLegMode::Wsol
        }
    }

    fn has_wsol(&self) -> bool {
        pool_has_wsol(&self.mint_a, &self.mint_b)
    }

    fn wallet_line(&self) -> String {
        match &self.balances {
            Some(b) => b.summary_line(
                &self.symbol_a,
                &self.symbol_b,
                self.decimals_a,
                self.decimals_b,
            ),
            None => "wallet balances unavailable".into(),
        }
    }

    fn use_native_for_max(&self) -> bool {
        // Manage always spends native SOL (+ existing WSOL ATA) on SOL pools.
        self.has_wsol()
    }

    fn max_for_primary(&self) -> Option<u64> {
        let bal = self.balances.as_ref()?;
        let (_, primary_a) = self.primary_side();
        Some(bal.max_depositable(primary_a, self.use_native_for_max()))
    }

    pub fn status_hint(&self) -> String {
        match self.step {
            ManageDepositStep::Mode => {
                format!(
                    "deposit · ↑↓ 2-side / 1-side · Enter · Esc · {}/{}",
                    self.symbol_a, self.symbol_b
                )
            }
            ManageDepositStep::Amount => {
                if self.swap_to_match() {
                    "1-side · type amount · autoswap · Enter · Esc".into()
                } else {
                    "2-side · type primary amount · Enter · Esc".into()
                }
            }
            ManageDepositStep::SwapSlippage => {
                format!(
                    "autoswap slippage · Tab {} ↔ · ↑↓ · Enter",
                    if self.swap_router == "jupiter" {
                        "Jupiter"
                    } else {
                        "Titan"
                    }
                )
            }
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(err) = &self.error {
            out.push(format!("! {err}"));
        }
        out.push(format!(
            "add liquidity · {} / {} · ticks [{}, {}]",
            self.symbol_a, self.symbol_b, self.tick_lower, self.tick_upper
        ));
        out.push(String::new());
        out.push(format!("wallet · {}", self.wallet_line()));
        out.push(String::new());
        match self.step {
            ManageDepositStep::Mode => {
                let side = self.range_side();
                out.push(side.label(&self.symbol_a, &self.symbol_b));
                out.push("choose deposit shape · ↑↓ · Enter".into());
                out.push(String::new());
                for (i, mode) in DepositMode::ALL.iter().enumerate() {
                    let mark = if i == self.choice % 3 { "▸" } else { " " };
                    let mut label = mode.label(&self.symbol_a, &self.symbol_b);
                    match (side, mode) {
                        (RangeDepositSide::Both, DepositMode::SingleA | DepositMode::SingleB) => {
                            label.push_str(" · + ratio swap");
                        }
                        (RangeDepositSide::TokenAOnly, DepositMode::SingleB)
                        | (RangeDepositSide::TokenBOnly, DepositMode::SingleA) => {
                            label.push_str(" · + full swap");
                        }
                        (RangeDepositSide::TokenAOnly, DepositMode::SingleA)
                        | (RangeDepositSide::TokenBOnly, DepositMode::SingleB) => {
                            label.push_str(" · no swap");
                        }
                        _ => {}
                    }
                    out.push(format!("{mark} {label}"));
                }
            }
            ManageDepositStep::Amount => {
                out.push(self.mode.label(&self.symbol_a, &self.symbol_b));
                out.push(self.range_side().label(&self.symbol_a, &self.symbol_b));
                match self.mode {
                    DepositMode::Dual => {
                        out.push("2-side · pair autofilled from range ratio (0 if out of range)".into());
                    }
                    DepositMode::SingleA | DepositMode::SingleB => match self.range_side() {
                        RangeDepositSide::Both => {
                            out.push("1-side in-range · proportional autoswap to match ratio".into());
                        }
                        side => {
                            let need_a = side == RangeDepositSide::TokenAOnly;
                            let funding_a = matches!(self.mode, DepositMode::SingleA);
                            if funding_a == need_a {
                                out.push(
                                    "out-of-range · correct token · no swap · deposit as-is".into(),
                                );
                            } else {
                                let (from, to) = if funding_a {
                                    (self.symbol_a.as_str(), self.symbol_b.as_str())
                                } else {
                                    (self.symbol_b.as_str(), self.symbol_a.as_str())
                                };
                                out.push(format!(
                                    "out-of-range · wrong token · full swap {from} → {to}"
                                ));
                            }
                        }
                    },
                }
                out.push(String::new());
                let (sym, primary_a) = self.primary_side();
                out.push(format!("amount {sym}"));
                if let Some(max) = self.max_for_primary() {
                    let dec = if primary_a {
                        self.decimals_a
                    } else {
                        self.decimals_b
                    };
                    out.push(format!(
                        "max available · {} {sym}",
                        format_amount(u128::from(max), dec)
                    ));
                }
                out.push(format!("▸▸{}", self.amount_input));
            }
            ManageDepositStep::SwapSlippage => {
                out.push(format!(
                    "autoswap · {} · Tab to switch router",
                    self.swap_router
                ));
                out.push(String::new());
                for (i, pick) in self.slippage_picks().iter().enumerate() {
                    let mark = if i == self.swap_slippage_choice { "▸" } else { " " };
                    out.push(format!("{mark} {}", pick.label(&self.swap_router)));
                }
            }
        }
        out
    }

    /// (symbol, is_a)
    fn primary_side(&self) -> (&str, bool) {
        match self.mode {
            DepositMode::Dual => {
                // Prefer non-SOL as typed side (same as create).
                if is_wsol(&self.mint_a) && !is_wsol(&self.mint_b) {
                    (&self.symbol_b, false)
                } else {
                    (&self.symbol_a, true)
                }
            }
            DepositMode::SingleA => (&self.symbol_a, true),
            DepositMode::SingleB => (&self.symbol_b, false),
        }
    }

    pub fn select_next(&mut self) {
        self.error = None;
        match self.step {
            ManageDepositStep::Mode => self.choice = (self.choice + 1) % 3,
            ManageDepositStep::SwapSlippage => {
                self.swap_slippage_choice = (self.swap_slippage_choice + 1) % 5;
            }
            ManageDepositStep::Amount => {}
        }
    }

    pub fn select_prev(&mut self) {
        self.error = None;
        match self.step {
            ManageDepositStep::Mode => {
                self.choice = if self.choice == 0 { 2 } else { self.choice - 1 };
            }
            ManageDepositStep::SwapSlippage => {
                self.swap_slippage_choice = if self.swap_slippage_choice == 0 {
                    4
                } else {
                    self.swap_slippage_choice - 1
                };
            }
            ManageDepositStep::Amount => {}
        }
    }

    pub fn toggle_router(&mut self) {
        if self.step != ManageDepositStep::SwapSlippage {
            return;
        }
        self.swap_router = if self.swap_router == "jupiter" {
            "titan".into()
        } else {
            "jupiter".into()
        };
    }

    pub fn input_char(&mut self, c: char) {
        if self.step != ManageDepositStep::Amount {
            return;
        }
        if c.is_ascii_digit() || c == '.' || c == ',' {
            self.amount_input.push(c);
            self.error = None;
        }
    }

    pub fn input_backspace(&mut self) {
        if self.step == ManageDepositStep::Amount {
            self.amount_input.pop();
            self.error = None;
        }
    }

    pub fn back(&mut self) -> bool {
        self.error = None;
        match self.step {
            ManageDepositStep::Mode => true, // leave wizard
            ManageDepositStep::Amount => {
                self.step = ManageDepositStep::Mode;
                false
            }
            ManageDepositStep::SwapSlippage => {
                self.step = ManageDepositStep::Amount;
                false
            }
        }
    }

    pub fn advance(&mut self) -> ManageDepositAdvance {
        self.error = None;
        match self.step {
            ManageDepositStep::Mode => {
                self.mode = DepositMode::from_idx(self.choice);
                self.amount_input.clear();
                self.step = ManageDepositStep::Amount;
                ManageDepositAdvance::Continue
            }
            ManageDepositStep::Amount => {
                if let Err(e) = self.commit_amount() {
                    self.error = Some(e);
                    return ManageDepositAdvance::Continue;
                }
                if self.swap_to_match() {
                    self.step = ManageDepositStep::SwapSlippage;
                    ManageDepositAdvance::Continue
                } else {
                    ManageDepositAdvance::Ready
                }
            }
            ManageDepositStep::SwapSlippage => ManageDepositAdvance::Ready,
        }
    }

    fn commit_amount(&mut self) -> Result<(), String> {
        let raw = self.amount_input.trim();
        if raw.is_empty() {
            return Err("enter an amount".into());
        }
        let (_, primary_a) = self.primary_side();
        let dec = if primary_a {
            self.decimals_a
        } else {
            self.decimals_b
        };
        let mut primary = parse_ui_amount(raw, dec)?;
        if primary == 0 {
            return Err("amount must be > 0".into());
        }
        if let Some(max) = self.max_for_primary() {
            if primary > max {
                return Err(format!(
                    "amount exceeds wallet · max {}",
                    format_amount(u128::from(max), dec)
                ));
            }
            primary = primary.min(max);
        }

        // Pair fill for 2-side from unit deposit ratio.
        let (ua, ub) = if self.sqrt_price > 0 {
            sol_clmm_manager::deposit_ratio_planner(
                self.sqrt_price,
                self.tick_lower,
                self.tick_upper,
                1u128 << 48,
            )
        } else {
            (0, 0)
        };

        let primary_ui = format_amount(u128::from(primary), dec);
        match self.mode {
            DepositMode::Dual => {
                if primary_a {
                    self.amount_a_input = primary_ui;
                    let mut b = if ua > 0 {
                        (u128::from(primary).saturating_mul(ub) / ua).min(u128::from(u64::MAX))
                            as u64
                    } else {
                        0
                    };
                    if let Some(bal) = &self.balances {
                        let max_b = bal.max_depositable(false, self.use_native_for_max());
                        if b > max_b {
                            return Err(format!(
                                "pair needs {} {} · wallet has {}",
                                format_amount(u128::from(b), self.decimals_b),
                                self.symbol_b,
                                format_amount(u128::from(max_b), self.decimals_b)
                            ));
                        }
                        b = b.min(max_b);
                    }
                    self.amount_b_input = format_amount(u128::from(b), self.decimals_b);
                } else {
                    self.amount_b_input = primary_ui;
                    let mut a = if ub > 0 {
                        (u128::from(primary).saturating_mul(ua) / ub).min(u128::from(u64::MAX))
                            as u64
                    } else {
                        0
                    };
                    if let Some(bal) = &self.balances {
                        let max_a = bal.max_depositable(true, self.use_native_for_max());
                        if a > max_a {
                            return Err(format!(
                                "pair needs {} {} · wallet SOL max {}",
                                format_amount(u128::from(a), self.decimals_a),
                                self.symbol_a,
                                format_amount(u128::from(max_a), self.decimals_a)
                            ));
                        }
                        a = a.min(max_a);
                    }
                    self.amount_a_input = format_amount(u128::from(a), self.decimals_a);
                }
            }
            DepositMode::SingleA => {
                self.amount_a_input = primary_ui;
                self.amount_b_input.clear();
            }
            DepositMode::SingleB => {
                self.amount_b_input = primary_ui;
                self.amount_a_input.clear();
            }
        }

        // Same rule as create: Dual never forces swap; 1-side only when analyze says so.
        let (a, b) = self.amounts_raw().unwrap_or((0, 0));
        let an = analyze_deposit(
            u128::from(a),
            u128::from(b),
            self.sqrt_price,
            self.tick_lower,
            self.tick_upper,
            self.tick_current,
            self.mode.to_shape(),
        );
        self.swap_needed = match self.mode {
            DepositMode::Dual => false,
            DepositMode::SingleA | DepositMode::SingleB => {
                an.swap_needed || !an.shape_ok_without_swap
            }
        };
        Ok(())
    }

    pub fn amounts_raw(&self) -> Result<(u64, u64), String> {
        let a = if self.amount_a_input.trim().is_empty() {
            0
        } else {
            parse_ui_amount(self.amount_a_input.trim(), self.decimals_a)?
        };
        let b = if self.amount_b_input.trim().is_empty() {
            0
        } else {
            parse_ui_amount(self.amount_b_input.trim(), self.decimals_b)?
        };
        if a == 0 && b == 0 {
            return Err("need amount A and/or B".into());
        }
        Ok((a, b))
    }
}

// ── manage · wrap SOL (standalone) ───────────────────────────────────────

/// Wrap native SOL → WSOL ATA (outside deposit / Jupiter).
#[derive(Debug)]
pub struct ManageWrapWizard {
    pub amount_input: String,
    pub error: Option<String>,
    pub sol_lamports: u64,
    pub wsol_ata: u64,
    pub rent_reserve: u64,
}

impl ManageWrapWizard {
    pub fn new(balances: Option<&WalletPoolBalances>) -> Self {
        let (sol, wsol, reserve) = match balances {
            Some(b) => {
                let wsol = if b.a.is_wsol {
                    b.a.ata_amount
                } else if b.b.is_wsol {
                    b.b.ata_amount
                } else {
                    0
                };
                (b.sol_lamports, wsol, b.rent_reserve_lamports)
            }
            None => (0, 0, sol_clmm_manager::SOL_FEE_RESERVE_LAMPORTS),
        };
        Self {
            amount_input: String::new(),
            error: None,
            sol_lamports: sol,
            wsol_ata: wsol,
            rent_reserve: reserve,
        }
    }

    pub fn max_wrap(&self) -> u64 {
        self.sol_lamports.saturating_sub(self.rent_reserve)
    }

    pub fn status_hint(&self) -> String {
        "wrap SOL · type amount · Enter · Esc".into()
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(err) = &self.error {
            out.push(format!("! {err}"));
        }
        out.push("wrap native SOL → WSOL ATA".into());
        out.push(String::new());
        out.push(format!(
            "wallet · SOL {} · WSOL ATA {}",
            format_amount(u128::from(self.sol_lamports), 9),
            format_amount(u128::from(self.wsol_ata), 9)
        ));
        out.push(format!(
            "max wrap · {} SOL (−{} fee reserve)",
            format_amount(u128::from(self.max_wrap()), 9),
            format_amount(u128::from(self.rent_reserve), 9)
        ));
        out.push(String::new());
        out.push("amount SOL".into());
        out.push(format!("▸▸{}", self.amount_input));
        out
    }

    pub fn input_char(&mut self, c: char) {
        if c.is_ascii_digit() || c == '.' || c == ',' {
            self.amount_input.push(c);
            self.error = None;
        }
    }

    pub fn input_backspace(&mut self) {
        self.amount_input.pop();
        self.error = None;
    }

    pub fn lamports(&self) -> Result<u64, String> {
        let raw = self.amount_input.trim();
        if raw.is_empty() {
            return Err("enter an amount".into());
        }
        let mut amt = parse_ui_amount(raw, 9)?;
        if amt == 0 {
            return Err("amount must be > 0".into());
        }
        let max = self.max_wrap();
        if amt > max {
            return Err(format!(
                "exceeds max wrap · {}",
                format_amount(u128::from(max), 9)
            ));
        }
        amt = amt.min(max);
        Ok(amt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateAdvance {
    Continue,
    /// Pool pubkey accepted — App should fetch [`PoolPreview`].
    NeedFetch,
    /// Review confirmed — assemble + simulate.
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmPhase {
    /// Action chosen · pick priority fee · Enter runs simulation (not sent yet).
    Tip,
    /// Simulation OK · Enter sends · Esc cancels · [ ] goes back to tip.
    Review,
    /// Sim or send in progress.
    Busy,
    /// Sim or confirmation failed — Retry / Cancel.
    Failed,
    /// Confirmed on-chain.
    Done,
}

#[derive(Debug)]
pub struct WriteConfirm {
    pub plan: WritePlan,
    /// Extra signers (position NFT mint / DLMM position key).
    pub extra_signers: Vec<Keypair>,
    pub phase: ConfirmPhase,
    pub choice: usize,
    pub last_error: Option<String>,
    /// Full error / sim failure lines (not truncated) for scrollable panel.
    pub error_lines: Vec<String>,
    pub signature: Option<String>,
    pub sim_preview: Vec<String>,
    /// From tx v1 `TransactionConfig` after auto/manual estimate (not ComputeBudget ixs).
    pub budget_label: Option<String>,
    /// Human “paid …” line after confirm (fee + balance Δ).
    pub paid_label: Option<String>,
    /// Priority fee for this send. `0` = off · `u64::MAX` = auto · else fixed.
    pub tip_lamports: u64,
    /// Optional Jupiter/Titan tip floor from assemble (create swap).
    pub router_tip: Option<u64>,
    pub position_pk: Option<String>,
    pub pool_pk: Option<String>,
    pub pool_label: Option<String>,
    /// Wallet balances re-fetched after a successful send.
    pub wallet_balances: Option<WalletPoolBalances>,
}

impl WriteConfirm {
    pub fn with_tip(
        plan: WritePlan,
        extra_signers: Vec<Keypair>,
        tip_lamports: u64,
        router_tip: Option<u64>,
    ) -> Self {
        let choice = crate::app::TIP_PRESETS
            .iter()
            .position(|&p| p == tip_lamports)
            .unwrap_or(3); // default highlight 5000
        Self {
            plan,
            extra_signers,
            phase: ConfirmPhase::Tip,
            choice,
            last_error: None,
            error_lines: Vec::new(),
            signature: None,
            sim_preview: Vec::new(),
            budget_label: None,
            paid_label: None,
            tip_lamports,
            router_tip,
            position_pk: None,
            pool_pk: None,
            pool_label: None,
            wallet_balances: None,
        }
    }

    pub fn set_error(&mut self, err: String) {
        self.last_error = Some(err.clone());
        self.error_lines = err
            .lines()
            .map(|l| l.to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if self.error_lines.is_empty() {
            self.error_lines.push(err);
        }
    }

    /// Sync `tip_lamports` from the ↑↓ `choice` index into [`TIP_PRESETS`].
    pub fn apply_tip_choice(&mut self) {
        let presets = crate::app::TIP_PRESETS;
        let idx = self.choice.min(presets.len() - 1);
        self.choice = idx;
        self.tip_lamports = presets[idx];
    }

    pub fn nudge_tip(&mut self, up: bool) {
        let n = crate::app::TIP_PRESETS.len();
        if up {
            self.choice = (self.choice + 1) % n;
        } else {
            self.choice = if self.choice == 0 { n - 1 } else { self.choice - 1 };
        }
        self.apply_tip_choice();
    }

    pub fn failed_choices() -> [&'static str; 2] {
        [
            "Change priority fee · then Enter to simulate",
            "Cancel · leave confirm",
        ]
    }
}

/// Fresh keypair for position mint / DLMM position account (must sign create tx).
pub fn new_position_signer() -> Keypair {
    Keypair::new()
}
