//! Jupiter / Titan swap routing for deposit+swap and compound (feature `write`).
//!
//! - **Jupiter:** Swap API **V2 Router** `GET https://api.jup.ag/swap/v2/build`
//!   (Metis raw ixs — composable into our tx v1). Not lite-api / not Metis v1.
//! - **Titan:** Portal `GET /api/v1/quote/swap` with `transactionFormat=1`
//!   (SIMD-0385 tx v1 · no ALTs) and short-form instructions (`p`/`a`/`d`).

use anyhow::{Context, Result, bail};
use base64::Engine;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::str::FromStr;

/// Settings swap router (parallel to USD price mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SwapMode {
    #[default]
    Jupiter,
    Titan,
}

impl SwapMode {
    pub const ALL: [SwapMode; 2] = [Self::Jupiter, Self::Titan];

    pub fn label(self) -> &'static str {
        match self {
            Self::Jupiter => "Jupiter Swap API v2 /build (Router)",
            Self::Titan => "Titan Portal swap (tx v1)",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Jupiter => "jupiter",
            Self::Titan => "titan",
        }
    }

    pub fn as_setting(self) -> &'static str {
        self.short()
    }

    pub fn from_setting(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "titan" => Self::Titan,
            _ => Self::Jupiter,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Jupiter => Self::Titan,
            Self::Titan => Self::Jupiter,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SwapQuote {
    pub in_mint: Pubkey,
    pub out_mint: Pubkey,
    pub in_amount: u64,
    pub out_amount: u64,
    /// Suggested total tip lamports (Jupiter prioritizationFeeLamports / Titan equiv).
    pub tip_lamports: Option<u64>,
    pub router: SwapMode,
    pub raw_note: String,
}

#[derive(Debug, Clone)]
pub struct SwapPlan {
    pub quote: SwapQuote,
    pub instructions: Vec<Instruction>,
}

/// Jupiter Swap API V2 · Router (raw ixs for custom tx assembly).
const JUPITER_BUILD: &str = "https://api.jup.ag/swap/v2/build";
const TITAN_PORTAL_QUOTE: &str = "https://portal.api.titan.exchange/api/v1/quote/swap";
const COMPUTE_BUDGET: &str = "ComputeBudget111111111111111111111111111111";

/// Fetch executable swap ixs for the selected router.
///
/// - Jupiter: `api.jup.ag/swap/v2/build` (API key recommended).
/// - Titan: Portal key **required**.
///
/// Both strip ComputeBudget ixs (we set CU via tx v1 `TransactionConfig`) and
/// refuse routes that still need address lookup tables (ALTs are v0-only).
pub fn fetch_swap_instructions(
    mode: SwapMode,
    api_key: Option<&str>,
    user: &Pubkey,
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
) -> Result<SwapPlan> {
    match mode {
        SwapMode::Jupiter => {
            jupiter_build_ixs(api_key, user, input_mint, output_mint, amount, slippage_bps)
        }
        SwapMode::Titan => {
            // Titan has no RTSE string — map Jupiter RTSE sentinel → fixed 50 bps default.
            let bps = if slippage_bps == u16::MAX {
                50
            } else {
                slippage_bps
            };
            titan_swap_ixs(api_key, user, input_mint, output_mint, amount, bps)
        }
    }
}

/// Jupiter Swap API V2 `/build` — single call, Metis Router raw instructions.
fn jupiter_build_ixs(
    api_key: Option<&str>,
    user: &Pubkey,
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
) -> Result<SwapPlan> {
    // Always false: we wrap native SOL before the swap and unwrap/close WSOL
    // after deposit/create/claim/withdraw — Jupiter must leave WSOL in the ATA.
    let wrap_and_unwrap_sol = false;

    // transactionVersion=1 → SIMD-0385 / no ALTs (matches our write path).
    // maxAccounts kept modest as a secondary size guard.
    // `u16::MAX` → Jupiter RTSE (Real-Time Slippage Estimator). Else fixed bps.
    let slip = if slippage_bps == u16::MAX {
        "rtse".to_string()
    } else {
        slippage_bps.to_string()
    };
    let url = format!(
        "{JUPITER_BUILD}?inputMint={input_mint}&outputMint={output_mint}&amount={amount}\
         &taker={user}&slippageBps={slip}&wrapAndUnwrapSol={wrap_and_unwrap_sol}\
         &maxAccounts=32&transactionVersion=1"
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("jupiter http client")?;
    let mut req = client.get(&url).header("Accept", "application/json");
    if let Some(k) = api_key.filter(|s| !s.is_empty()) {
        req = req.header("x-api-key", k);
    }
    let resp = req.send().context("jupiter /swap/v2/build")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        bail!("jupiter /build HTTP {status}: {body}");
    }
    let build_val: serde_json::Value = resp.json().context("jupiter /build json")?;

    let out_amount = parse_u64_flexible(build_val.get("outAmount")).unwrap_or(0);
    let in_amount = parse_u64_flexible(build_val.get("inAmount")).unwrap_or(amount);
    let applied_slip = parse_u64_flexible(build_val.get("slippageBps"))
        .or_else(|| {
            build_val
                .get("otherAmountThreshold")
                .and_then(|_| parse_u64_flexible(build_val.get("slippageBps")))
        })
        .map(|v| v as u16);
    let wrap_note = "wrapAndUnwrapSol=false · we wrap/unwrap";
    let slip_note = match (slippage_bps == u16::MAX, applied_slip) {
        (true, Some(b)) => {
            format!("jupiter RTSE → {b} bps · {wrap_note} · out≈{out_amount}")
        }
        (true, None) => format!("jupiter RTSE · {wrap_note} · out≈{out_amount}"),
        (false, _) => {
            format!("jupiter v2/build slip={slippage_bps} · {wrap_note} · out≈{out_amount}")
        }
    };

    // ALTs → v0 only; our write path packs into SIMD-0385 tx v1.
    if let Some(alts) = build_val
        .get("addressesByLookupTableAddress")
        .and_then(|v| v.as_object())
        && !alts.is_empty()
    {
        bail!(
            "jupiter /build route needs {} ALT(s) · incompatible with tx v1 packing · try smaller amount or Titan",
            alts.len()
        );
    }

    let mut instructions = Vec::new();
    // Skip computeBudgetInstructions — CU lives in TransactionConfig.
    push_ix_array(&mut instructions, build_val.get("setupInstructions"))?;
    if let Some(ix) = build_val.get("swapInstruction") {
        let ix = json_ix_long(ix)?;
        if !is_compute_budget(&ix) {
            instructions.push(ix);
        }
    }
    // Never append cleanupInstruction (would unwrap WSOL) — we unwrap ourselves.
    let _ = build_val.get("cleanupInstruction");
    push_ix_array(&mut instructions, build_val.get("otherInstructions"))?;
    // tipInstruction is for Jupiter /submit — omit when we land ourselves.

    if instructions.is_empty() {
        bail!("jupiter /build returned no swap instructions");
    }

    Ok(SwapPlan {
        quote: SwapQuote {
            in_mint: *input_mint,
            out_mint: *output_mint,
            in_amount,
            out_amount,
            tip_lamports: None,
            router: SwapMode::Jupiter,
            raw_note: slip_note,
        },
        instructions,
    })
}

fn push_ix_array(out: &mut Vec<Instruction>, v: Option<&serde_json::Value>) -> Result<()> {
    let Some(arr) = v.and_then(|x| x.as_array()) else {
        return Ok(());
    };
    for ix in arr {
        let ix = json_ix_long(ix)?;
        if !is_compute_budget(&ix) {
            out.push(ix);
        }
    }
    Ok(())
}

fn parse_u64_flexible(v: Option<&serde_json::Value>) -> Option<u64> {
    let v = v?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_f64().map(|f| f as u64))
}

/// Titan Portal executable route · **tx v1** (`transactionFormat=1`, no ALTs).
fn titan_swap_ixs(
    api_key: Option<&str>,
    user: &Pubkey,
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
) -> Result<SwapPlan> {
    let key = api_key
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("Titan swap needs an API key · settings → Titan · developer.titan.exchange")?;

    // Keep WSOL wrapped when depositing into a WSOL CLMM/DLMM pool next.
    let output_wsol = *output_mint
        == Pubkey::from_str("So11111111111111111111111111111111111111112").expect("wsol");

    let url = format!(
        "{TITAN_PORTAL_QUOTE}?inputMint={input_mint}&outputMint={output_mint}&amount={amount}\
         &userPublicKey={user}&slippageBps={slippage_bps}&numQuotes=3\
         &transactionFormat=1&titanSwapVersion=3&createOutputTokenAccount=true\
         &outputWsol={}&addSizeConstraint=true&sizeConstraint=3500&simulate=true",
        if output_wsol { "true" } else { "false" }
    );

    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("titan http client")?
        .get(&url)
        .header("x-api-key", key)
        .header("Accept", "application/json")
        .header("User-Agent", "sol-clmm-manager/0.1 (write; titan swap)")
        .send()
        .context("titan quote/swap HTTP")?;

    let status = resp.status();
    if status.as_u16() == 401 {
        bail!("titan invalid API key (401)");
    }
    if status.as_u16() == 429 {
        bail!("titan 429 · sandbox ~1 RPS · wait or upgrade");
    }
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        bail!("titan HTTP {status}: {body}");
    }

    let value: serde_json::Value = resp.json().context("titan swap JSON")?;
    let meta = value.get("metadata");
    let winner = meta
        .and_then(|m| {
            m.get("ExpectedWinner")
                .or_else(|| m.get("expectedWinner"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string());

    let quotes = value
        .get("quotes")
        .and_then(|q| q.as_object())
        .context("titan quotes missing")?;

    let (provider, route) = if let Some(w) = winner.as_deref() {
        quotes
            .get(w)
            .map(|r| (w.to_string(), r))
            .context("titan ExpectedWinner route missing")?
    } else {
        let (k, v) = quotes.iter().next().context("titan no routes")?;
        (k.clone(), v)
    };

    let out_amount = route
        .get("outAmount")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .context("titan outAmount")?;
    let in_amount = route
        .get("inAmount")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .unwrap_or(amount);

    let tip = route
        .get("prioritizationFeeLamports")
        .and_then(|v| v.as_u64())
        .or_else(|| meta.and_then(|m| m.get("prioritizationFeeLamports").and_then(|v| v.as_u64())));

    let raw_ixs = route
        .get("instructions")
        .and_then(|v| v.as_array())
        .context("titan route.instructions missing")?;

    let mut instructions = Vec::with_capacity(raw_ixs.len());
    for ix in raw_ixs {
        let ix = json_ix_short(ix).or_else(|_| json_ix_long(ix))?;
        if !is_compute_budget(&ix) {
            instructions.push(ix);
        }
    }
    if instructions.is_empty() {
        bail!("titan route {provider} returned no instructions");
    }

    // V1 path must not depend on ALTs — refuse if the provider still listed any.
    if let Some(alts) = route.get("addressLookupTables").and_then(|v| v.as_array())
        && !alts.is_empty()
    {
        bail!(
            "titan route {provider} still references {} ALTs · need transactionFormat=1 without ALTs",
            alts.len()
        );
    }

    let titan_slip = route
        .get("slippageBps")
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        .or_else(|| meta.and_then(|m| m.get("slippageBps").and_then(|v| v.as_u64())))
        .map(|v| v as u16);
    let slip_note = match titan_slip {
        Some(b) if b != slippage_bps => {
            format!("titan/{provider} slip {slippage_bps}→{b} bps · out≈{out_amount} · tx v1")
        }
        _ => format!("titan/{provider} slip={slippage_bps} · out≈{out_amount} · tx v1"),
    };

    Ok(SwapPlan {
        quote: SwapQuote {
            in_mint: *input_mint,
            out_mint: *output_mint,
            in_amount,
            out_amount,
            tip_lamports: tip,
            router: SwapMode::Titan,
            raw_note: slip_note,
        },
        instructions,
    })
}

fn is_compute_budget(ix: &Instruction) -> bool {
    ix.program_id.to_string() == COMPUTE_BUDGET
}

/// Jupiter-style long keys: programId / accounts[{pubkey,isSigner,isWritable}] / data.
fn json_ix_long(v: &serde_json::Value) -> Result<Instruction> {
    let program_id = Pubkey::from_str(
        v.get("programId")
            .and_then(|x| x.as_str())
            .context("ix programId")?,
    )?;
    let accounts = v
        .get("accounts")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let mut metas = Vec::with_capacity(accounts.len());
    for a in accounts {
        let pubkey = Pubkey::from_str(a.get("pubkey").and_then(|x| x.as_str()).context("acct")?)?;
        let is_signer = a.get("isSigner").and_then(|x| x.as_bool()).unwrap_or(false);
        let is_writable = a
            .get("isWritable")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        metas.push(if is_writable {
            AccountMeta::new(pubkey, is_signer)
        } else {
            AccountMeta::new_readonly(pubkey, is_signer)
        });
    }
    let data_b64 = v.get("data").and_then(|x| x.as_str()).unwrap_or("");
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .unwrap_or_default();
    Ok(Instruction {
        program_id,
        accounts: metas,
        data,
    })
}

/// Titan short keys: p / a[{p,s,w}] / d.
fn json_ix_short(v: &serde_json::Value) -> Result<Instruction> {
    let program_id = Pubkey::from_str(v.get("p").and_then(|x| x.as_str()).context("ix p")?)?;
    let accounts = v
        .get("a")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let mut metas = Vec::with_capacity(accounts.len());
    for a in accounts {
        let pubkey = Pubkey::from_str(a.get("p").and_then(|x| x.as_str()).context("acct p")?)?;
        let is_signer = a.get("s").and_then(|x| x.as_bool()).unwrap_or(false);
        let is_writable = a.get("w").and_then(|x| x.as_bool()).unwrap_or(false);
        metas.push(if is_writable {
            AccountMeta::new(pubkey, is_signer)
        } else {
            AccountMeta::new_readonly(pubkey, is_signer)
        });
    }
    let data_b64 = v.get("d").and_then(|x| x.as_str()).unwrap_or("");
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .context("titan ix data base64")?;
    Ok(Instruction {
        program_id,
        accounts: metas,
        data,
    })
}
