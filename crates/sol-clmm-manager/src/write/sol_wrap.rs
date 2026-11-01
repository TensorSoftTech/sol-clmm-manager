//! Native SOL ↔ WSOL for pools that use `So111…112`.
//!
//! Policy: treat the SOL leg as **native SOL**. Jupiter/Titan keep
//! `wrapAndUnwrapSol=false` / WSOL out — we [`wrap_sol_ixs`] at the start and
//! [`unwrap_sol_ix`] (close ATA) at the end of deposit / create / claim /
//! withdraw. Manage also exposes standalone wrap / unwrap.

use crate::stables::{WSOL, wsol_mint};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::str::FromStr;

const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const SYNC_NATIVE: u8 = 17;
const CLOSE_ACCOUNT: u8 = 9;

/// How the user funds / receives the SOL leg of a WSOL pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SolLegMode {
    /// Use wrapped SOL already in the WSOL ATA (no wrap/unwrap).
    #[default]
    Wsol,
    /// Native SOL: wrap before deposit / unwrap after withdraw or claim.
    NativeSol,
}

impl SolLegMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Wsol => "WSOL · use existing wrapped balance",
            Self::NativeSol => "SOL · wrap / unwrap native lamports",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Wsol => Self::NativeSol,
            Self::NativeSol => Self::Wsol,
        }
    }
}

pub fn is_wsol(mint: &Pubkey) -> bool {
    *mint == wsol_mint() || mint.to_string() == WSOL
}

pub fn pool_has_wsol(mint_a: &Pubkey, mint_b: &Pubkey) -> bool {
    is_wsol(mint_a) || is_wsol(mint_b)
}

fn token_program() -> Pubkey {
    Pubkey::from_str(TOKEN_PROGRAM).expect("token")
}

fn system_program() -> Pubkey {
    Pubkey::from_str(SYSTEM_PROGRAM).expect("system")
}

fn associated_token_program() -> Pubkey {
    Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM).expect("ata")
}

pub fn wsol_ata(owner: &Pubkey) -> Pubkey {
    let token = token_program();
    let associated = associated_token_program();
    let wsol = wsol_mint();
    Pubkey::find_program_address(
        &[owner.as_ref(), token.as_ref(), wsol.as_ref()],
        &associated,
    )
    .0
}

/// CreateIdempotent ATA for any mint (SPL associated token).
pub fn create_ata_idempotent_ix(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    let token = token_program();
    let associated = associated_token_program();
    let ata = Pubkey::find_program_address(
        &[owner.as_ref(), token.as_ref(), mint.as_ref()],
        &associated,
    )
    .0;
    Instruction {
        program_id: associated,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(system_program(), false),
            AccountMeta::new_readonly(token, false),
        ],
        // CreateIdempotent discriminator = 1
        data: vec![1],
    }
}

/// Create idempotent ATA for WSOL.
pub fn create_wsol_ata_idempotent_ix(payer: &Pubkey, owner: &Pubkey) -> Instruction {
    create_ata_idempotent_ix(payer, owner, &wsol_mint())
}

/// Ensure ATAs exist before claim/withdraw (fixes Anchor 3012 AccountNotInitialized).
pub fn ensure_atas_ixs(payer: &Pubkey, owner: &Pubkey, mints: &[Pubkey]) -> Vec<Instruction> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for mint in mints {
        if *mint == Pubkey::default() || !seen.insert(*mint) {
            continue;
        }
        out.push(create_ata_idempotent_ix(payer, owner, mint));
    }
    out
}

/// CreateIdempotent for both pool token ATAs (incl. WSOL after a prior unwrap closed it).
pub fn ensure_pool_atas_ixs(owner: &Pubkey, mint_a: &Pubkey, mint_b: &Pubkey) -> Vec<Instruction> {
    ensure_atas_ixs(owner, owner, &[*mint_a, *mint_b])
}

/// System transfer lamports into the WSOL ATA, then `SyncNative`.
pub fn wrap_sol_ixs(owner: &Pubkey, lamports: u64) -> Vec<Instruction> {
    let ata = wsol_ata(owner);
    let transfer = Instruction {
        program_id: system_program(),
        accounts: vec![AccountMeta::new(*owner, true), AccountMeta::new(ata, false)],
        data: {
            // SystemInstruction::Transfer { lamports }
            let mut d = vec![2, 0, 0, 0];
            d.extend_from_slice(&lamports.to_le_bytes());
            d
        },
    };
    let sync = Instruction {
        program_id: token_program(),
        accounts: vec![AccountMeta::new(ata, false)],
        data: vec![SYNC_NATIVE],
    };
    vec![create_wsol_ata_idempotent_ix(owner, owner), transfer, sync]
}

/// Close WSOL ATA → unwrap remaining balance to native SOL (`CloseAccount`).
pub fn unwrap_sol_ix(owner: &Pubkey) -> Instruction {
    let ata = wsol_ata(owner);
    Instruction {
        program_id: token_program(),
        accounts: vec![
            AccountMeta::new(ata, false),
            AccountMeta::new(*owner, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data: vec![CLOSE_ACCOUNT],
    }
}

/// Prefix wrap ixs when depositing with [`SolLegMode::NativeSol`] into a WSOL pool.
pub fn maybe_wrap_before_deposit(
    mode: SolLegMode,
    owner: &Pubkey,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
    lamports: u64,
) -> Vec<Instruction> {
    if mode != SolLegMode::NativeSol || !pool_has_wsol(mint_a, mint_b) || lamports == 0 {
        return Vec::new();
    }
    wrap_sol_ixs(owner, lamports)
}

/// Associated Token Program id (ATokenGPv…).
pub fn associated_token_program_id() -> Pubkey {
    associated_token_program()
}

fn is_ata_create_idempotent(ix: &Instruction) -> bool {
    ix.program_id == associated_token_program() && ix.data.first() == Some(&1)
}

/// Drop duplicate `CreateIdempotent` for the same ATA (account[1]).
/// Wrap + Jupiter/Titan setup often both emit WSOL ATA create.
pub fn dedupe_ata_create_idempotent(ixs: &mut Vec<Instruction>) {
    let mut seen = std::collections::HashSet::new();
    ixs.retain(|ix| {
        if !is_ata_create_idempotent(ix) {
            return true;
        }
        let Some(ata) = ix.accounts.get(1).map(|a| a.pubkey) else {
            return true;
        };
        seen.insert(ata)
    });
}

/// Suffix unwrap when claiming/withdrawing from a WSOL pool and user chose native SOL.
pub fn maybe_unwrap_after(
    mode: SolLegMode,
    owner: &Pubkey,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
) -> Vec<Instruction> {
    if mode != SolLegMode::NativeSol || !pool_has_wsol(mint_a, mint_b) {
        return Vec::new();
    }
    vec![unwrap_sol_ix(owner)]
}
