//! Wallet SPL / native SOL balances for deposit caps and wallet management.

use crate::clmm_math::read_token_account_amount;
use crate::rpc::{Rpc, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
use crate::stables::wsol_mint;
use crate::store::Store;
use crate::tokens::{TokenInfo, resolve_mints};
use anyhow::{Context, Result};
use solana_account_decoder_client_types::UiAccountData;
use solana_client::rpc_request::TokenAccountsFilter;
use solana_pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;

const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// Lamports kept for fees / rent when spending native SOL into a WSOL pool leg.
pub const SOL_FEE_RESERVE_LAMPORTS: u64 = 10_000_000; // 0.01 SOL

#[derive(Debug, Clone, Copy)]
pub struct SideBalance {
    pub mint: Pubkey,
    /// ATA token amount (0 if no account).
    pub ata_amount: u64,
    pub is_wsol: bool,
}

/// Wallet balances for a pool's two mints (+ native SOL).
#[derive(Debug, Clone)]
pub struct WalletPoolBalances {
    pub a: SideBalance,
    pub b: SideBalance,
    pub sol_lamports: u64,
    pub rent_reserve_lamports: u64,
}

impl WalletPoolBalances {
    /// Max raw amount depositable for side A/B.
    ///
    /// Non-WSOL: ATA balance.
    /// WSOL + `use_native_sol=false`: WSOL ATA only.
    /// WSOL + `use_native_sol=true`: (native SOL − reserve) + WSOL ATA
    /// (wrap then deposit from the combined WSOL ATA).
    pub fn max_depositable(&self, side_a: bool, use_native_sol: bool) -> u64 {
        let side = if side_a { &self.a } else { &self.b };
        if side.is_wsol {
            if use_native_sol {
                let native = self.sol_lamports.saturating_sub(self.rent_reserve_lamports);
                native.saturating_add(side.ata_amount)
            } else {
                side.ata_amount
            }
        } else {
            side.ata_amount
        }
    }

    pub fn clamp(&self, side_a: bool, amount: u64, use_native_sol: bool) -> u64 {
        amount.min(self.max_depositable(side_a, use_native_sol))
    }

    /// Short UI line, e.g. `SOL 1.28 · USDC 100` (native + wrapped combined as SOL).
    pub fn summary_line(&self, sym_a: &str, sym_b: &str, dec_a: u8, dec_b: u8) -> String {
        format!(
            "{} · {}",
            format_side(&self.a, sym_a, dec_a, self.sol_lamports),
            format_side(&self.b, sym_b, dec_b, self.sol_lamports),
        )
    }
}

fn format_side(side: &SideBalance, symbol: &str, decimals: u8, sol_lamports: u64) -> String {
    if side.is_wsol {
        let total = sol_lamports.saturating_add(side.ata_amount);
        if side.ata_amount == 0 {
            format!("SOL {}", format_ui(sol_lamports, 9))
        } else {
            format!(
                "SOL {} ({} wrapped)",
                format_ui(total, 9),
                format_ui(side.ata_amount, 9),
            )
        }
    } else {
        format!("{symbol} {}", format_ui(side.ata_amount, decimals))
    }
}

fn format_ui(raw: u64, decimals: u8) -> String {
    if decimals == 0 {
        return raw.to_string();
    }
    let div = 10u128.pow(decimals as u32);
    let whole = raw as u128 / div;
    let frac = raw as u128 % div;
    if frac == 0 {
        return whole.to_string();
    }
    let frac_s = format!("{frac:0width$}", width = decimals as usize);
    let frac_trim = frac_s.trim_end_matches('0');
    if frac_trim.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac_trim}")
    }
}

fn associated_token_program() -> Pubkey {
    Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM).expect("ata program")
}

fn token_program() -> Pubkey {
    Pubkey::from_str(TOKEN_PROGRAM_ID).expect("token")
}

fn token_2022_program() -> Pubkey {
    Pubkey::from_str(TOKEN_2022_PROGRAM_ID).expect("token2022")
}

pub fn ata_address(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &associated_token_program(),
    )
    .0
}

fn mint_token_program(rpc: &Rpc<'_>, mint: &Pubkey) -> Pubkey {
    match rpc.get_account(mint) {
        Ok(acc) if acc.owner == token_2022_program() => token_2022_program(),
        _ => token_program(),
    }
}

fn read_ata_amount(rpc: &Rpc<'_>, owner: &Pubkey, mint: &Pubkey) -> Result<u64> {
    let program = mint_token_program(rpc, mint);
    let ata = ata_address(owner, mint, &program);
    match rpc.get_account(&ata) {
        Ok(acc) => Ok(read_token_account_amount(&acc.data).unwrap_or(0)),
        Err(_) => Ok(0),
    }
}

/// Fetch ATA balances for both pool mints + native SOL.
pub fn fetch_wallet_pool_balances(
    rpc: &Rpc<'_>,
    owner: &Pubkey,
    mint_a: Pubkey,
    mint_b: Pubkey,
) -> Result<WalletPoolBalances> {
    let sol_lamports = rpc.get_balance(owner).context("get SOL balance")?;
    let amount_a = read_ata_amount(rpc, owner, &mint_a)?;
    let amount_b = read_ata_amount(rpc, owner, &mint_b)?;
    let wsol = wsol_mint();
    Ok(WalletPoolBalances {
        a: SideBalance {
            mint: mint_a,
            ata_amount: amount_a,
            is_wsol: mint_a == wsol,
        },
        b: SideBalance {
            mint: mint_b,
            ata_amount: amount_b,
            is_wsol: mint_b == wsol,
        },
        sol_lamports,
        rent_reserve_lamports: SOL_FEE_RESERVE_LAMPORTS,
    })
}

/// One wallet SPL balance row (human label filled by caller / resolve).
#[derive(Debug, Clone)]
pub struct WalletTokenBalance {
    pub mint: Pubkey,
    pub amount: u64,
    pub symbol: String,
    pub decimals: u8,
    pub is_wsol: bool,
}

/// Native SOL + every non-zero SPL / Token-2022 ATA (amount > 0).
pub fn fetch_wallet_token_balances(
    rpc: &Rpc<'_>,
    owner: &Pubkey,
    store: Option<&Store>,
) -> Result<(u64, Vec<WalletTokenBalance>)> {
    let sol_lamports = rpc.get_balance(owner).context("get SOL balance")?;
    let mut by_mint: HashMap<Pubkey, u64> = HashMap::new();
    for program_str in [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
        let program = Pubkey::from_str(program_str).context("token program")?;
        let accounts = rpc
            .get_token_accounts_by_owner(owner, TokenAccountsFilter::ProgramId(program))
            .with_context(|| format!("token accounts ({program_str})"))?;
        for keyed in accounts {
            if let Some((mint, amount)) = parse_token_amount_mint(&keyed.account.data)
                && amount > 0
            {
                *by_mint.entry(mint).or_default() = by_mint
                    .get(&mint)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(amount);
            }
        }
    }

    let mints: Vec<Pubkey> = by_mint.keys().copied().collect();
    let resolved = resolve_mints(rpc, &mints, store, false).unwrap_or_default();
    let wsol = wsol_mint();
    let mut rows: Vec<WalletTokenBalance> = by_mint
        .into_iter()
        .map(|(mint, amount)| {
            let info = resolved.get(&mint).cloned().unwrap_or_else(|| TokenInfo {
                mint,
                symbol: mint.to_string().chars().take(4).collect(),
                decimals: 0,
            });
            WalletTokenBalance {
                mint,
                amount,
                symbol: if mint == wsol {
                    "WSOL".into()
                } else {
                    info.symbol
                },
                decimals: if mint == wsol { 9 } else { info.decimals },
                is_wsol: mint == wsol,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.amount
            .cmp(&a.amount)
            .then(a.symbol.cmp(&b.symbol))
            .then(a.mint.to_string().cmp(&b.mint.to_string()))
    });
    Ok((sol_lamports, rows))
}

fn parse_token_amount_mint(data: &UiAccountData) -> Option<(Pubkey, u64)> {
    let UiAccountData::Json(parsed) = data else {
        return None;
    };
    let info = parsed.parsed.get("info")?;
    let amount_s = info
        .pointer("/tokenAmount/amount")
        .and_then(|v| v.as_str())
        .or_else(|| info.get("tokenAmount")?.get("amount")?.as_str())?;
    let amount: u64 = amount_s.parse().ok()?;
    let mint = Pubkey::from_str(info.get("mint")?.as_str()?).ok()?;
    Some((mint, amount))
}
