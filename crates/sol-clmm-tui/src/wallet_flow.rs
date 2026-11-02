//! Wallet management (list-level): wrap · unwrap · swap · create pool.

#![cfg(feature = "write")]

use sol_clmm_manager::{
    CreatePoolMode, CreatePoolParams, DepositShape, DlmmStrategy, SolLegMode, Venue, WSOL,
    WalletTokenBalance, format_amount, parse_ui_amount, popular_swap_mints, wsol_mint,
};
use solana_pubkey::Pubkey;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletMenu {
    Wrap,
    Unwrap,
    Swap,
    CreatePool,
}

impl WalletMenu {
    pub const ALL: [WalletMenu; 4] = [Self::Wrap, Self::Unwrap, Self::Swap, Self::CreatePool];

    pub fn title(self) -> &'static str {
        match self {
            Self::Wrap => "wrap SOL",
            Self::Unwrap => "unwrap SOL",
            Self::Swap => "swap",
            Self::CreatePool => "create pool",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Wrap => "native SOL → WSOL ATA",
            Self::Unwrap => "close WSOL ATA → native SOL",
            Self::Swap => "Jupiter / Titan · popular destinations",
            Self::CreatePool => "Orca / Raydium / Meteora · pool ± deposit ± swap",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapStep {
    From,
    To,
    Amount,
    Slippage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatePoolStep {
    Venue,
    MintA,
    MintB,
    FeeParam,
    Price,
    Mode,
    Review,
}

#[derive(Debug)]
pub struct WalletWizard {
    pub menu_choice: usize,
    pub sol_lamports: u64,
    pub tokens: Vec<WalletTokenBalance>,
    pub rent_reserve: u64,
    /// Sub-flow
    pub wrap: Option<crate::write_flow::ManageWrapWizard>,
    pub swap: Option<SwapWizard>,
    pub create_pool: Option<CreatePoolWizard>,
    pub error: Option<String>,
}

impl WalletWizard {
    pub fn new(sol_lamports: u64, tokens: Vec<WalletTokenBalance>, rent_reserve: u64) -> Self {
        Self {
            menu_choice: 0,
            sol_lamports,
            tokens,
            rent_reserve,
            wrap: None,
            swap: None,
            create_pool: None,
            error: None,
        }
    }

    pub fn menu(&self) -> WalletMenu {
        WalletMenu::ALL[self.menu_choice % WalletMenu::ALL.len()]
    }

    pub fn status_hint(&self) -> String {
        if let Some(w) = &self.wrap {
            return w.status_hint();
        }
        if let Some(s) = &self.swap {
            return s.status_hint();
        }
        if let Some(c) = &self.create_pool {
            return c.status_hint();
        }
        "wallet · ↑↓ · Enter · Esc positions".into()
    }

    pub fn lines(&self) -> Vec<String> {
        if let Some(w) = &self.wrap {
            return w.lines();
        }
        if let Some(s) = &self.swap {
            return s.lines();
        }
        if let Some(c) = &self.create_pool {
            return c.lines();
        }
        let mut out = Vec::new();
        if let Some(err) = &self.error {
            out.push(format!("! {err}"));
        }
        out.push(format!(
            "SOL {} · holdings {}",
            format_amount(u128::from(self.sol_lamports), 9),
            self.tokens.len()
        ));
        let top: Vec<String> = self
            .tokens
            .iter()
            .take(6)
            .map(|t| {
                format!(
                    "{} {}",
                    t.symbol,
                    format_amount(u128::from(t.amount), t.decimals)
                )
            })
            .collect();
        if !top.is_empty() {
            out.push(top.join(" · "));
        }
        out.push(String::new());
        out.push("wallet tools".into());
        for (i, m) in WalletMenu::ALL.iter().enumerate() {
            let mark = if i == self.menu_choice % WalletMenu::ALL.len() {
                "▸"
            } else {
                " "
            };
            out.push(format!("{mark} {} · {}", m.title(), m.detail()));
        }
        out
    }

    pub fn up(&mut self) {
        if self.wrap.is_some() {
            return;
        }
        if let Some(c) = self.create_pool.as_mut() {
            c.up();
            return;
        }
        if let Some(s) = self.swap.as_mut() {
            s.up();
            return;
        }
        let n = WalletMenu::ALL.len();
        self.menu_choice = if self.menu_choice == 0 {
            n - 1
        } else {
            self.menu_choice - 1
        };
    }

    pub fn down(&mut self) {
        if self.wrap.is_some() {
            return;
        }
        if let Some(c) = self.create_pool.as_mut() {
            c.down();
            return;
        }
        if let Some(s) = self.swap.as_mut() {
            s.down();
            return;
        }
        self.menu_choice = (self.menu_choice + 1) % WalletMenu::ALL.len();
    }

    pub fn input_char(&mut self, c: char) {
        if let Some(w) = self.wrap.as_mut() {
            w.input_char(c);
            return;
        }
        if let Some(s) = self.swap.as_mut() {
            s.input_char(c);
            return;
        }
        if let Some(p) = self.create_pool.as_mut() {
            p.input_char(c);
        }
    }

    pub fn input_backspace(&mut self) {
        if let Some(w) = self.wrap.as_mut() {
            w.input_backspace();
            return;
        }
        if let Some(s) = self.swap.as_mut() {
            s.input_backspace();
            return;
        }
        if let Some(p) = self.create_pool.as_mut() {
            p.input_backspace();
        }
    }

    pub fn tab(&mut self) {
        if let Some(s) = self.swap.as_mut() {
            s.cycle_router();
        }
        if let Some(p) = self.create_pool.as_mut() {
            p.tab();
        }
    }
}

#[derive(Debug)]
pub struct SwapWizard {
    pub step: SwapStep,
    pub from_idx: usize,
    pub to_idx: usize,
    pub amount_input: String,
    pub slippage_idx: usize,
    pub router: String,
    pub from_options: Vec<SwapTokenOpt>,
    pub to_options: Vec<SwapTokenOpt>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SwapTokenOpt {
    pub mint: Pubkey,
    pub symbol: String,
    pub decimals: u8,
    pub amount: u64,
    pub native_sol: bool,
}

impl SwapWizard {
    pub fn new(
        sol_lamports: u64,
        tokens: &[WalletTokenBalance],
        rent_reserve: u64,
        router: String,
    ) -> Self {
        let wsol = wsol_mint();
        let mut from_options = Vec::new();
        let native_max = sol_lamports.saturating_sub(rent_reserve);
        let wsol_ata = tokens
            .iter()
            .find(|t| t.is_wsol)
            .map(|t| t.amount)
            .unwrap_or(0);
        if native_max > 0 || wsol_ata > 0 {
            from_options.push(SwapTokenOpt {
                mint: wsol,
                symbol: "SOL".into(),
                decimals: 9,
                amount: native_max.saturating_add(wsol_ata),
                native_sol: true,
            });
        }
        for t in tokens {
            if t.is_wsol {
                continue;
            }
            from_options.push(SwapTokenOpt {
                mint: t.mint,
                symbol: t.symbol.clone(),
                decimals: t.decimals,
                amount: t.amount,
                native_sol: false,
            });
        }

        let mut to_options: Vec<SwapTokenOpt> = popular_swap_mints()
            .iter()
            .map(|(mint, sym)| {
                let bal = if *mint == wsol {
                    native_max.saturating_add(wsol_ata)
                } else {
                    tokens
                        .iter()
                        .find(|t| t.mint == *mint)
                        .map(|t| t.amount)
                        .unwrap_or(0)
                };
                SwapTokenOpt {
                    mint: *mint,
                    symbol: (*sym).into(),
                    decimals: if *mint == wsol { 9 } else { 6 },
                    amount: bal,
                    native_sol: *mint == wsol,
                }
            })
            .collect();
        for t in tokens {
            if to_options.iter().any(|o| o.mint == t.mint) {
                continue;
            }
            to_options.push(SwapTokenOpt {
                mint: t.mint,
                symbol: t.symbol.clone(),
                decimals: t.decimals,
                amount: t.amount,
                native_sol: t.is_wsol,
            });
        }

        Self {
            step: SwapStep::From,
            from_idx: 0,
            to_idx: 1.min(to_options.len().saturating_sub(1)),
            amount_input: String::new(),
            slippage_idx: 0,
            router,
            from_options,
            to_options,
            error: None,
        }
    }

    pub fn status_hint(&self) -> String {
        match self.step {
            SwapStep::From => "swap · pick FROM · ↑↓ · Enter · Esc".into(),
            SwapStep::To => "swap · pick TO · ↑↓ · Enter · Esc".into(),
            SwapStep::Amount => "swap · type amount · Enter · Esc".into(),
            SwapStep::Slippage => {
                format!("swap · slippage · Tab {} ↔ · Enter", self.router)
            }
        }
    }

    fn slippage_picks() -> [(u16, &'static str); 5] {
        [
            (u16::MAX, "RTSE / auto"),
            (10, "0.10%"),
            (50, "0.50%"),
            (100, "1.00%"),
            (300, "3.00%"),
        ]
    }

    pub fn slippage_bps(&self) -> u16 {
        Self::slippage_picks()[self.slippage_idx % 5].0
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(e) = &self.error {
            out.push(format!("! {e}"));
        }
        match self.step {
            SwapStep::From => {
                out.push("FROM (wallet)".into());
                for (i, t) in self.from_options.iter().enumerate() {
                    let mark = if i == self.from_idx { "▸" } else { " " };
                    out.push(format!(
                        "{mark} {} · {}",
                        t.symbol,
                        format_amount(u128::from(t.amount), t.decimals)
                    ));
                }
            }
            SwapStep::To => {
                out.push("TO (popular + wallet)".into());
                for (i, t) in self.to_options.iter().enumerate() {
                    let mark = if i == self.to_idx { "▸" } else { " " };
                    out.push(format!("{mark} {}", t.symbol));
                }
            }
            SwapStep::Amount => {
                let from = &self.from_options[self.from_idx];
                out.push(format!(
                    "{} → {}",
                    from.symbol,
                    self.to_options
                        .get(self.to_idx)
                        .map(|t| t.symbol.as_str())
                        .unwrap_or("?")
                ));
                out.push(format!(
                    "max {} {}",
                    format_amount(u128::from(from.amount), from.decimals),
                    from.symbol
                ));
                out.push(format!("▸▸{}", self.amount_input));
            }
            SwapStep::Slippage => {
                out.push(format!("router · {}", self.router));
                for (i, (bps, label)) in Self::slippage_picks().iter().enumerate() {
                    let mark = if i == self.slippage_idx { "▸" } else { " " };
                    let _ = bps;
                    out.push(format!("{mark} {label}"));
                }
            }
        }
        out
    }

    pub fn up(&mut self) {
        match self.step {
            SwapStep::From if !self.from_options.is_empty() => {
                self.from_idx = if self.from_idx == 0 {
                    self.from_options.len() - 1
                } else {
                    self.from_idx - 1
                };
            }
            SwapStep::To if !self.to_options.is_empty() => {
                self.to_idx = if self.to_idx == 0 {
                    self.to_options.len() - 1
                } else {
                    self.to_idx - 1
                };
            }
            SwapStep::Slippage => {
                self.slippage_idx = if self.slippage_idx == 0 {
                    4
                } else {
                    self.slippage_idx - 1
                };
            }
            _ => {}
        }
    }

    pub fn down(&mut self) {
        match self.step {
            SwapStep::From if !self.from_options.is_empty() => {
                self.from_idx = (self.from_idx + 1) % self.from_options.len();
            }
            SwapStep::To if !self.to_options.is_empty() => {
                self.to_idx = (self.to_idx + 1) % self.to_options.len();
            }
            SwapStep::Slippage => {
                self.slippage_idx = (self.slippage_idx + 1) % 5;
            }
            _ => {}
        }
    }

    pub fn input_char(&mut self, c: char) {
        if self.step == SwapStep::Amount && (c.is_ascii_digit() || c == '.' || c == ',') {
            self.amount_input.push(c);
            self.error = None;
        }
    }

    pub fn input_backspace(&mut self) {
        if self.step == SwapStep::Amount {
            self.amount_input.pop();
            self.error = None;
        }
    }

    pub fn cycle_router(&mut self) {
        self.router = if self.router == "jupiter" {
            "titan".into()
        } else {
            "jupiter".into()
        };
    }

    pub fn enter_advance(&mut self) -> Result<Option<SwapReady>, String> {
        match self.step {
            SwapStep::From => {
                if self.from_options.is_empty() {
                    return Err("no tokens to swap from".into());
                }
                self.step = SwapStep::To;
                Ok(None)
            }
            SwapStep::To => {
                let from = &self.from_options[self.from_idx];
                let to = &self.to_options[self.to_idx];
                if from.mint == to.mint {
                    return Err("from and to must differ".into());
                }
                self.step = SwapStep::Amount;
                Ok(None)
            }
            SwapStep::Amount => {
                let from = &self.from_options[self.from_idx];
                let raw = self.amount_input.trim();
                if raw.is_empty() {
                    return Err("enter an amount".into());
                }
                let mut amt = parse_ui_amount(raw, from.decimals)?;
                if amt == 0 {
                    return Err("amount must be > 0".into());
                }
                if amt > from.amount {
                    return Err(format!(
                        "exceeds balance · {}",
                        format_amount(u128::from(from.amount), from.decimals)
                    ));
                }
                amt = amt.min(from.amount);
                let _ = amt;
                self.step = SwapStep::Slippage;
                Ok(None)
            }
            SwapStep::Slippage => {
                let from = &self.from_options[self.from_idx];
                let to = &self.to_options[self.to_idx];
                let amt = parse_ui_amount(self.amount_input.trim(), from.decimals)?;
                Ok(Some(SwapReady {
                    input_mint: from.mint,
                    output_mint: to.mint,
                    amount: amt.min(from.amount),
                    slippage_bps: self.slippage_bps(),
                    input_is_native_sol: from.native_sol,
                    input_symbol: from.symbol.clone(),
                    output_symbol: to.symbol.clone(),
                    input_decimals: from.decimals,
                    output_decimals: to.decimals,
                    router: self.router.clone(),
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SwapReady {
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub amount: u64,
    pub slippage_bps: u16,
    pub input_is_native_sol: bool,
    pub input_symbol: String,
    pub output_symbol: String,
    pub input_decimals: u8,
    pub output_decimals: u8,
    pub router: String,
}

#[derive(Debug)]
pub struct CreatePoolWizard {
    pub step: CreatePoolStep,
    pub venue_idx: usize,
    pub mint_a_idx: usize,
    pub mint_b_idx: usize,
    pub fee_input: String,
    pub price_input: String,
    pub mode: CreatePoolMode,
    pub mint_options: Vec<SwapTokenOpt>,
    pub error: Option<String>,
}

impl CreatePoolWizard {
    pub fn new(sol_lamports: u64, tokens: &[WalletTokenBalance], rent_reserve: u64) -> Self {
        let wsol = wsol_mint();
        let native_max = sol_lamports.saturating_sub(rent_reserve);
        let mut mint_options: Vec<SwapTokenOpt> = popular_swap_mints()
            .iter()
            .map(|(mint, sym)| SwapTokenOpt {
                mint: *mint,
                symbol: (*sym).into(),
                decimals: if *mint == wsol { 9 } else { 6 },
                amount: if *mint == wsol {
                    native_max
                        + tokens
                            .iter()
                            .find(|t| t.is_wsol)
                            .map(|t| t.amount)
                            .unwrap_or(0)
                } else {
                    tokens
                        .iter()
                        .find(|t| t.mint == *mint)
                        .map(|t| t.amount)
                        .unwrap_or(0)
                },
                native_sol: *mint == wsol,
            })
            .collect();
        for t in tokens {
            if mint_options.iter().any(|o| o.mint == t.mint) {
                continue;
            }
            mint_options.push(SwapTokenOpt {
                mint: t.mint,
                symbol: t.symbol.clone(),
                decimals: t.decimals,
                amount: t.amount,
                native_sol: t.is_wsol,
            });
        }
        Self {
            step: CreatePoolStep::Venue,
            venue_idx: 0,
            mint_a_idx: 0,
            mint_b_idx: 1.min(mint_options.len().saturating_sub(1)),
            fee_input: "64".into(),
            price_input: "100".into(),
            mode: CreatePoolMode::PoolOnly,
            mint_options,
            error: None,
        }
    }

    fn venues() -> [Venue; 3] {
        [Venue::OrcaWhirlpool, Venue::RaydiumClmm, Venue::MeteoraDlmm]
    }

    pub fn venue(&self) -> Venue {
        Self::venues()[self.venue_idx % 3]
    }

    pub fn status_hint(&self) -> String {
        match self.step {
            CreatePoolStep::Venue => "create pool · venue · ↑↓ · Enter".into(),
            CreatePoolStep::MintA => "create pool · mint A · ↑↓ · Enter".into(),
            CreatePoolStep::MintB => "create pool · mint B · ↑↓ · Enter".into(),
            CreatePoolStep::FeeParam => {
                "fee · Orca tick spacing / Raydium config idx / DLMM bin step".into()
            }
            CreatePoolStep::Price => "initial price · B per A (human) · Enter".into(),
            CreatePoolStep::Mode => "mode · ↑↓ pool only / +deposit / +swap · Enter".into(),
            CreatePoolStep::Review => "review · Enter assemble · Esc back".into(),
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(e) = &self.error {
            out.push(format!("! {e}"));
        }
        match self.step {
            CreatePoolStep::Venue => {
                for (i, v) in Self::venues().iter().enumerate() {
                    let mark = if i == self.venue_idx { "▸" } else { " " };
                    out.push(format!("{mark} {}", v.full()));
                }
            }
            CreatePoolStep::MintA => {
                out.push("token A".into());
                for (i, t) in self.mint_options.iter().enumerate() {
                    let mark = if i == self.mint_a_idx { "▸" } else { " " };
                    out.push(format!("{mark} {}", t.symbol));
                }
            }
            CreatePoolStep::MintB => {
                out.push("token B".into());
                for (i, t) in self.mint_options.iter().enumerate() {
                    let mark = if i == self.mint_b_idx { "▸" } else { " " };
                    out.push(format!("{mark} {}", t.symbol));
                }
            }
            CreatePoolStep::FeeParam => {
                out.push(match self.venue() {
                    Venue::OrcaWhirlpool => "tick spacing (e.g. 64, 128)".into(),
                    Venue::RaydiumClmm => "amm_config index (e.g. 0..20)".into(),
                    Venue::MeteoraDlmm => "bin step (e.g. 10, 25)".into(),
                });
                out.push(format!("▸▸{}", self.fee_input));
            }
            CreatePoolStep::Price => {
                out.push("initial price · token B per token A".into());
                out.push(format!("▸▸{}", self.price_input));
            }
            CreatePoolStep::Mode => {
                for (i, m) in CreatePoolMode::ALL.iter().enumerate() {
                    let mark = if *m == self.mode { "▸" } else { " " };
                    let _ = i;
                    out.push(format!("{mark} {}", m.label()));
                }
            }
            CreatePoolStep::Review => match self.build_params() {
                Ok(p) => {
                    out.push(format!("venue · {}", p.venue.full()));
                    out.push(format!(
                        "{} / {} · fee {} · price {:.6}",
                        p.symbol_a.as_deref().unwrap_or("A"),
                        p.symbol_b.as_deref().unwrap_or("B"),
                        p.fee_param,
                        p.initial_price
                    ));
                    out.push(format!("mode · {}", p.mode.label()));
                    if p.mode != CreatePoolMode::PoolOnly {
                        out.push(
                            "seed deposit: after pool lands, use n create on the new pool".into(),
                        );
                    }
                }
                Err(e) => out.push(format!("! {e}")),
            },
        }
        out
    }

    pub fn up(&mut self) {
        match self.step {
            CreatePoolStep::Venue => {
                self.venue_idx = if self.venue_idx == 0 {
                    2
                } else {
                    self.venue_idx - 1
                };
            }
            CreatePoolStep::MintA if !self.mint_options.is_empty() => {
                self.mint_a_idx = if self.mint_a_idx == 0 {
                    self.mint_options.len() - 1
                } else {
                    self.mint_a_idx - 1
                };
            }
            CreatePoolStep::MintB if !self.mint_options.is_empty() => {
                self.mint_b_idx = if self.mint_b_idx == 0 {
                    self.mint_options.len() - 1
                } else {
                    self.mint_b_idx - 1
                };
            }
            CreatePoolStep::Mode => {
                self.mode = match self.mode {
                    CreatePoolMode::PoolOnly => CreatePoolMode::PoolDepositSwap,
                    CreatePoolMode::PoolDeposit => CreatePoolMode::PoolOnly,
                    CreatePoolMode::PoolDepositSwap => CreatePoolMode::PoolDeposit,
                };
            }
            _ => {}
        }
    }

    pub fn down(&mut self) {
        match self.step {
            CreatePoolStep::Venue => self.venue_idx = (self.venue_idx + 1) % 3,
            CreatePoolStep::MintA if !self.mint_options.is_empty() => {
                self.mint_a_idx = (self.mint_a_idx + 1) % self.mint_options.len();
            }
            CreatePoolStep::MintB if !self.mint_options.is_empty() => {
                self.mint_b_idx = (self.mint_b_idx + 1) % self.mint_options.len();
            }
            CreatePoolStep::Mode => self.mode = self.mode.next(),
            _ => {}
        }
    }

    pub fn input_char(&mut self, c: char) {
        let slot = match self.step {
            CreatePoolStep::FeeParam => Some(&mut self.fee_input),
            CreatePoolStep::Price => Some(&mut self.price_input),
            _ => None,
        };
        if let Some(s) = slot
            && (c.is_ascii_digit() || c == '.' || c == ',')
        {
            s.push(c);
            self.error = None;
        }
    }

    pub fn input_backspace(&mut self) {
        match self.step {
            CreatePoolStep::FeeParam => {
                self.fee_input.pop();
            }
            CreatePoolStep::Price => {
                self.price_input.pop();
            }
            _ => {}
        }
    }

    pub fn tab(&mut self) {
        if self.step == CreatePoolStep::Mode {
            self.mode = self.mode.next();
        }
    }

    pub fn enter_advance(&mut self) -> Result<Option<CreatePoolParams>, String> {
        match self.step {
            CreatePoolStep::Venue => {
                self.step = CreatePoolStep::MintA;
                // defaults per venue
                self.fee_input = match self.venue() {
                    Venue::OrcaWhirlpool => "64".into(),
                    Venue::RaydiumClmm => "3".into(),
                    Venue::MeteoraDlmm => "25".into(),
                };
                Ok(None)
            }
            CreatePoolStep::MintA => {
                self.step = CreatePoolStep::MintB;
                Ok(None)
            }
            CreatePoolStep::MintB => {
                if self.mint_a_idx == self.mint_b_idx {
                    return Err("mint A and B must differ".into());
                }
                self.step = CreatePoolStep::FeeParam;
                Ok(None)
            }
            CreatePoolStep::FeeParam => {
                let _ = self
                    .fee_input
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "fee param u16")?;
                self.step = CreatePoolStep::Price;
                Ok(None)
            }
            CreatePoolStep::Price => {
                let p: f64 = self
                    .price_input
                    .trim()
                    .parse()
                    .map_err(|_| "price must be a number")?;
                if !(p.is_finite() && p > 0.0) {
                    return Err("price must be > 0".into());
                }
                self.step = CreatePoolStep::Mode;
                Ok(None)
            }
            CreatePoolStep::Mode => {
                self.step = CreatePoolStep::Review;
                Ok(None)
            }
            CreatePoolStep::Review => Ok(Some(self.build_params()?)),
        }
    }

    fn build_params(&self) -> Result<CreatePoolParams, String> {
        let a = self
            .mint_options
            .get(self.mint_a_idx)
            .ok_or("mint A")?
            .clone();
        let b = self
            .mint_options
            .get(self.mint_b_idx)
            .ok_or("mint B")?
            .clone();
        let fee_param: u16 = self.fee_input.trim().parse().map_err(|_| "fee param")?;
        let initial_price: f64 = self.price_input.trim().parse().map_err(|_| "price")?;
        Ok(CreatePoolParams {
            venue: self.venue(),
            mint_a: a.mint,
            mint_b: b.mint,
            fee_param,
            initial_price,
            mode: self.mode,
            tick_lower: -64,
            tick_upper: 64,
            amount_a: 0,
            amount_b: 0,
            shape: DepositShape::Dual,
            dlmm_strategy: Some(DlmmStrategy::CurveBalanced),
            swap_slippage_bps: 50,
            sol_leg: SolLegMode::NativeSol,
            symbol_a: Some(a.symbol),
            symbol_b: Some(b.symbol),
            decimals_a: a.decimals,
            decimals_b: b.decimals,
        })
    }
}

/// Resolve WSOL mint string helper for tests / UI.
#[allow(dead_code)]
pub fn wsol_str() -> &'static str {
    WSOL
}

#[allow(dead_code)]
pub fn parse_mint(s: &str) -> Result<Pubkey, String> {
    Pubkey::from_str(s.trim()).map_err(|e| e.to_string())
}
