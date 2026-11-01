//! Thin RPC helpers + session call meter (counter + recent history).

use anyhow::{Context, Result};
use solana_account::Account;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::RpcFilterType;
use solana_client::rpc_request::TokenAccountsFilter;
use solana_client::rpc_response::RpcKeyedAccount;
use solana_pubkey::Pubkey;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::str::FromStr;
use std::time::Duration;

pub const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

/// Cap of remembered calls for the bottom history strip.
pub const RPC_HISTORY_CAP: usize = 64;

/// Known public / rate-limited Solana endpoints.
const PUBLIC_RPC_HOSTS: &[&str] = &[
    "api.mainnet-beta.solana.com",
    "api.devnet.solana.com",
    "api.testnet.solana.com",
];

pub fn connect(rpc_url: &str) -> RpcClient {
    RpcClient::new_with_timeout(rpc_url.to_string(), Duration::from_secs(30))
}

pub fn is_public_rpc(rpc_url: &str) -> bool {
    let host = rpc_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    PUBLIC_RPC_HOSTS.iter().any(|h| host == *h)
}

#[derive(Debug, Clone)]
pub struct RpcCall {
    pub method: String,
    pub note: String,
    pub ok: bool,
}

impl RpcCall {
    pub fn label(&self) -> String {
        let mark = if self.ok { "" } else { "!" };
        if self.note.is_empty() {
            format!("{}{mark}", self.method)
        } else {
            format!("{}({}{})", self.method, self.note, if self.ok { "" } else { "!" })
        }
    }
}

/// Session-scoped call counter + ring history (Solana RPC + Jupiter/Titan HTTPS).
#[derive(Debug, Default)]
pub struct RpcMeter {
    inner: RefCell<RpcMeterInner>,
}

#[derive(Debug, Default)]
struct RpcMeterInner {
    total: u64,
    solana: u64,
    jupiter: u64,
    titan: u64,
    history: VecDeque<RpcCall>,
}

impl RpcMeter {
    pub fn record(&self, method: &str, note: impl Into<String>, ok: bool) {
        let mut g = self.inner.borrow_mut();
        g.total = g.total.saturating_add(1);
        if method.starts_with("jupiter.") {
            g.jupiter = g.jupiter.saturating_add(1);
        } else if method.starts_with("titan.") {
            g.titan = g.titan.saturating_add(1);
        } else {
            g.solana = g.solana.saturating_add(1);
        }
        if g.history.len() >= RPC_HISTORY_CAP {
            g.history.pop_front();
        }
        g.history.push_back(RpcCall {
            method: method.to_string(),
            note: note.into(),
            ok,
        });
    }

    pub fn total(&self) -> u64 {
        self.inner.borrow().total
    }

    pub fn counts(&self) -> (u64, u64, u64, u64) {
        let g = self.inner.borrow();
        (g.total, g.solana, g.jupiter, g.titan)
    }

    pub fn history(&self) -> Vec<RpcCall> {
        self.inner.borrow().history.iter().cloned().collect()
    }

    /// Merge another meter's counts + history (e.g. after a background fetch).
    pub fn merge(&self, other: &RpcMeter) {
        let o = other.inner.borrow();
        let mut g = self.inner.borrow_mut();
        g.total = g.total.saturating_add(o.total);
        g.solana = g.solana.saturating_add(o.solana);
        g.jupiter = g.jupiter.saturating_add(o.jupiter);
        g.titan = g.titan.saturating_add(o.titan);
        for c in &o.history {
            if g.history.len() >= RPC_HISTORY_CAP {
                g.history.pop_front();
            }
            g.history.push_back(c.clone());
        }
    }

    /// Panel title: totals broken down by backend.
    pub fn footer_title(&self) -> String {
        let (total, sol, jup, titan) = self.counts();
        format!(" calls · {total} · sol×{sol} · jupiter×{jup} · titan×{titan} ")
    }

    /// Compact one-line summary for the footer (newest last).
    pub fn footer_line(&self, max_chars: usize) -> String {
        let g = self.inner.borrow();
        let head = format!("hist {} · ", g.total);
        if g.history.is_empty() {
            return format!("{head}(none yet)");
        }
        let labels: Vec<String> = g.history.iter().map(|c| c.label()).collect();
        let joined = labels.join(" · ");
        let budget = max_chars.saturating_sub(head.len());
        if joined.len() <= budget {
            return format!("{head}{joined}");
        }
        // Keep the newest tail that fits.
        let mut acc = String::new();
        for lab in labels.iter().rev() {
            let next = if acc.is_empty() {
                lab.clone()
            } else {
                format!("{lab} · {acc}")
            };
            if next.len() > budget.saturating_sub(1) {
                break;
            }
            acc = next;
        }
        format!("{head}…{acc}")
    }
}

/// Traced view of an [`RpcClient`] — every method bumps the session meter.
pub struct Rpc<'a> {
    client: &'a RpcClient,
    meter: &'a RpcMeter,
}

impl<'a> Rpc<'a> {
    pub fn new(client: &'a RpcClient, meter: &'a RpcMeter) -> Self {
        Self { client, meter }
    }

    pub fn meter(&self) -> &RpcMeter {
        self.meter
    }

    pub fn get_slot(&self) -> Result<u64> {
        let r = self.client.get_slot();
        self.meter.record("get_slot", "", r.is_ok());
        r.context("get_slot")
    }

    pub fn get_balance(&self, pubkey: &Pubkey) -> Result<u64> {
        let r = self.client.get_balance(pubkey);
        self.meter.record("get_balance", short_pk(pubkey), r.is_ok());
        r.context("get_balance")
    }

    pub fn get_account(&self, pubkey: &Pubkey) -> Result<Account> {
        let r = self.client.get_account(pubkey);
        self.meter.record("get_account", short_pk(pubkey), r.is_ok());
        r.with_context(|| format!("get_account {}", pubkey))
    }

    pub fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Option<Account>>> {
        let r = self.client.get_multiple_accounts(pubkeys);
        self.meter
            .record("get_multiple_accounts", format!("n={}", pubkeys.len()), r.is_ok());
        r.context("get_multiple_accounts")
    }

    pub fn get_token_accounts_by_owner(
        &self,
        owner: &Pubkey,
        filter: TokenAccountsFilter,
    ) -> Result<Vec<RpcKeyedAccount>> {
        let note = match &filter {
            TokenAccountsFilter::ProgramId(p) => short_pk(p),
            TokenAccountsFilter::Mint(m) => format!("mint:{}", short_pk(m)),
        };
        let r = self.client.get_token_accounts_by_owner(owner, filter);
        self.meter
            .record("get_token_accounts_by_owner", note, r.is_ok());
        r.context("get_token_accounts_by_owner")
    }

    /// `getProgramAccounts` with filters; returns decoded binary accounts.
    pub fn get_program_accounts_with_config(
        &self,
        program: &Pubkey,
        filters: Vec<RpcFilterType>,
    ) -> Result<Vec<(Pubkey, Account)>> {
        let config = RpcProgramAccountsConfig {
            filters: Some(filters),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                ..RpcAccountInfoConfig::default()
            },
            ..RpcProgramAccountsConfig::default()
        };
        let r = self
            .client
            .get_program_ui_accounts_with_config(program, config);
        self.meter.record(
            "get_program_accounts",
            short_pk(program),
            r.is_ok(),
        );
        let ui = r.context("get_program_accounts")?;
        let mut out = Vec::with_capacity(ui.len());
        for (pk, ui_acc) in ui {
            let Some(acc) = ui_acc.to_account() else {
                continue;
            };
            out.push((pk, acc));
        }
        Ok(out)
    }

    pub fn get_multiple_accounts_chunked(
        &self,
        keys: &[Pubkey],
    ) -> Result<Vec<Option<Account>>> {
        const CHUNK: usize = 100;
        let mut out = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(CHUNK) {
            out.extend(self.get_multiple_accounts(chunk)?);
        }
        Ok(out)
    }
}

/// SPL token accounts owned by `wallet` with amount == 1 (position NFTs, etc.).
pub fn owned_nft_mints(rpc: &Rpc<'_>, wallet: &Pubkey) -> Result<Vec<Pubkey>> {
    let mut mints = HashSet::new();
    for program_str in [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
        let program = Pubkey::from_str(program_str).context("token program id")?;
        let accounts = rpc
            .get_token_accounts_by_owner(wallet, TokenAccountsFilter::ProgramId(program))
            .with_context(|| format!("get_token_accounts_by_owner ({program_str})"))?;
        for keyed in accounts {
            if let Some(mint) = parse_amount1_mint(&keyed.account.data) {
                mints.insert(mint);
            }
        }
    }
    Ok(mints.into_iter().collect())
}

fn parse_amount1_mint(data: &solana_account_decoder_client_types::UiAccountData) -> Option<Pubkey> {
    use solana_account_decoder_client_types::UiAccountData;
    let UiAccountData::Json(parsed) = data else {
        return None;
    };
    let info = parsed.parsed.get("info")?;
    let amount = info
        .pointer("/tokenAmount/amount")
        .and_then(|v| v.as_str())
        .or_else(|| info.get("tokenAmount")?.get("amount")?.as_str())?;
    if amount != "1" {
        return None;
    }
    let mint = info.get("mint")?.as_str()?;
    Pubkey::from_str(mint).ok()
}

pub fn fetch_status(rpc: &Rpc<'_>, wallet: Option<&Pubkey>) -> Result<RpcStatus> {
    let slot = rpc.get_slot()?;
    let balance_lamports = match wallet {
        Some(pk) => Some(rpc.get_balance(pk)?),
        None => None,
    };
    Ok(RpcStatus {
        slot,
        balance_lamports,
    })
}

#[derive(Debug, Clone)]
pub struct RpcStatus {
    pub slot: u64,
    pub balance_lamports: Option<u64>,
}

pub fn lamports_to_sol(lamports: u64) -> f64 {
    lamports as f64 / 1_000_000_000.0
}

fn short_pk(pk: &Pubkey) -> String {
    let s = pk.to_string();
    if s.len() <= 8 {
        s
    } else {
        format!("{}…{}", &s[..4], &s[s.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_counts_and_caps_history() {
        let m = RpcMeter::default();
        for i in 0..70 {
            m.record("get_slot", format!("{i}"), true);
        }
        assert_eq!(m.total(), 70);
        assert_eq!(m.history().len(), RPC_HISTORY_CAP);
        let line = m.footer_line(80);
        assert!(line.starts_with("hist 70"));
        assert!(line.contains('…') || line.len() <= 80);
        m.record("jupiter.price", "n=2", true);
        m.record("titan.quote", "mint", true);
        let (_, sol, jup, titan) = m.counts();
        assert_eq!(sol, 70);
        assert_eq!(jup, 1);
        assert_eq!(titan, 1);
    }
}
