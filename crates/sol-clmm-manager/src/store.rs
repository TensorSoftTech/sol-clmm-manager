//! Local SQLite config + token metadata cache.
//!
//! Default DB: `~/.config/sol-clmm-manager/sol-clmm.db`
//! Active-path pointer: `~/.config/sol-clmm-manager/active_db`

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const APP_DIR_NAME: &str = "sol-clmm-manager";
pub const DEFAULT_DB_FILE: &str = "sol-clmm.db";
pub const ACTIVE_POINTER_FILE: &str = "active_db";

const KEY_WALLET: &str = "wallet_pubkey";
/// Filesystem path only — never keypair secret bytes.
const KEY_KEYPAIR: &str = "keypair_path";
const KEY_RPC: &str = "rpc_url";
const KEY_PRIVACY: &str = "privacy_trim";
const KEY_PRICE_MODE: &str = "price_mode";
const KEY_JUPITER_API: &str = "jupiter_api_key";
const KEY_TITAN_API: &str = "titan_api_key";
const KEY_API_PAID_FAST: &str = "api_paid_fast";
const KEY_WRITE_ALLOWED: &str = "write_allowed";
const KEY_WRITE_PROMPT_SKIP: &str = "write_prompt_skip";
const KEY_SWAP_MODE: &str = "swap_mode";
const KEY_BUDGET_MODE: &str = "tx_budget_mode";
const KEY_CU_LIMIT: &str = "tx_cu_limit";
const KEY_TIP_LAMPORTS: &str = "tx_tip_lamports";
const KEY_LOADED_DATA: &str = "tx_loaded_data";
const KEY_HEAP: &str = "tx_heap";
const KEY_CU_MARGIN_BPS: &str = "tx_cu_margin_bps";
const KEY_TIP_FLOOR: &str = "tx_tip_floor";
const KEY_TIP_CEILING: &str = "tx_tip_ceiling";
const KEY_DLMM_STRATEGY: &str = "dlmm_strategy";

#[derive(Debug, Clone, Default)]
pub struct AppSettings {
    pub wallet_pubkey: Option<String>,
    /// Path to JSON keypair file (not the secret).
    pub keypair_path: Option<String>,
    pub rpc_url: Option<String>,
    pub privacy_trim: bool,
    /// `onchain` | `jupiter` | `titan`
    pub price_mode: Option<String>,
    pub jupiter_api_key: Option<String>,
    pub titan_api_key: Option<String>,
    /// Use short throttle when API keys are set (paid / higher plan).
    pub api_paid_fast: bool,
    /// `None` = never asked; `Some(true/false)` = consent.
    pub write_allowed: Option<bool>,
    /// Skip the “allow write access?” prompt on startup.
    pub write_prompt_skip: bool,
    /// `jupiter` | `titan`
    pub swap_mode: Option<String>,
    /// `auto` | `manual` for tx v1 budget.
    pub tx_budget_mode: Option<String>,
    pub tx_cu_limit: Option<u32>,
    pub tx_tip_lamports: Option<u64>,
    pub tx_loaded_data: Option<u32>,
    pub tx_heap: Option<u32>,
    pub tx_cu_margin_bps: Option<u32>,
    pub tx_tip_floor: Option<u64>,
    pub tx_tip_ceiling: Option<u64>,
    /// Last DLMM deposit strategy short name.
    pub dlmm_strategy: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CachedToken {
    pub mint: String,
    pub symbol: String,
    pub decimals: u8,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(path: &Path, overwrite: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }

        if overwrite && path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("overwrite remove {}", path.display()))?;
        }

        let conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS tokens (
                mint       TEXT PRIMARY KEY NOT NULL,
                symbol     TEXT NOT NULL,
                decimals   INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS write_txs (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at         TEXT NOT NULL,
                signature          TEXT NOT NULL UNIQUE,
                op                 TEXT NOT NULL,
                venue              TEXT,
                position           TEXT,
                pool               TEXT,
                tip_lamports       INTEGER,
                fee_lamports       INTEGER,
                sol_spent_lamports INTEGER,
                summary            TEXT,
                paid_label         TEXT
            );
            ",
        )
        .context("migrate schema")?;

        crate::snapshots::migrate_snapshots(&conn)?;

        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        write_active_pointer(path)?;
        Ok(store)
    }

    /// Persist a pool snapshot and return fee/volume over the full retained chain.
    pub fn record_pool_snapshot(
        &self,
        snap: &crate::snapshots::PoolSnapshotInput,
        decimals_a: u8,
        decimals_b: u8,
        symbol_a: &str,
        symbol_b: &str,
    ) -> Result<Option<crate::snapshots::PoolPeriodReport>> {
        crate::snapshots::record_pool_snapshot(
            &self.conn, snap, decimals_a, decimals_b, symbol_a, symbol_b,
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// On-disk size of the SQLite file (bytes), if the path exists.
    pub fn file_size_bytes(&self) -> Option<u64> {
        fs::metadata(&self.path).ok().map(|m| m.len())
    }

    /// Compact size for the TUI footer (`12K`, `1.2M`, …).
    pub fn file_size_label(&self) -> String {
        match self.file_size_bytes() {
            Some(n) => format_db_size(n),
            None => "—".into(),
        }
    }

    pub fn load_settings(&self) -> Result<AppSettings> {
        Ok(AppSettings {
            wallet_pubkey: self.get_setting(KEY_WALLET)?,
            keypair_path: self.get_setting(KEY_KEYPAIR)?,
            rpc_url: self.get_setting(KEY_RPC)?,
            privacy_trim: self
                .get_setting(KEY_PRIVACY)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            price_mode: self.get_setting(KEY_PRICE_MODE)?,
            jupiter_api_key: self.get_setting(KEY_JUPITER_API)?,
            titan_api_key: self.get_setting(KEY_TITAN_API)?,
            api_paid_fast: self
                .get_setting(KEY_API_PAID_FAST)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            write_allowed: self
                .get_setting(KEY_WRITE_ALLOWED)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            write_prompt_skip: self
                .get_setting(KEY_WRITE_PROMPT_SKIP)?
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            swap_mode: self.get_setting(KEY_SWAP_MODE)?,
            tx_budget_mode: self.get_setting(KEY_BUDGET_MODE)?,
            tx_cu_limit: self.get_setting(KEY_CU_LIMIT)?.and_then(|s| s.parse().ok()),
            tx_tip_lamports: self
                .get_setting(KEY_TIP_LAMPORTS)?
                .and_then(|s| s.parse().ok()),
            tx_loaded_data: self
                .get_setting(KEY_LOADED_DATA)?
                .and_then(|s| s.parse().ok()),
            tx_heap: self.get_setting(KEY_HEAP)?.and_then(|s| s.parse().ok()),
            tx_cu_margin_bps: self
                .get_setting(KEY_CU_MARGIN_BPS)?
                .and_then(|s| s.parse().ok()),
            tx_tip_floor: self
                .get_setting(KEY_TIP_FLOOR)?
                .and_then(|s| s.parse().ok()),
            tx_tip_ceiling: self
                .get_setting(KEY_TIP_CEILING)?
                .and_then(|s| s.parse().ok()),
            dlmm_strategy: self.get_setting(KEY_DLMM_STRATEGY)?,
        })
    }

    pub fn set_wallet_pubkey(&self, pubkey: &str) -> Result<()> {
        self.set_setting(KEY_WALLET, pubkey)
    }

    pub fn clear_wallet(&self) -> Result<()> {
        self.delete_setting(KEY_WALLET)?;
        self.delete_setting(KEY_KEYPAIR)?;
        Ok(())
    }

    /// Persist keypair **file path** only — never secret key material.
    pub fn set_keypair_path(&self, path: &str) -> Result<()> {
        self.set_setting(KEY_KEYPAIR, path)
    }

    pub fn set_rpc_url(&self, url: &str) -> Result<()> {
        self.set_setting(KEY_RPC, url)
    }

    pub fn set_privacy_trim(&self, on: bool) -> Result<()> {
        self.set_setting(KEY_PRIVACY, if on { "1" } else { "0" })
    }

    pub fn set_price_mode(&self, mode: &str) -> Result<()> {
        self.set_setting(KEY_PRICE_MODE, mode)
    }

    pub fn set_jupiter_api_key(&self, key: Option<&str>) -> Result<()> {
        match key.map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => self.set_setting(KEY_JUPITER_API, k),
            None => self.delete_setting(KEY_JUPITER_API),
        }
    }

    pub fn set_titan_api_key(&self, key: Option<&str>) -> Result<()> {
        match key.map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => self.set_setting(KEY_TITAN_API, k),
            None => self.delete_setting(KEY_TITAN_API),
        }
    }

    pub fn set_api_paid_fast(&self, on: bool) -> Result<()> {
        self.set_setting(KEY_API_PAID_FAST, if on { "1" } else { "0" })
    }

    pub fn set_write_allowed(&self, allowed: bool) -> Result<()> {
        self.set_setting(KEY_WRITE_ALLOWED, if allowed { "1" } else { "0" })
    }

    pub fn set_write_prompt_skip(&self, skip: bool) -> Result<()> {
        self.set_setting(KEY_WRITE_PROMPT_SKIP, if skip { "1" } else { "0" })
    }

    pub fn set_swap_mode(&self, mode: &str) -> Result<()> {
        self.set_setting(KEY_SWAP_MODE, mode)
    }

    pub fn set_tx_budget_mode(&self, mode: &str) -> Result<()> {
        self.set_setting(KEY_BUDGET_MODE, mode)
    }

    pub fn set_tx_cu_limit(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_CU_LIMIT, &v.to_string())
    }

    pub fn set_tx_tip_lamports(&self, v: u64) -> Result<()> {
        self.set_setting(KEY_TIP_LAMPORTS, &v.to_string())
    }

    pub fn set_tx_loaded_data(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_LOADED_DATA, &v.to_string())
    }

    pub fn set_tx_heap(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_HEAP, &v.to_string())
    }

    pub fn set_tx_cu_margin_bps(&self, v: u32) -> Result<()> {
        self.set_setting(KEY_CU_MARGIN_BPS, &v.to_string())
    }

    pub fn set_tx_tip_floor(&self, v: u64) -> Result<()> {
        self.set_setting(KEY_TIP_FLOOR, &v.to_string())
    }

    pub fn set_tx_tip_ceiling(&self, v: u64) -> Result<()> {
        self.set_setting(KEY_TIP_CEILING, &v.to_string())
    }

    pub fn set_dlmm_strategy(&self, s: &str) -> Result<()> {
        self.set_setting(KEY_DLMM_STRATEGY, s)
    }

    /// Persist a landed write tx (manage / create). More analytics later.
    #[allow(clippy::too_many_arguments)]
    pub fn record_write_tx(
        &self,
        signature: &str,
        op: &str,
        venue: Option<&str>,
        position: Option<&str>,
        pool: Option<&str>,
        tip_lamports: Option<u64>,
        fee_lamports: Option<u64>,
        sol_spent_lamports: Option<u64>,
        summary: Option<&str>,
        paid_label: Option<&str>,
    ) -> Result<()> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "0".into());
        self.conn
            .execute(
                "INSERT OR REPLACE INTO write_txs (
                    created_at, signature, op, venue, position, pool,
                    tip_lamports, fee_lamports, sol_spent_lamports, summary, paid_label
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    created_at,
                    signature,
                    op,
                    venue,
                    position,
                    pool,
                    tip_lamports.map(|v| v as i64),
                    fee_lamports.map(|v| v as i64),
                    sol_spent_lamports.map(|v| v as i64),
                    summary,
                    paid_label,
                ],
            )
            .context("record_write_tx")?;
        Ok(())
    }

    pub fn get_token(&self, mint: &str) -> Result<Option<CachedToken>> {
        self.conn
            .query_row(
                "SELECT mint, symbol, decimals FROM tokens WHERE mint = ?1",
                params![mint],
                |row| {
                    Ok(CachedToken {
                        mint: row.get(0)?,
                        symbol: row.get(1)?,
                        decimals: row.get::<_, i64>(2)? as u8,
                    })
                },
            )
            .optional()
            .context("get token")
    }

    pub fn upsert_token(&self, mint: &str, symbol: &str, decimals: u8) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO tokens (mint, symbol, decimals, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(mint) DO UPDATE SET
                   symbol = excluded.symbol,
                   decimals = excluded.decimals,
                   updated_at = excluded.updated_at",
                params![mint, symbol, decimals as i64, now],
            )
            .context("upsert token")?;
        Ok(())
    }

    fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("get setting {key}"))
    }

    fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .with_context(|| format!("set setting {key}"))?;
        Ok(())
    }

    fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", params![key])
            .with_context(|| format!("delete setting {key}"))?;
        Ok(())
    }
}

/// `~/.config/sol-clmm-manager`
pub fn default_config_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join(APP_DIR_NAME))
}

pub fn default_db_path() -> Result<PathBuf> {
    Ok(default_config_dir()?.join(DEFAULT_DB_FILE))
}

pub fn active_pointer_path() -> Result<PathBuf> {
    Ok(default_config_dir()?.join(ACTIVE_POINTER_FILE))
}

pub fn read_active_pointer() -> Result<Option<PathBuf>> {
    let pointer = active_pointer_path()?;
    if !pointer.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(&pointer).with_context(|| format!("read {}", pointer.display()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(trimmed)))
}

pub fn write_active_pointer(db_path: &Path) -> Result<()> {
    let dir = default_config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let abs = if db_path.is_absolute() {
        db_path.to_path_buf()
    } else {
        std::env::current_dir().context("cwd")?.join(db_path)
    };
    let pointer = active_pointer_path()?;
    fs::write(&pointer, format!("{}\n", abs.display()))
        .with_context(|| format!("write {}", pointer.display()))?;
    Ok(())
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest);
    }
    if path == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(path)
}

/// Resolve DB path for this launch: `--db` wins, else last active pointer if file exists.
pub fn resolve_existing_db(cli_db: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(p) = cli_db {
        let path = expand_tilde(&p.display().to_string());
        return Ok(Some(path));
    }
    if let Some(path) = read_active_pointer()?
        && path.exists()
    {
        return Ok(Some(path));
    }
    Ok(None)
}

pub fn display_path(path: &Path) -> String {
    let s = path.display().to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    s
}

fn format_db_size(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let x = n as f64;
    if x >= GB {
        format!("{:.1}G", x / GB)
    } else if x >= MB {
        format!("{:.1}M", x / MB)
    } else if x >= KB {
        format!("{:.0}K", x / KB)
    } else {
        format!("{n}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn settings_and_tokens_roundtrip() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("whirpool-store-{stamp}.db"));
        let _ = fs::remove_file(&path);

        let store = Store::open(&path, false).unwrap();
        store.set_wallet_pubkey("Abc").unwrap();
        store.set_rpc_url("https://example.com").unwrap();
        store.set_privacy_trim(true).unwrap();
        store.upsert_token("mint1", "USDC", 6).unwrap();

        let s = store.load_settings().unwrap();
        assert_eq!(s.wallet_pubkey.as_deref(), Some("Abc"));
        assert_eq!(s.rpc_url.as_deref(), Some("https://example.com"));
        assert!(s.privacy_trim);

        let t = store.get_token("mint1").unwrap().unwrap();
        assert_eq!(t.symbol, "USDC");
        assert_eq!(t.decimals, 6);

        let store2 = Store::open(&path, true).unwrap();
        assert!(store2.load_settings().unwrap().wallet_pubkey.is_none());
        assert!(store2.get_token("mint1").unwrap().is_none());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn overwrite_requires_existing_file_gone() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("whirpool-store-ow-{stamp}.db"));
        fs::write(&path, b"not-sqlite").unwrap();
        // opening without overwrite should fail on corrupt? sqlite may create over or fail
        // overwrite true removes first
        let store = Store::open(&path, true).unwrap();
        store.set_rpc_url("https://x").unwrap();
        assert!(path.exists());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn expand_tilde_home() {
        if std::env::var_os("HOME").is_some() {
            let p = expand_tilde("~/.config/sol-clmm-manager/sol-clmm.db");
            assert!(p.is_absolute());
            assert!(p.ends_with("sol-clmm.db"));
        }
    }

    #[test]
    fn default_paths_ok() {
        if std::env::var_os("HOME").is_none() {
            return;
        }
        let p = default_db_path().unwrap();
        assert!(p.ends_with(DEFAULT_DB_FILE));
    }

    #[test]
    fn resolve_cli_db() {
        let p = resolve_existing_db(Some(Path::new("/tmp/x.db")))
            .unwrap()
            .unwrap();
        assert_eq!(p, PathBuf::from("/tmp/x.db"));
    }

    #[test]
    fn file_size_label_after_open() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("sol-clmm-size-{stamp}.db"));
        let _ = fs::remove_file(&path);
        let store = Store::open(&path, false).unwrap();
        store.set_rpc_url("https://x").unwrap();
        let n = store.file_size_bytes().unwrap();
        assert!(n > 0);
        assert!(!store.file_size_label().is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn format_db_size_units() {
        assert_eq!(format_db_size(500), "500B");
        assert_eq!(format_db_size(2048), "2K");
        assert_eq!(format_db_size(2 * 1024 * 1024), "2.0M");
    }
}
