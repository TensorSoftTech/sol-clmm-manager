use anyhow::Result;
use ratatui::widgets::TableState;
use sol_clmm_client::Client;
use sol_clmm_manager::rpc::{self, Rpc, RpcMeter, RpcStatus};
use sol_clmm_manager::store::{
    self, AppSettings, Store, default_db_path, display_path, expand_tilde,
};
use sol_clmm_manager::{
    PositionDetail, PositionRow, PriceApiKeys, PriceMode, VenueFilter, WriteTxRow,
};

/// General write history vs one position's txs.
#[derive(Debug, Clone, Default)]
pub enum HistoryFilter {
    #[default]
    All,
    Position {
        address: String,
        /// Short pair label for the status/title.
        label: String,
    },
}
use solana_keypair::{Signer, read_keypair_file};
use solana_pubkey::Pubkey;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    DbSetup,
    Setup,
    RpcOffer,
    WriteConsent,
    Positions,
    /// Landed write txs from SQLite (`write_txs`).
    History,
    Settings,
    SettingsRpc,
    SettingsApiKey,
    Detail,
    /// Position manage menu (`m`) — deposit / claim / withdraw / close.
    #[cfg_attr(not(feature = "write"), allow(dead_code))]
    Manage,
    /// Add liquidity to an open position (2-side / 1-side).
    #[cfg_attr(not(feature = "write"), allow(dead_code))]
    ManageDeposit,
    /// List-level wallet tools: wrap / unwrap / swap / create pool.
    #[cfg_attr(not(feature = "write"), allow(dead_code))]
    Wallet,
    /// Claim fees → redeposit (compound).
    #[cfg_attr(not(feature = "write"), allow(dead_code))]
    ManageCompound,
    /// Withdraw / claim+withdraw · % of L.
    #[cfg_attr(not(feature = "write"), allow(dead_code))]
    ManageWithdraw,
    /// Create position wizard (`n`) — works with an empty position list.
    #[cfg_attr(not(feature = "write"), allow(dead_code))]
    CreatePosition,
    /// Simulate → user confirm → send; on failure offer retry.
    #[cfg_attr(not(feature = "write"), allow(dead_code))]
    WriteConfirm,
    Help,
}

#[cfg(feature = "write")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageChoice {
    /// Add liquidity · 2-side or 1-side wizard.
    Deposit,
    /// Claim fees → redeposit (no-swap / with-swap).
    Compound,
    /// Claim fees+rewards + withdraw all + close position account.
    Close,
    /// Claim fees+rewards + withdraw all liquidity (keep account).
    ClaimWithdraw,
    /// Withdraw all liquidity only.
    Withdraw,
    /// Claim fees+rewards only.
    Claim,
}

#[cfg(feature = "write")]
impl ManageChoice {
    pub const ALL: [ManageChoice; 6] = [
        ManageChoice::Deposit,
        ManageChoice::Compound,
        ManageChoice::Close,
        ManageChoice::ClaimWithdraw,
        ManageChoice::Withdraw,
        ManageChoice::Claim,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Compound => "compound",
            Self::Close => "close position",
            Self::ClaimWithdraw => "claim + withdraw",
            Self::Withdraw => "withdraw liquidity",
            Self::Claim => "claim fees + rewards",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Deposit => "add liquidity · SOL as native · Jupiter wrapAndUnwrapSol on autoswap",
            Self::Compound => "claim fees → redeposit · no-swap ratio or with-swap residual",
            Self::Close => "claim fees → withdraw all → unwrap SOL → close account (1 tx)",
            Self::ClaimWithdraw => "claim fees → withdraw % of L · unwrap SOL · keep NFT",
            Self::Withdraw => "withdraw % of liquidity · unwrap SOL · fees stay unclaimed",
            Self::Claim => "collect fees + rewards only · liquidity stays",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyTarget {
    Jupiter,
    Titan,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    Choose,
    Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbPathChoice {
    UseDefault,
    CustomPath,
}

impl DbPathChoice {
    pub const ALL: [DbPathChoice; 2] = [DbPathChoice::UseDefault, DbPathChoice::CustomPath];

    pub fn label(self, default_path: &str) -> String {
        match self {
            Self::UseDefault => format!("default · {default_path}"),
            Self::CustomPath => "custom path".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbExistChoice {
    UseExisting,
    Overwrite,
}

impl DbExistChoice {
    pub const ALL: [DbExistChoice; 2] = [DbExistChoice::UseExisting, DbExistChoice::Overwrite];

    pub fn label(self) -> &'static str {
        match self {
            Self::UseExisting => "load existing database",
            Self::Overwrite => "overwrite (wipe wallet / RPC / token cache)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbSetupPhase {
    ChooseLocation,
    CustomInput,
    ConfirmExist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletSource {
    Pubkey,
    Keypair,
}

impl WalletSource {
    pub const ALL: [WalletSource; 2] = [WalletSource::Pubkey, WalletSource::Keypair];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pubkey => "wallet pubkey (read-only)",
            Self::Keypair => "keypair file path",
        }
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Self::Pubkey => "paste base58 pubkey, then Enter",
            Self::Keypair => "type keypair path (e.g. ~/.config/solana/id.json), then Enter",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcChoice {
    KeepPublic,
    UsePrivate,
}

impl RpcChoice {
    pub const ALL: [RpcChoice; 2] = [RpcChoice::KeepPublic, RpcChoice::UsePrivate];

    pub fn label(self) -> &'static str {
        match self {
            Self::KeepPublic => "keep public RPC (free, slower / rate-limited)",
            Self::UsePrivate => "use private RPC URL (faster)",
        }
    }
}

/// Sentinel · network fee sample (capped by tip ceiling).
pub const TIP_AUTO: u64 = u64::MAX;

/// Priority fee presets. `0` = off · `TIP_AUTO` = network sample.
pub const TIP_PRESETS: [u64; 9] = [
    0, // off — no priority fee on the tx
    1_000,
    2_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    TIP_AUTO,
];

pub fn tip_label(tip: u64) -> String {
    if tip == 0 {
        "OFF · no priority fee".into()
    } else if tip == TIP_AUTO {
        "auto · network sample (capped)".into()
    } else {
        format!("{tip} lamports ({:.6} SOL)", tip as f64 / 1e9)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItem {
    PrivacyTrim,
    PriceMode,
    SwapMode,
    BudgetMode,
    PriorityFee,
    WriteAccess,
    JupiterKey,
    TitanKey,
    ApiPaidFast,
    DlmmStrategy,
    ChangeRpc,
    ChangeWallet,
    Back,
}

impl SettingsItem {
    pub const ALL: [SettingsItem; 13] = [
        SettingsItem::PrivacyTrim,
        SettingsItem::PriceMode,
        SettingsItem::SwapMode,
        SettingsItem::BudgetMode,
        SettingsItem::PriorityFee,
        SettingsItem::WriteAccess,
        SettingsItem::JupiterKey,
        SettingsItem::TitanKey,
        SettingsItem::ApiPaidFast,
        SettingsItem::DlmmStrategy,
        SettingsItem::ChangeRpc,
        SettingsItem::ChangeWallet,
        SettingsItem::Back,
    ];

    #[allow(clippy::too_many_arguments)]
    pub fn label(
        self,
        privacy_trim: bool,
        price_mode: PriceMode,
        jupiter_key: bool,
        titan_key: bool,
        api_paid_fast: bool,
        swap_mode: &str,
        budget_mode: &str,
        tip_lamports: u64,
        write_allowed: Option<bool>,
        dlmm_strategy: &str,
    ) -> String {
        match self {
            Self::PrivacyTrim => format!(
                "privacy · trim wallet + private RPC  [{}]",
                if privacy_trim {
                    "ON · wallet 4 · https://xyz…"
                } else {
                    "OFF · full"
                }
            ),
            Self::PriceMode => format!("USD estimate · {}  [Enter cycles]", price_mode.short()),
            Self::SwapMode => format!("swap router · {swap_mode}  [Enter cycles]"),
            Self::BudgetMode => {
                format!("tx v1 CU · {budget_mode}  [Enter cycles · auto = sim CU]")
            }
            Self::PriorityFee => {
                format!("priority fee · {}  [Enter cycles]", tip_label(tip_lamports))
            }
            Self::WriteAccess => format!(
                "writes  [{}]",
                match write_allowed {
                    Some(true) => "ENABLED · Enter disables",
                    Some(false) => "DISABLED · Enter enables",
                    None => {
                        if sol_clmm_manager::WRITE_ENABLED {
                            "ask on start · Enter enables"
                        } else {
                            "compiled read-only · cargo --features write"
                        }
                    }
                }
            ),
            Self::JupiterKey => format!(
                "Jupiter API key  [{}]",
                if jupiter_key {
                    "set · Enter edit · empty clears"
                } else {
                    "none · free/keyless · Enter to set"
                }
            ),
            Self::TitanKey => format!(
                "Titan API key  [{}]",
                if titan_key {
                    "set · Enter edit · empty clears"
                } else {
                    "required for Titan · developer.titan.exchange"
                }
            ),
            Self::ApiPaidFast => format!(
                "API throttle · paid/fast when keyed  [{}]",
                if api_paid_fast {
                    "ON · short wait"
                } else {
                    "OFF · free-tier throttle"
                }
            ),
            Self::DlmmStrategy => {
                format!("DLMM deposit strategy · {dlmm_strategy}  [Enter cycles]")
            }
            Self::ChangeRpc => "change RPC URL".into(),
            Self::ChangeWallet => "change wallet".into(),
            Self::Back => "back to positions".into(),
        }
    }
}

pub struct WizardState {
    pub step: SetupStep,
    pub selected: usize,
    pub input: String,
    pub error: Option<String>,
}

impl WizardState {
    fn new() -> Self {
        Self {
            step: SetupStep::Choose,
            selected: 0,
            input: String::new(),
            error: None,
        }
    }

    pub fn wallet_source(&self) -> WalletSource {
        WalletSource::ALL[self.selected.min(WalletSource::ALL.len() - 1)]
    }

    pub fn rpc_choice(&self) -> RpcChoice {
        RpcChoice::ALL[self.selected.min(RpcChoice::ALL.len() - 1)]
    }
}

pub struct DbWizard {
    pub phase: DbSetupPhase,
    pub selected: usize,
    pub input: String,
    pub error: Option<String>,
    pub default_path: PathBuf,
    pub pending_path: Option<PathBuf>,
}

impl DbWizard {
    pub fn new() -> Self {
        let default_path = default_db_path().unwrap_or_else(|_| {
            PathBuf::from(format!(
                "~/.config/{}/{}",
                store::APP_DIR_NAME,
                store::DEFAULT_DB_FILE
            ))
        });
        Self {
            phase: DbSetupPhase::ChooseLocation,
            selected: 0,
            input: display_path(&default_path),
            error: None,
            default_path,
            pending_path: None,
        }
    }

    pub fn path_choice(&self) -> DbPathChoice {
        DbPathChoice::ALL[self.selected.min(DbPathChoice::ALL.len() - 1)]
    }

    pub fn exist_choice(&self) -> DbExistChoice {
        DbExistChoice::ALL[self.selected.min(DbExistChoice::ALL.len() - 1)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingWork {
    None,
    /// `refresh_meta` → re-fetch token symbols (skip SQLite read).
    List {
        refresh_meta: bool,
    },
    Detail {
        refresh_meta: bool,
    },
    /// Wallet screen balances (wrap / swap / create pool).
    #[cfg(feature = "write")]
    Wallet,
}

/// Result of a background list/detail fetch (keeps UI animating).
pub enum BgFetchResult {
    List {
        status: Result<RpcStatus>,
        positions: Result<Vec<PositionRow>>,
        meter: RpcMeter,
    },
    Detail {
        result: Box<Result<PositionDetail>>,
        meter: RpcMeter,
    },
    #[cfg(feature = "write")]
    Wallet {
        result: Box<anyhow::Result<(u64, Vec<sol_clmm_manager::WalletTokenBalance>)>>,
        meter: RpcMeter,
    },
    #[cfg(feature = "write")]
    Write {
        result: Box<anyhow::Result<sol_clmm_manager::SendOutcome>>,
        /// Fresh wallet SOL/ATA balances after a successful send (every tx changes them).
        balances: Option<sol_clmm_manager::WalletPoolBalances>,
        /// Pool mint ATA Δ (after − before) for history.
        token_delta: Option<sol_clmm_manager::WriteTokenDelta>,
    },
}

pub struct App {
    pub screen: Screen,
    pub db: DbWizard,
    pub store: Option<Store>,
    pub setup: WizardState,
    pub rpc_offer: WizardState,
    pub settings_selected: usize,
    pub settings_rpc: WizardState,
    pub settings_api_key: WizardState,
    pub api_key_target: ApiKeyTarget,
    /// When true, show only first 4 chars of wallet pubkey.
    pub privacy_trim: bool,
    pub price_mode: PriceMode,
    pub jupiter_api_key: Option<String>,
    pub titan_api_key: Option<String>,
    /// When true + key set → short throttle; false → free-tier intervals.
    pub api_paid_fast: bool,
    /// `jupiter` | `titan` — swap router for deposit+swap / compound.
    pub swap_mode: String,
    /// `auto` | `manual` — tx v1 CU estimation.
    pub budget_mode: String,
    /// Priority fee lamports (`0` = network auto, capped). Used for every write.
    pub tip_lamports: u64,
    pub write_allowed: Option<bool>,
    pub write_prompt_skip: bool,
    /// Path to keypair JSON (CLI / wizard / SQLite). Secret bytes are never stored — load at sign time.
    pub keypair_path: Option<String>,
    pub dlmm_strategy: String,
    pub rpc_url: String,
    /// CLI/default RPC before store overrides (used to detect "user already set private").
    cli_rpc_url: String,
    /// Wallet from CLI — wins over DB until user changes wallet in settings.
    cli_wallet: Option<Pubkey>,
    pub wallet: Option<Pubkey>,
    pub positions: Vec<PositionRow>,
    /// `v` cycles: all → Orca Whirlpool → Raydium CLMM → Meteora DLMM → all.
    pub venue_filter: VenueFilter,
    pub table_state: TableState,
    /// Write history from SQLite (`write_txs` · newest first).
    pub history: Vec<WriteTxRow>,
    pub history_table_state: TableState,
    pub history_filter: HistoryFilter,
    /// Screen to restore when leaving history (positions list or detail).
    pub history_return: Screen,
    pub status: Option<RpcStatus>,
    pub status_line: String,
    pub should_quit: bool,
    pub pending: PendingWork,
    pub rpc_dirty: bool,
    pub detail: Option<PositionDetail>,
    pub detail_error: Option<String>,
    pub detail_loading: bool,
    /// Detail chart X zoom (± tick-spacings around current). `+/-` · `0` fit range.
    pub detail_chart_half_spacings: i32,
    /// True while list RPC is in flight (survives clearing `pending` before fetch).
    pub list_loading: bool,
    /// True while wallet balance RPC is in flight (Wallet screen load bar).
    #[cfg(feature = "write")]
    pub wallet_loading: bool,
    /// `p` pubkey overlay active — second `p` restores the previous status line.
    pub links_overlay: bool,
    status_before_links: Option<String>,
    /// When loading started — drives the oscillating progress bar.
    pub load_started: Option<Instant>,
    /// Background fetch receiver (list / detail) so the UI can keep animating.
    bg_rx: Option<Receiver<BgFetchResult>>,
    /// Session RPC call counter + history (not persisted).
    pub rpc_meter: RpcMeter,
    /// Screen to return to when leaving Help (`?` / `h`).
    pub help_return: Screen,
    /// Scroll offset (lines) inside the help panel.
    pub help_scroll: u16,
    #[cfg(feature = "write")]
    pub create: Option<crate::write_flow::CreateWizard>,
    #[cfg(feature = "write")]
    pub manage_deposit: Option<crate::write_flow::ManageDepositWizard>,
    #[cfg(feature = "write")]
    #[cfg(feature = "write")]
    pub wallet_wiz: Option<crate::wallet_flow::WalletWizard>,
    #[cfg(feature = "write")]
    pub manage_compound: Option<crate::write_flow::ManageCompoundWizard>,
    #[cfg(feature = "write")]
    pub manage_withdraw: Option<crate::write_flow::ManageWithdrawWizard>,
    #[cfg(feature = "write")]
    pub write_confirm: Option<crate::write_flow::WriteConfirm>,
    /// Manage menu selection index (`m` on detail).
    #[cfg(feature = "write")]
    pub manage_choice: usize,
    /// Scroll offset inside confirm error / log panel.
    #[cfg(feature = "write")]
    pub confirm_scroll: u16,
}

impl App {
    /// `db_from_cli`: `--db` / `SOL_CLMM_DB` — open immediately.
    /// Otherwise a saved pointer only pre-fills the DB wizard (use existing / overwrite).
    pub fn new(
        rpc_url: String,
        wallet: Option<Pubkey>,
        existing_db: Option<PathBuf>,
        db_from_cli: bool,
    ) -> Self {
        let mut app = Self {
            screen: Screen::DbSetup,
            db: DbWizard::new(),
            store: None,
            setup: WizardState::new(),
            rpc_offer: WizardState::new(),
            settings_selected: 0,
            settings_rpc: WizardState::new(),
            settings_api_key: WizardState::new(),
            api_key_target: ApiKeyTarget::Jupiter,
            privacy_trim: false,
            price_mode: PriceMode::OnChain,
            jupiter_api_key: std::env::var("JUPITER_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            titan_api_key: std::env::var("TITAN_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            api_paid_fast: false,
            swap_mode: "jupiter".into(),
            budget_mode: "auto".into(),
            tip_lamports: 5_000,
            write_allowed: None,
            write_prompt_skip: false,
            keypair_path: None,
            dlmm_strategy: "CurveBalanced".into(),
            rpc_url: rpc_url.clone(),
            cli_rpc_url: rpc_url,
            cli_wallet: wallet,
            wallet: None,
            positions: Vec::new(),
            venue_filter: VenueFilter::All,
            table_state: TableState::default(),
            history: Vec::new(),
            history_table_state: TableState::default(),
            history_filter: HistoryFilter::All,
            history_return: Screen::Positions,
            status: None,
            status_line: String::new(),
            should_quit: false,
            pending: PendingWork::None,
            rpc_dirty: false,
            detail: None,
            detail_error: None,
            detail_loading: false,
            detail_chart_half_spacings: sol_clmm_manager::CHART_HALF_SPACINGS_DEFAULT,
            list_loading: false,
            #[cfg(feature = "write")]
            wallet_loading: false,
            links_overlay: false,
            status_before_links: None,
            load_started: None,
            bg_rx: None,
            rpc_meter: RpcMeter::default(),
            help_return: Screen::Positions,
            help_scroll: 0,
            #[cfg(feature = "write")]
            create: None,
            #[cfg(feature = "write")]
            manage_deposit: None,
            #[cfg(feature = "write")]
            wallet_wiz: None,
            #[cfg(feature = "write")]
            manage_compound: None,
            #[cfg(feature = "write")]
            manage_withdraw: None,
            #[cfg(feature = "write")]
            write_confirm: None,
            #[cfg(feature = "write")]
            manage_choice: 0,
            #[cfg(feature = "write")]
            confirm_scroll: 0,
        };

        if let Some(path) = existing_db {
            if db_from_cli {
                match Store::open(&path, false) {
                    Ok(store) => app.finish_store(store),
                    Err(e) => {
                        app.db.error = Some(format!("open {}: {e:#}", display_path(&path)));
                        app.db.input = display_path(&path);
                        app.db.phase = DbSetupPhase::ChooseLocation;
                        app.status_line =
                            "could not open --db · ↑↓ choose path · Enter · q quit".into();
                    }
                }
            } else if path.exists() {
                // Last session DB — ask before jumping to positions.
                app.db.pending_path = Some(path.clone());
                app.db.phase = DbSetupPhase::ConfirmExist;
                app.db.selected = 0; // UseExisting
                app.status_line = format!(
                    "saved DB · {} · ↑↓ use / overwrite · Enter · Esc other path",
                    display_path(&path)
                );
            } else {
                app.status_line =
                    "saved DB missing · choose where to store SQLite · ↑↓ · Enter · q quit".into();
            }
        } else {
            app.status_line =
                "first run · choose where to store SQLite · ↑↓ · Enter · q quit".into();
        }
        app
    }

    fn finish_store(&mut self, store: Store) {
        let settings = store.load_settings().unwrap_or_default();
        let rpc_already_saved = settings.rpc_url.is_some();
        self.apply_settings(&settings);
        self.store = Some(store);

        if let Some(pk) = self.cli_wallet {
            self.wallet = Some(pk);
            self.persist_wallet(&pk, None);
        }

        // CLI non-default RPC wins for this session and is persisted.
        let cli_rpc_override = self.cli_rpc_url != rpc::DEFAULT_RPC_URL;
        if cli_rpc_override {
            self.rpc_url = self.cli_rpc_url.clone();
            self.persist_rpc();
        }

        if self.wallet.is_some() {
            self.after_wallet_ready(rpc_already_saved || cli_rpc_override);
        } else {
            self.screen = Screen::Setup;
            self.status_line = "wallet not set · ↑↓ choose · Enter · Esc quit".into();
        }
    }

    fn apply_settings(&mut self, settings: &AppSettings) {
        self.privacy_trim = settings.privacy_trim;
        if let Some(mode) = &settings.price_mode {
            self.price_mode = PriceMode::from_setting(mode);
        }
        if let Some(k) = &settings.jupiter_api_key
            && !k.is_empty()
        {
            self.jupiter_api_key = Some(k.clone());
        }
        if let Some(k) = &settings.titan_api_key
            && !k.is_empty()
        {
            self.titan_api_key = Some(k.clone());
        }
        self.api_paid_fast = settings.api_paid_fast;
        if let Some(m) = &settings.swap_mode {
            self.swap_mode = m.clone();
        }
        if let Some(m) = &settings.tx_budget_mode {
            self.budget_mode = m.clone();
        }
        if let Some(v) = settings.tx_tip_lamports {
            self.tip_lamports = v;
        }
        self.write_allowed = settings.write_allowed;
        self.write_prompt_skip = settings.write_prompt_skip;
        if let Some(p) = &settings.keypair_path {
            self.keypair_path = Some(p.clone());
        }
        if let Some(s) = &settings.dlmm_strategy {
            self.dlmm_strategy = s.clone();
        }
        if let Some(url) = &settings.rpc_url {
            self.rpc_url = url.clone();
            self.rpc_dirty = true;
        }
        if self.cli_wallet.is_none()
            && let Some(pk) = &settings.wallet_pubkey
            && let Ok(pk) = pk.parse::<Pubkey>()
        {
            self.wallet = Some(pk);
        }
    }

    #[allow(dead_code)]
    pub fn writes_live(&self) -> bool {
        sol_clmm_manager::WRITE_ENABLED
            && self.write_allowed == Some(true)
            && self.keypair_path.is_some()
    }

    pub fn price_keys(&self) -> PriceApiKeys {
        PriceApiKeys {
            jupiter: self.jupiter_api_key.clone(),
            titan: self.titan_api_key.clone(),
            paid_fast: self.api_paid_fast,
        }
    }

    fn mask_key(key: &Option<String>) -> String {
        match key {
            Some(k) if k.len() > 8 => format!("{}…{}", &k[..4], &k[k.len() - 4..]),
            Some(_) => "(set)".into(),
            None => "(none)".into(),
        }
    }

    fn persist_wallet(&mut self, pubkey: &Pubkey, keypair_path: Option<&str>) {
        if let Some(path) = keypair_path {
            self.keypair_path = Some(path.to_string());
        }
        if let Some(store) = &self.store {
            let _ = store.set_wallet_pubkey(&pubkey.to_string());
            if let Some(path) = keypair_path {
                let _ = store.set_keypair_path(path);
            }
        }
    }

    fn persist_rpc(&self) {
        if let Some(store) = &self.store {
            let _ = store.set_rpc_url(&self.rpc_url);
        }
    }

    fn persist_privacy(&self) {
        if let Some(store) = &self.store {
            let _ = store.set_privacy_trim(self.privacy_trim);
        }
    }

    pub fn db_path_display(&self) -> String {
        self.store
            .as_ref()
            .map(|s| display_path(s.path()))
            .unwrap_or_else(|| "(no db)".into())
    }

    pub fn format_wallet(&self) -> String {
        match self.wallet {
            Some(pk) => {
                let s = pk.to_string();
                if self.privacy_trim {
                    let take = s.chars().take(4).collect::<String>();
                    format!("{take}…")
                } else {
                    s
                }
            }
            None => "(no wallet)".into(),
        }
    }

    /// Display RPC for UI. Privacy ON → `https://xyz…` (first chars of host only).
    /// Always drops query string (api-key in URL) from what we render.
    pub fn format_rpc(&self) -> String {
        format_rpc_url(&self.rpc_url, self.privacy_trim)
    }

    fn after_wallet_ready(&mut self, skip_rpc_offer: bool) {
        if !skip_rpc_offer && rpc::is_public_rpc(&self.rpc_url) {
            self.screen = Screen::RpcOffer;
            self.rpc_offer = WizardState::new();
            self.status_line =
                "public RPC detected · ↑↓ choose · Enter · private RPC = better performance".into();
        } else {
            self.maybe_write_consent_then_positions();
        }
    }

    /// After RPC offer / wallet ready: ask write consent once (if compiled with `write`).
    pub fn maybe_write_consent_then_positions(&mut self) {
        if sol_clmm_manager::WRITE_ENABLED
            && self.keypair_path.is_some()
            && !self.write_prompt_skip
            && self.write_allowed.is_none()
        {
            self.screen = Screen::WriteConsent;
            self.setup = WizardState::new();
            self.status_line =
                "allow write access? · ↑↓ Yes/No/Don't ask · Enter · Esc = no".into();
            return;
        }
        self.enter_positions();
    }

    pub fn write_consent_choices() -> [&'static str; 3] {
        [
            "Yes · allow claim / deposit / withdraw (tx v1 · always simulate)",
            "No · read-only this session",
            "Don't ask again · remember No",
        ]
    }

    pub fn write_consent_activate(&mut self) {
        let i = self.setup.selected.min(2);
        match i {
            0 => {
                self.write_allowed = Some(true);
                if let Some(store) = &self.store {
                    let _ = store.set_write_allowed(true);
                }
            }
            1 => {
                self.write_allowed = Some(false);
                if let Some(store) = &self.store {
                    let _ = store.set_write_allowed(false);
                }
            }
            _ => {
                self.write_allowed = Some(false);
                self.write_prompt_skip = true;
                if let Some(store) = &self.store {
                    let _ = store.set_write_allowed(false);
                    let _ = store.set_write_prompt_skip(true);
                }
            }
        }
        self.enter_positions();
    }

    fn enter_positions(&mut self) {
        self.screen = Screen::Positions;
        self.detail = None;
        self.detail_error = None;
        self.begin_list_load(false, "loading from RPC…");
    }

    /// Esc from write-consent → positions without allowing writes.
    pub fn enter_positions_public(&mut self) {
        self.enter_positions();
    }

    pub fn request_list_refresh(&mut self) {
        if self.is_busy() {
            return;
        }
        self.dismiss_links_overlay();
        self.begin_list_load(true, "refreshing positions…");
    }

    /// Cycle venue filter (`v`): all → Orca → Raydium → Meteora → all.
    pub fn cycle_venue_filter(&mut self) {
        if self.is_busy() {
            return;
        }
        self.dismiss_links_overlay();
        self.venue_filter = self.venue_filter.next();
        self.sync_table_selection_after_filter();
        self.status_line = self.positions_status_line();
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        self.positions
            .iter()
            .enumerate()
            .filter(|(_, p)| self.venue_filter.matches(p.venue))
            .map(|(i, _)| i)
            .collect()
    }

    fn sync_table_selection_after_filter(&mut self) {
        let n = self.filtered_indices().len();
        if n == 0 {
            self.table_state.select(None);
        } else {
            let i = self.table_state.selected().unwrap_or(0).min(n - 1);
            self.table_state.select(Some(i));
        }
    }

    pub fn positions_status_line(&self) -> String {
        let kind = if rpc::is_public_rpc(&self.rpc_url) {
            "public"
        } else {
            "private"
        };
        let shown = self.filtered_indices().len();
        let total = self.positions.len();
        let venue = self.venue_filter.label();
        let write_hint = if sol_clmm_manager::WRITE_ENABLED {
            " · n create · w wallet"
        } else {
            ""
        };
        format!(
            "ok · {kind} · {shown}/{total} · {venue} · ? help · v venue · y history{write_hint} · Enter detail · p pubkey · s settings · r refresh · q"
        )
    }

    /// General history from the positions list (`y`).
    pub fn open_history(&mut self) {
        if self.is_busy() {
            return;
        }
        self.dismiss_links_overlay();
        self.history_filter = HistoryFilter::All;
        self.history_return = Screen::Positions;
        self.reload_history();
        self.screen = Screen::History;
        self.status_line = self.history_status_line();
    }

    /// Position-scoped history from detail (`y`).
    pub fn open_position_history(&mut self) {
        if self.is_busy() {
            return;
        }
        let (address, label) = match self.detail.as_ref() {
            Some(d) => (
                d.row.address.to_string(),
                format!("{} / {}", d.token_a.symbol, d.token_b.symbol),
            ),
            None => {
                self.status_line = "no detail loaded".into();
                return;
            }
        };
        self.dismiss_links_overlay();
        self.history_filter = HistoryFilter::Position { address, label };
        self.history_return = Screen::Detail;
        self.reload_history();
        self.screen = Screen::History;
        self.status_line = self.history_status_line();
    }

    pub fn close_history(&mut self) {
        self.dismiss_links_overlay();
        let back = self.history_return;
        self.screen = back;
        self.status_line = match back {
            Screen::Detail => {
                if self.writes_live() {
                    "detail · m manage · y history · p pubkey · r refresh · Esc".into()
                } else {
                    "detail · y history · p pubkey · r refresh · Esc".into()
                }
            }
            _ => self.positions_status_line(),
        };
    }

    pub fn reload_history(&mut self) {
        let filter = match &self.history_filter {
            HistoryFilter::All => None,
            HistoryFilter::Position { address, .. } => Some(address.clone()),
        };
        let rows = match self.store.as_ref() {
            Some(store) => store
                .list_write_txs(500, filter.as_deref())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let n = rows.len();
        self.history = rows;
        if n == 0 {
            self.history_table_state.select(None);
        } else {
            let i = self.history_table_state.selected().unwrap_or(0).min(n - 1);
            self.history_table_state.select(Some(i));
        }
    }

    pub fn history_status_line(&self) -> String {
        let n = self.history.len();
        match &self.history_filter {
            HistoryFilter::All => format!(
                "history · all · {n} writes · ↑↓ · Enter/p sig · r reload · Esc list · ?"
            ),
            HistoryFilter::Position { label, .. } => format!(
                "history · {label} · {n} writes · ↑↓ · Enter/p sig · r reload · Esc detail · ?"
            ),
        }
    }

    pub fn selected_history_row(&self) -> Option<&WriteTxRow> {
        let i = self.history_table_state.selected()?;
        self.history.get(i)
    }

    pub fn history_select_next(&mut self) {
        self.dismiss_links_overlay();
        let n = self.history.len();
        if n == 0 {
            return;
        }
        let i = self.history_table_state.selected().unwrap_or(0);
        self.history_table_state.select(Some((i + 1).min(n - 1)));
    }

    pub fn history_select_prev(&mut self) {
        self.dismiss_links_overlay();
        let n = self.history.len();
        if n == 0 {
            return;
        }
        let i = self.history_table_state.selected().unwrap_or(0);
        self.history_table_state.select(Some(i.saturating_sub(1)));
    }

    /// Full signature (+ Solscan URL) in status (`p` / Enter · again to hide).
    pub fn toggle_selected_history_link(&mut self) {
        if self.links_overlay {
            self.restore_links_overlay();
            return;
        }
        let Some(row) = self.selected_history_row() else {
            self.status_line = "select a tx first · then p for signature".into();
            return;
        };
        let sig = row.signature.clone();
        let deltas = row.deltas_label();
        let note = row
            .summary
            .clone()
            .or_else(|| row.paid_label.clone())
            .unwrap_or_default();
        self.status_before_links = Some(self.status_line.clone());
        self.links_overlay = true;
        self.status_line = if note.is_empty() {
            format!("sig {sig} · {deltas} · p hide")
        } else {
            format!("sig {sig} · {deltas} · {note} · p hide")
        };
    }

    /// Open help overlay; remembers current screen for Esc.
    pub fn open_help(&mut self) {
        if matches!(
            self.screen,
            Screen::DbSetup
                | Screen::Setup
                | Screen::RpcOffer
                | Screen::SettingsRpc
                | Screen::SettingsApiKey
                | Screen::Manage
                | Screen::ManageDeposit
                | Screen::Wallet
                | Screen::ManageCompound
                | Screen::ManageWithdraw
                | Screen::CreatePosition
                | Screen::WriteConfirm
                | Screen::Help
        ) {
            return;
        }
        self.dismiss_links_overlay();
        self.help_return = self.screen;
        self.help_scroll = 0;
        self.screen = Screen::Help;
        self.status_line = "help · ↑↓ / j k scroll · Esc / ? / h back".into();
    }

    pub fn close_help(&mut self) {
        let back = self.help_return;
        self.screen = back;
        self.help_scroll = 0;
        self.status_line = match back {
            Screen::Detail => {
                if self.writes_live() {
                    "detail · m manage · y history · p pubkey · r refresh · Esc".into()
                } else {
                    "detail · y history · p pubkey · r refresh · Esc".into()
                }
            }
            Screen::Manage => "manage · ↑↓ · Enter · Esc · p pubkey".into(),
            Screen::ManageDeposit => {
                #[cfg(feature = "write")]
                {
                    self.manage_deposit
                        .as_ref()
                        .map(|w| w.status_hint())
                        .unwrap_or_else(|| "deposit".into())
                }
                #[cfg(not(feature = "write"))]
                {
                    "deposit".into()
                }
            }
            Screen::ManageCompound => {
                #[cfg(feature = "write")]
                {
                    self.manage_compound
                        .as_ref()
                        .map(|w| w.status_hint())
                        .unwrap_or_else(|| "compound".into())
                }
                #[cfg(not(feature = "write"))]
                {
                    "compound".into()
                }
            }
            Screen::ManageWithdraw => {
                #[cfg(feature = "write")]
                {
                    self.manage_withdraw
                        .as_ref()
                        .map(|w| w.status_hint())
                        .unwrap_or_else(|| "withdraw".into())
                }
                #[cfg(not(feature = "write"))]
                {
                    "withdraw".into()
                }
            }
            Screen::History => self.history_status_line(),
            Screen::Settings => "settings · ↑↓ · Enter · ? help · Esc back".into(),
            Screen::CreatePosition => {
                "create · ←→ range · Tab edge · Enter next · Esc back".into()
            }
            Screen::Wallet => {
                #[cfg(feature = "write")]
                {
                    self.wallet_wiz
                        .as_ref()
                        .map(|w| w.status_hint())
                        .unwrap_or_else(|| "wallet".into())
                }
                #[cfg(not(feature = "write"))]
                {
                    "wallet".into()
                }
            }
            Screen::WriteConfirm => {
                "manage tx · [ ] tip · Enter sim/send · Esc".into()
            }
            _ => self.positions_status_line(),
        };
    }

    pub fn help_scroll_by(&mut self, delta: i32) {
        if delta < 0 {
            self.help_scroll = self.help_scroll.saturating_sub((-delta) as u16);
        } else {
            self.help_scroll = self.help_scroll.saturating_add(delta as u16);
        }
    }

    pub fn request_detail_refresh(&mut self) {
        if self.is_busy() {
            return;
        }
        self.dismiss_links_overlay();
        self.detail = None;
        self.detail_error = None;
        self.begin_detail_load(true, "refreshing position…");
    }

    pub fn is_busy(&self) -> bool {
        let write_busy = {
            #[cfg(feature = "write")]
            {
                self.write_confirm
                    .as_ref()
                    .is_some_and(|c| c.phase == crate::write_flow::ConfirmPhase::Busy)
            }
            #[cfg(not(feature = "write"))]
            {
                false
            }
        };
        let wallet_busy = {
            #[cfg(feature = "write")]
            {
                self.wallet_loading
            }
            #[cfg(not(feature = "write"))]
            {
                false
            }
        };
        self.list_loading
            || self.detail_loading
            || wallet_busy
            || self.bg_rx.is_some()
            || self.pending != PendingWork::None
            || write_busy
    }

    /// 0.0 → 1.0 then wrap (slow left→right sweep for the in-panel load bar).
    pub fn load_bar_ratio(&self) -> f64 {
        let Some(started) = self.load_started else {
            return 0.0;
        };
        const PERIOD_SECS: f64 = 2.4;
        (started.elapsed().as_secs_f64() % PERIOD_SECS) / PERIOD_SECS
    }

    fn begin_list_load(&mut self, refresh_meta: bool, status: &str) {
        self.list_loading = true;
        self.load_started = Some(Instant::now());
        self.status_line = status.into();
        self.pending = PendingWork::List { refresh_meta };
    }

    fn begin_detail_load(&mut self, refresh_meta: bool, status: &str) {
        self.detail_loading = true;
        self.load_started = Some(Instant::now());
        self.status_line = status.into();
        self.pending = PendingWork::Detail { refresh_meta };
    }

    // —— DB setup ——

    pub fn db_select_next(&mut self) {
        match self.db.phase {
            DbSetupPhase::ChooseLocation => {
                let n = DbPathChoice::ALL.len();
                self.db.selected = (self.db.selected + 1) % n;
            }
            DbSetupPhase::ConfirmExist => {
                let n = DbExistChoice::ALL.len();
                self.db.selected = (self.db.selected + 1) % n;
            }
            DbSetupPhase::CustomInput => {}
        }
        self.db.error = None;
    }

    pub fn db_select_prev(&mut self) {
        match self.db.phase {
            DbSetupPhase::ChooseLocation => {
                let n = DbPathChoice::ALL.len();
                self.db.selected = if self.db.selected == 0 {
                    n - 1
                } else {
                    self.db.selected - 1
                };
            }
            DbSetupPhase::ConfirmExist => {
                let n = DbExistChoice::ALL.len();
                self.db.selected = if self.db.selected == 0 {
                    n - 1
                } else {
                    self.db.selected - 1
                };
            }
            DbSetupPhase::CustomInput => {}
        }
        self.db.error = None;
    }

    pub fn db_confirm(&mut self) {
        match self.db.phase {
            DbSetupPhase::ChooseLocation => match self.db.path_choice() {
                DbPathChoice::UseDefault => {
                    self.db_try_path(self.db.default_path.clone());
                }
                DbPathChoice::CustomPath => {
                    self.db.phase = DbSetupPhase::CustomInput;
                    self.db.input = display_path(&self.db.default_path);
                    self.db.error = None;
                    self.status_line = "type SQLite path · Enter · Esc back".into();
                }
            },
            DbSetupPhase::CustomInput => {
                let raw = self.db.input.trim();
                if raw.is_empty() {
                    self.db.error = Some("path is empty".into());
                    return;
                }
                let path = expand_tilde(raw);
                self.db_try_path(path);
            }
            DbSetupPhase::ConfirmExist => {
                let Some(path) = self.db.pending_path.clone() else {
                    self.db.phase = DbSetupPhase::ChooseLocation;
                    return;
                };
                let overwrite = matches!(self.db.exist_choice(), DbExistChoice::Overwrite);
                self.db_open(path, overwrite);
            }
        }
    }

    fn db_try_path(&mut self, path: PathBuf) {
        if path.exists() {
            self.db.pending_path = Some(path.clone());
            self.db.phase = DbSetupPhase::ConfirmExist;
            self.db.selected = 0;
            self.db.error = None;
            self.status_line = format!("DB exists at {} · ↑↓ · Enter", display_path(&path));
        } else {
            self.db_open(path, false);
        }
    }

    fn db_open(&mut self, path: PathBuf, overwrite: bool) {
        match Store::open(&path, overwrite) {
            Ok(store) => {
                self.db.error = None;
                self.finish_store(store);
            }
            Err(e) => {
                self.db.error = Some(format!("{e:#}"));
                self.db.phase = DbSetupPhase::ChooseLocation;
                self.db.pending_path = None;
                self.status_line = "failed to open DB · ↑↓ choose · Enter · q quit".into();
            }
        }
    }

    pub fn db_back(&mut self) {
        match self.db.phase {
            DbSetupPhase::CustomInput | DbSetupPhase::ConfirmExist => {
                self.db.phase = DbSetupPhase::ChooseLocation;
                self.db.pending_path = None;
                self.db.error = None;
                self.db.selected = 0;
                self.status_line = "choose where to store SQLite · ↑↓ · Enter · q quit".into();
            }
            DbSetupPhase::ChooseLocation => self.quit(),
        }
    }

    pub fn db_input_char(&mut self, c: char) {
        if self.db.phase != DbSetupPhase::CustomInput {
            return;
        }
        if !c.is_control() {
            self.db.input.push(c);
            self.db.error = None;
        }
    }

    pub fn db_input_backspace(&mut self) {
        if self.db.phase != DbSetupPhase::CustomInput {
            return;
        }
        self.db.input.pop();
        self.db.error = None;
    }

    pub fn open_settings(&mut self) {
        self.screen = Screen::Settings;
        self.settings_selected = 0;
        self.status_line = format!(
            "settings · db {} · ↑↓ · Enter · ? help · Esc back",
            self.db_path_display()
        );
    }

    pub fn settings_select_next(&mut self) {
        let n = SettingsItem::ALL.len();
        self.settings_selected = (self.settings_selected + 1) % n;
    }

    pub fn settings_select_prev(&mut self) {
        let n = SettingsItem::ALL.len();
        self.settings_selected = if self.settings_selected == 0 {
            n - 1
        } else {
            self.settings_selected - 1
        };
    }

    pub fn settings_activate(&mut self) {
        match SettingsItem::ALL[self.settings_selected] {
            SettingsItem::PrivacyTrim => {
                self.privacy_trim = !self.privacy_trim;
                self.persist_privacy();
                self.status_line = if self.privacy_trim {
                    "privacy ON · wallet first 4 · RPC https://xyz… (host only)".into()
                } else {
                    "privacy OFF · full wallet + RPC host".into()
                };
            }
            SettingsItem::PriceMode => {
                self.price_mode = self.price_mode.next();
                if let Some(store) = &self.store {
                    let _ = store.set_price_mode(self.price_mode.as_setting());
                }
                self.status_line = format!(
                    "USD estimate → {} · reopen detail / r to refresh",
                    self.price_mode.short()
                );
            }
            SettingsItem::SwapMode => {
                self.swap_mode = if self.swap_mode == "titan" {
                    "jupiter".into()
                } else {
                    "titan".into()
                };
                if let Some(store) = &self.store {
                    let _ = store.set_swap_mode(&self.swap_mode);
                }
                self.status_line = format!("swap router → {}", self.swap_mode);
            }
            SettingsItem::BudgetMode => {
                self.budget_mode = if self.budget_mode == "manual" {
                    "auto".into()
                } else {
                    "manual".into()
                };
                if let Some(store) = &self.store {
                    let _ = store.set_tx_budget_mode(&self.budget_mode);
                }
                self.status_line = format!(
                    "tx v1 CU → {} · tip is separate (priority fee setting)",
                    self.budget_mode
                );
            }
            SettingsItem::PriorityFee => {
                let idx = TIP_PRESETS
                    .iter()
                    .position(|&p| p == self.tip_lamports)
                    .unwrap_or(3); // default slot = 5_000
                self.tip_lamports = TIP_PRESETS[(idx + 1) % TIP_PRESETS.len()];
                if let Some(store) = &self.store {
                    let _ = store.set_tx_tip_lamports(self.tip_lamports);
                }
                self.status_line = format!("priority fee → {}", tip_label(self.tip_lamports));
            }
            SettingsItem::WriteAccess => {
                if !sol_clmm_manager::WRITE_ENABLED {
                    self.status_line =
                        "read-only build · cargo run -p sol-clmm-tui --features write".into();
                    return;
                }
                let next = !matches!(self.write_allowed, Some(true));
                self.write_allowed = Some(next);
                if next {
                    // Re-enable: clear "don't ask again" so consent can return if unset later.
                    self.write_prompt_skip = false;
                }
                if let Some(store) = &self.store {
                    let _ = store.set_write_allowed(next);
                    if next {
                        let _ = store.set_write_prompt_skip(false);
                    }
                }
                self.status_line = if next {
                    "writes ENABLED · n create · claims need keypair + confirm".into()
                } else {
                    "writes DISABLED · reads only (create/claim blocked)".into()
                };
            }
            SettingsItem::DlmmStrategy => {
                #[cfg(feature = "write")]
                {
                    let next =
                        sol_clmm_manager::DlmmStrategy::from_setting(&self.dlmm_strategy).next();
                    self.dlmm_strategy = next.setting_key().into();
                    if let Some(store) = &self.store {
                        let _ = store.set_dlmm_strategy(&self.dlmm_strategy);
                    }
                    self.status_line = format!("DLMM strategy → {}", self.dlmm_strategy);
                }
                #[cfg(not(feature = "write"))]
                {
                    self.status_line = "DLMM strategy needs --features write".into();
                }
            }
            SettingsItem::JupiterKey => {
                self.api_key_target = ApiKeyTarget::Jupiter;
                self.screen = Screen::SettingsApiKey;
                self.settings_api_key = WizardState::new();
                self.settings_api_key.step = SetupStep::Input;
                self.settings_api_key.input = self.jupiter_api_key.clone().unwrap_or_default();
                self.status_line = format!(
                    "Jupiter key {} · Enter save · empty clears · Esc cancel",
                    Self::mask_key(&self.jupiter_api_key)
                );
            }
            SettingsItem::TitanKey => {
                self.api_key_target = ApiKeyTarget::Titan;
                self.screen = Screen::SettingsApiKey;
                self.settings_api_key = WizardState::new();
                self.settings_api_key.step = SetupStep::Input;
                self.settings_api_key.input = self.titan_api_key.clone().unwrap_or_default();
                self.status_line = format!(
                    "Titan key {} · Enter save · empty clears · Esc cancel",
                    Self::mask_key(&self.titan_api_key)
                );
            }
            SettingsItem::ApiPaidFast => {
                self.api_paid_fast = !self.api_paid_fast;
                if let Some(store) = &self.store {
                    let _ = store.set_api_paid_fast(self.api_paid_fast);
                }
                self.status_line = if self.api_paid_fast {
                    "API paid/fast ON · short throttle when a key is set".into()
                } else {
                    "API paid/fast OFF · free-tier throttle (safer for sandbox)".into()
                };
            }
            SettingsItem::ChangeRpc => {
                self.screen = Screen::SettingsRpc;
                self.settings_rpc = WizardState::new();
                self.settings_rpc.step = SetupStep::Input;
                self.settings_rpc.input = self.rpc_url.clone();
                self.status_line = "edit RPC URL · Enter save · Esc cancel".into();
            }
            SettingsItem::ChangeWallet => {
                self.wallet = None;
                self.cli_wallet = None;
                self.positions.clear();
                self.detail = None;
                if let Some(store) = &self.store {
                    let _ = store.clear_wallet();
                }
                self.screen = Screen::Setup;
                self.setup = WizardState::new();
                self.status_line = "change wallet · ↑↓ choose · Enter · Esc quit".into();
            }
            SettingsItem::Back => {
                self.screen = Screen::Positions;
                self.status_line = self.positions_status_line();
            }
        }
    }

    pub fn settings_api_key_char(&mut self, c: char) {
        if !c.is_control() {
            self.settings_api_key.input.push(c);
            self.settings_api_key.error = None;
        }
    }

    pub fn settings_api_key_backspace(&mut self) {
        self.settings_api_key.input.pop();
        self.settings_api_key.error = None;
    }

    pub fn settings_api_key_cancel(&mut self) {
        self.screen = Screen::Settings;
        self.status_line = "settings · ↑↓ · Enter · Esc back".into();
    }

    pub fn settings_api_key_submit(&mut self) {
        let raw = self.settings_api_key.input.trim();
        let value = if raw.is_empty() {
            None
        } else {
            Some(raw.to_string())
        };
        match self.api_key_target {
            ApiKeyTarget::Jupiter => {
                self.jupiter_api_key = value.clone();
                if let Some(store) = &self.store {
                    let _ = store.set_jupiter_api_key(value.as_deref());
                }
                self.status_line = if value.is_some() {
                    "Jupiter API key saved (sqlite)".into()
                } else {
                    "Jupiter API key cleared · keyless + throttle".into()
                };
            }
            ApiKeyTarget::Titan => {
                self.titan_api_key = value.clone();
                if let Some(store) = &self.store {
                    let _ = store.set_titan_api_key(value.as_deref());
                }
                self.status_line = if value.is_some() {
                    "Titan API key saved (sqlite)".into()
                } else {
                    "Titan API key cleared · Titan mode needs a key".into()
                };
            }
        }
        self.screen = Screen::Settings;
    }

    pub fn settings_rpc_char(&mut self, c: char) {
        if !c.is_control() {
            self.settings_rpc.input.push(c);
            self.settings_rpc.error = None;
        }
    }

    pub fn settings_rpc_backspace(&mut self) {
        self.settings_rpc.input.pop();
        self.settings_rpc.error = None;
    }

    pub fn settings_rpc_submit(&mut self) {
        let raw = self.settings_rpc.input.trim();
        if raw.is_empty() {
            self.settings_rpc.error = Some("URL is empty".into());
            return;
        }
        if !(raw.starts_with("http://") || raw.starts_with("https://")) {
            self.settings_rpc.error = Some("URL must start with http:// or https://".into());
            return;
        }
        self.rpc_url = raw.to_string();
        self.rpc_dirty = true;
        self.persist_rpc();
        self.settings_rpc.error = None;
        self.enter_positions();
    }

    pub fn settings_rpc_cancel(&mut self) {
        self.screen = Screen::Settings;
        self.status_line = "settings · ↑↓ · Enter · Esc back".into();
    }

    /// Spawn list fetch on a background thread (UI keeps drawing the load bar).
    pub fn spawn_list_fetch(&mut self, refresh_meta: bool) {
        if self.bg_rx.is_some() {
            return;
        }
        let rpc_url = self.rpc_url.clone();
        let wallet = self.wallet;
        let db_path = self.store.as_ref().map(|s| s.path().to_path_buf());
        let (tx, rx) = mpsc::channel();
        self.bg_rx = Some(rx);
        self.list_loading = true;
        self.load_started = Some(Instant::now());

        let price_mode = self.price_mode;
        let keys = self.price_keys();
        thread::spawn(move || {
            let sol = rpc::connect(&rpc_url);
            let meter = RpcMeter::default();
            let rpc = Rpc::new(&sol, &meter);
            let store = db_path.and_then(|p| Store::open(&p, false).ok());
            let status = rpc::fetch_status(&rpc, wallet.as_ref());
            let api = Client::new(price_mode, keys);
            let positions = match wallet {
                Some(pk) => api.list_positions_lenient(
                    &rpc,
                    &pk,
                    store.as_ref(),
                    VenueFilter::All,
                    refresh_meta,
                ),
                None => Ok(Vec::new()),
            };
            let _ = tx.send(BgFetchResult::List {
                status,
                positions,
                meter,
            });
        });
    }

    /// Spawn detail fetch on a background thread.
    pub fn spawn_detail_fetch(&mut self, refresh_meta: bool) {
        if self.bg_rx.is_some() {
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            self.detail_loading = false;
            self.load_started = None;
            self.status_line = "select a position first".into();
            return;
        };
        let rpc_url = self.rpc_url.clone();
        let db_path = self.store.as_ref().map(|s| s.path().to_path_buf());
        let price_mode = self.price_mode;
        let keys = self.price_keys();
        let wallet = self.wallet;
        let (tx, rx) = mpsc::channel();
        self.bg_rx = Some(rx);
        self.detail_loading = true;
        self.load_started = Some(Instant::now());

        thread::spawn(move || {
            let sol = rpc::connect(&rpc_url);
            let meter = RpcMeter::default();
            let rpc = Rpc::new(&sol, &meter);
            let store = db_path.and_then(|p| Store::open(&p, false).ok());
            let api = Client::new(price_mode, keys);
            // Always refresh wallet SOL/ATA with detail (SOL pairs change after wrap/unwrap).
            let result =
                api.position_detail(&rpc, &row, store.as_ref(), refresh_meta, wallet.as_ref());
            let _ = tx.send(BgFetchResult::Detail {
                result: Box::new(result),
                meter,
            });
        });
    }

    /// Non-blocking poll of the background fetch; returns true if still waiting.
    pub fn poll_bg_fetch(&mut self) -> bool {
        let Some(rx) = &self.bg_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.bg_rx = None;
                self.apply_bg_result(result);
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                self.bg_rx = None;
                self.list_loading = false;
                self.detail_loading = false;
                #[cfg(feature = "write")]
                {
                    self.wallet_loading = false;
                }
                self.load_started = None;
                #[cfg(feature = "write")]
                if let Some(c) = self.write_confirm.as_mut()
                    && c.phase == crate::write_flow::ConfirmPhase::Busy
                {
                    c.phase = crate::write_flow::ConfirmPhase::Failed;
                    c.last_error = Some("send interrupted".into());
                    self.status_line = "send interrupted · Retry or Cancel".into();
                    return false;
                }
                self.status_line = "fetch interrupted · press r to retry".into();
                false
            }
        }
    }

    fn apply_bg_result(&mut self, result: BgFetchResult) {
        match result {
            BgFetchResult::List {
                status,
                positions,
                meter,
            } => {
                self.rpc_meter.merge(&meter);
                self.list_loading = false;
                self.load_started = None;
                match status {
                    Ok(s) => self.status = Some(s),
                    Err(e) => {
                        self.status_line =
                            format!("rpc error: {e:#} · press r to retry · q to quit");
                        return;
                    }
                }
                match positions {
                    Ok(rows) => {
                        self.positions = rows;
                        if self.positions.is_empty() {
                            self.table_state.select(None);
                        } else if self.table_state.selected().is_none() {
                            self.table_state.select(Some(0));
                        }
                        self.sync_table_selection_after_filter();
                        // Positions list stays clean — no tx chatter in keys.
                        self.status_line = self.positions_status_line();
                    }
                    Err(e) => {
                        self.status_line =
                            format!("rpc error: {e:#} · press r to retry · q to quit");
                    }
                }
            }
            #[cfg(feature = "write")]
            BgFetchResult::Wallet { result, meter } => {
                self.rpc_meter.merge(&meter);
                self.wallet_loading = false;
                self.load_started = None;
                if self.screen != Screen::Wallet {
                    return;
                }
                match *result {
                    Ok((sol, tokens)) => {
                        let wiz = crate::wallet_flow::WalletWizard::new(
                            sol,
                            tokens,
                            sol_clmm_manager::SOL_FEE_RESERVE_LAMPORTS,
                        );
                        self.status_line = wiz.status_hint();
                        self.wallet_wiz = Some(wiz);
                    }
                    Err(e) => {
                        self.wallet_wiz = None;
                        self.status_line =
                            format!("wallet balances: {e:#} · Esc back · w retry");
                    }
                }
            }
            BgFetchResult::Detail { result, meter } => {
                self.rpc_meter.merge(&meter);
                self.detail_loading = false;
                self.load_started = None;
                match *result {
                    Ok(d) => {
                        // Start zoomed out to full series (narrow fit was unreadable on SOL/USDC).
                        let step = d.pool.tick_spacing.max(1) as i32;
                        let span = (d.series_max_tick - d.series_min_tick).max(step);
                        self.detail_chart_half_spacings = (span / step / 2)
                            .max(sol_clmm_manager::CHART_HALF_SPACINGS_MIN);
                        self.detail = Some(d);
                        self.detail_error = None;
                        self.status_line = if self.writes_live() {
                            "detail · full chart · + zoom · m manage · y history · Esc".into()
                        } else {
                            "detail · full chart · + zoom · y history · r · Esc".into()
                        };
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        // Closed / burned position still in the stale list.
                        if msg.contains("AccountNotFound") {
                            if let Some(addr) = self.selected_row().map(|r| r.address) {
                                self.positions.retain(|p| p.address != addr);
                                self.sync_table_selection_after_filter();
                            }
                            self.detail = None;
                            self.detail_error = None;
                            self.screen = Screen::Positions;
                            self.status_line =
                                "position gone on-chain · dropped from list · r refresh".into();
                        } else {
                            self.detail_error = Some(msg);
                            self.status_line = "detail error · r retry · Esc back".into();
                        }
                    }
                }
            }
            #[cfg(feature = "write")]
            BgFetchResult::Write {
                result,
                balances,
                token_delta,
            } => {
                self.load_started = None;
                match *result {
                    Ok(out) => {
                        let paid = out.paid_label();
                        let op = self
                            .write_confirm
                            .as_ref()
                            .map(|c| c.plan.op.label())
                            .unwrap_or("tx");
                        if let Some(store) = &self.store {
                            let c = self.write_confirm.as_ref();
                            let pool_meta = c.and_then(|x| x.pool_meta());
                            let _ = store.record_write_tx(
                                &out.signature,
                                op,
                                c.map(|x| x.plan.venue_label.as_str()),
                                c.and_then(|x| x.position_pk.as_deref()),
                                c.and_then(|x| x.pool_pk.as_deref()),
                                Some(out.budget.priority_fee_lamports),
                                Some(out.fee_lamports),
                                out.sol_spent_lamports,
                                c.map(|x| x.plan.summary.as_str()),
                                Some(paid.as_str()),
                                pool_meta.as_ref(),
                                token_delta.as_ref(),
                            );
                        }
                        if let Some(c) = self.write_confirm.as_mut() {
                            c.phase = crate::write_flow::ConfirmPhase::Done;
                            c.signature = Some(out.signature.clone());
                            c.budget_label = Some(out.budget.label());
                            c.paid_label = Some(paid.clone());
                            c.wallet_balances = balances.clone();
                        }
                        // Every landed tx changes SOL / ATA — apply to detail immediately.
                        if let Some(bal) = balances {
                            if let Some(d) = self.detail.as_mut() {
                                d.wallet_balances = Some(bal);
                            }
                        }
                        self.status_line = "landed · wallet balances updated · Enter back".into();
                    }
                    Err(e) => {
                        if let Some(c) = self.write_confirm.as_mut() {
                            c.phase = crate::write_flow::ConfirmPhase::Failed;
                            c.last_error = Some(e.to_string());
                        }
                        self.status_line = "send failed · Retry or Cancel".into();
                    }
                }
            }
        }
    }

    pub fn open_selected_detail(&mut self) {
        if self.is_busy() {
            return;
        }
        if self.selected_row().is_none() {
            self.status_line = "select a position first".into();
            return;
        }
        self.dismiss_links_overlay();
        self.screen = Screen::Detail;
        self.detail = None;
        self.detail_error = None;
        self.begin_detail_load(false, "loading pool + tick arrays…");
    }

    fn selected_row(&self) -> Option<&PositionRow> {
        let sel = self.table_state.selected()?;
        let idx = *self.filtered_indices().get(sel)?;
        self.positions.get(idx)
    }

    /// Zoom detail liquidity chart in (`+`) / out (`-`) / fit position (`0`).
    pub fn detail_chart_zoom(&mut self, dir: i8) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let step = d.pool.tick_spacing.max(1) as i32;
        let span = (d.series_max_tick - d.series_min_tick).max(step);
        let max_half = (span / step / 2).max(sol_clmm_manager::CHART_HALF_SPACINGS_MIN);
        let prev = self
            .detail_chart_half_spacings
            .min(max_half)
            .max(sol_clmm_manager::CHART_HALF_SPACINGS_MIN);
        match dir {
            1 => {
                let next = ((prev * 2) / 3).max(sol_clmm_manager::CHART_HALF_SPACINGS_MIN);
                self.detail_chart_half_spacings = if next >= prev {
                    (prev - 1).max(sol_clmm_manager::CHART_HALF_SPACINGS_MIN)
                } else {
                    next
                };
            }
            -1 => {
                self.detail_chart_half_spacings =
                    (prev.saturating_mul(3) / 2).max(prev + 1).min(max_half);
            }
            _ => {
                let cur = d.pool.tick_current_index;
                let dist = (d.row.tick_lower - cur)
                    .abs()
                    .max((d.row.tick_upper - cur).abs())
                    .max(step);
                let need = ((dist * 5) / (step * 4) + 2)
                    .max(sol_clmm_manager::CHART_HALF_SPACINGS_MIN);
                self.detail_chart_half_spacings = need.min(max_half);
            }
        }
        let half = self
            .detail_chart_half_spacings
            .min(max_half)
            .max(sol_clmm_manager::CHART_HALF_SPACINGS_MIN);
        self.status_line = if dir == 1 && half == prev {
            format!("zoom min ±{half} · - wider · 0 fit")
        } else if dir == -1 && half == prev {
            format!("zoom full ±{half} · + tighter · 0 fit")
        } else if half >= max_half {
            format!("chart full ±{half} · + zoom in · 0 fit")
        } else {
            format!("chart ±{half}/{max_half} · +/- · 0 fit")
        };
    }

    pub fn close_detail(&mut self) {
        self.dismiss_links_overlay();
        self.screen = Screen::Positions;
        self.detail = None;
        self.detail_error = None;
        self.detail_loading = false;
        self.status_line = self.positions_status_line();
    }

    // —— wallet setup ——

    pub fn setup_select_next(&mut self) {
        if self.setup.step != SetupStep::Choose {
            return;
        }
        let n = WalletSource::ALL.len();
        self.setup.selected = (self.setup.selected + 1) % n;
        self.setup.error = None;
    }

    pub fn setup_select_prev(&mut self) {
        if self.setup.step != SetupStep::Choose {
            return;
        }
        let n = WalletSource::ALL.len();
        self.setup.selected = if self.setup.selected == 0 {
            n - 1
        } else {
            self.setup.selected - 1
        };
        self.setup.error = None;
    }

    pub fn setup_confirm_choice(&mut self) {
        if self.setup.step != SetupStep::Choose {
            return;
        }
        self.setup.step = SetupStep::Input;
        self.setup.input.clear();
        self.setup.error = None;
        self.status_line = format!("{} · Esc back", self.setup.wallet_source().prompt());
    }

    pub fn setup_back(&mut self) {
        match self.setup.step {
            SetupStep::Input => {
                self.setup.step = SetupStep::Choose;
                self.setup.input.clear();
                self.setup.error = None;
                self.status_line =
                    "wallet / keypair not provided · ↑↓ choose · Enter · Esc quit".into();
            }
            SetupStep::Choose => self.quit(),
        }
    }

    pub fn setup_input_char(&mut self, c: char) {
        if self.setup.step != SetupStep::Input {
            return;
        }
        if !c.is_control() {
            self.setup.input.push(c);
            self.setup.error = None;
        }
    }

    pub fn setup_input_backspace(&mut self) {
        if self.setup.step != SetupStep::Input {
            return;
        }
        self.setup.input.pop();
        self.setup.error = None;
    }

    pub fn setup_submit(&mut self) {
        if self.setup.step != SetupStep::Input {
            return;
        }
        let raw = self.setup.input.trim();
        if raw.is_empty() {
            self.setup.error = Some("value is empty".into());
            return;
        }

        let (wallet, keypair_path) = match self.setup.wallet_source() {
            WalletSource::Pubkey => match raw.parse::<Pubkey>() {
                Ok(pk) => (pk, None),
                Err(e) => {
                    self.setup.error = Some(format!("invalid pubkey: {e}"));
                    return;
                }
            },
            WalletSource::Keypair => {
                let path = expand_tilde(raw);
                match read_keypair_file(&path) {
                    Ok(kp) => (kp.pubkey(), Some(path.display().to_string())),
                    Err(e) => {
                        self.setup.error = Some(format!("keypair {}: {e}", path.display()));
                        return;
                    }
                }
            }
        };

        self.wallet = Some(wallet);
        self.persist_wallet(&wallet, keypair_path.as_deref());
        self.setup.error = None;
        // Offer private RPC only if none saved yet.
        let skip_offer = self
            .store
            .as_ref()
            .and_then(|s| s.load_settings().ok())
            .map(|s| s.rpc_url.is_some())
            .unwrap_or(false);
        self.after_wallet_ready(skip_offer);
    }

    // —— RPC offer ——

    pub fn rpc_select_next(&mut self) {
        if self.rpc_offer.step != SetupStep::Choose {
            return;
        }
        let n = RpcChoice::ALL.len();
        self.rpc_offer.selected = (self.rpc_offer.selected + 1) % n;
        self.rpc_offer.error = None;
    }

    pub fn rpc_select_prev(&mut self) {
        if self.rpc_offer.step != SetupStep::Choose {
            return;
        }
        let n = RpcChoice::ALL.len();
        self.rpc_offer.selected = if self.rpc_offer.selected == 0 {
            n - 1
        } else {
            self.rpc_offer.selected - 1
        };
        self.rpc_offer.error = None;
    }

    pub fn rpc_confirm_choice(&mut self) {
        if self.rpc_offer.step != SetupStep::Choose {
            return;
        }
        match self.rpc_offer.rpc_choice() {
            RpcChoice::KeepPublic => {
                self.persist_rpc();
                self.maybe_write_consent_then_positions();
            }
            RpcChoice::UsePrivate => {
                self.rpc_offer.step = SetupStep::Input;
                self.rpc_offer.input.clear();
                self.rpc_offer.error = None;
                self.status_line = "paste private HTTPS RPC URL · Enter · Esc back".into();
            }
        }
    }

    pub fn rpc_back(&mut self) {
        match self.rpc_offer.step {
            SetupStep::Input => {
                self.rpc_offer.step = SetupStep::Choose;
                self.rpc_offer.input.clear();
                self.rpc_offer.error = None;
                self.status_line = "public RPC detected · ↑↓ choose · Enter".into();
            }
            SetupStep::Choose => {
                self.screen = Screen::Setup;
                self.setup = WizardState::new();
                self.wallet = None;
                if let Some(store) = &self.store {
                    let _ = store.clear_wallet();
                }
                self.status_line =
                    "wallet / keypair not provided · ↑↓ choose · Enter · Esc quit".into();
            }
        }
    }

    pub fn rpc_input_char(&mut self, c: char) {
        if self.rpc_offer.step != SetupStep::Input {
            return;
        }
        if !c.is_control() {
            self.rpc_offer.input.push(c);
            self.rpc_offer.error = None;
        }
    }

    pub fn rpc_input_backspace(&mut self) {
        if self.rpc_offer.step != SetupStep::Input {
            return;
        }
        self.rpc_offer.input.pop();
        self.rpc_offer.error = None;
    }

    pub fn rpc_submit_private(&mut self) {
        if self.rpc_offer.step != SetupStep::Input {
            return;
        }
        let raw = self.rpc_offer.input.trim();
        if raw.is_empty() {
            self.rpc_offer.error = Some("URL is empty".into());
            return;
        }
        if !(raw.starts_with("http://") || raw.starts_with("https://")) {
            self.rpc_offer.error = Some("URL must start with http:// or https://".into());
            return;
        }
        if rpc::is_public_rpc(raw) {
            self.rpc_offer.error = Some("that still looks like a public Solana endpoint".into());
            return;
        }
        self.rpc_url = raw.to_string();
        self.rpc_dirty = true;
        self.persist_rpc();
        self.rpc_offer.error = None;
        self.maybe_write_consent_then_positions();
    }

    /// Hide pubkey overlay and restore the previous status (second `p`).
    fn restore_links_overlay(&mut self) {
        if self.links_overlay {
            if let Some(prev) = self.status_before_links.take() {
                self.status_line = prev;
            }
            self.links_overlay = false;
        }
    }

    /// Drop overlay without restoring (navigation / refresh will set a new status).
    fn dismiss_links_overlay(&mut self) {
        self.links_overlay = false;
        self.status_before_links = None;
    }

    /// Full position pubkey in status (`p` · again to hide). Never trimmed.
    pub fn toggle_selected_position_link(&mut self) {
        if self.links_overlay {
            self.restore_links_overlay();
            return;
        }
        let Some(row) = self.selected_row() else {
            self.status_line = "select a position first · then p for full pubkey".into();
            return;
        };
        let pk = row.address.to_string();
        self.status_before_links = Some(self.status_line.clone());
        self.links_overlay = true;
        self.status_line = format!("position {pk} · p hide");
    }

    /// Full position + pool pubkeys in status (`p` · again to hide).
    pub fn toggle_detail_position_link(&mut self) {
        if self.links_overlay {
            self.restore_links_overlay();
            return;
        }
        let Some(d) = &self.detail else {
            self.status_line = "no detail loaded · p for full pubkey".into();
            return;
        };
        let pos = d.row.address.to_string();
        let pool = d.pool.address.to_string();
        self.status_before_links = Some(self.status_line.clone());
        self.links_overlay = true;
        self.status_line = format!("position {pos} · pool {pool} · p hide");
    }

    pub fn select_next(&mut self) {
        self.dismiss_links_overlay();
        let len = self.filtered_indices().len();
        if len == 0 {
            return;
        }
        let i = self
            .table_state
            .selected()
            .map(|i| (i + 1) % len)
            .unwrap_or(0);
        self.table_state.select(Some(i));
    }

    pub fn select_prev(&mut self) {
        self.dismiss_links_overlay();
        let len = self.filtered_indices().len();
        if len == 0 {
            return;
        }
        let i = self
            .table_state
            .selected()
            .map(|i| if i == 0 { len - 1 } else { i - 1 })
            .unwrap_or(0);
        self.table_state.select(Some(i));
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    #[cfg(feature = "write")]
    pub fn write_settings_snapshot(&self) -> AppSettings {
        AppSettings {
            wallet_pubkey: self.wallet.map(|p| p.to_string()),
            keypair_path: self.keypair_path.clone(),
            rpc_url: Some(self.rpc_url.clone()),
            privacy_trim: self.privacy_trim,
            price_mode: Some(self.price_mode.as_setting().to_string()),
            jupiter_api_key: self.jupiter_api_key.clone(),
            titan_api_key: self.titan_api_key.clone(),
            api_paid_fast: self.api_paid_fast,
            write_allowed: self.write_allowed,
            write_prompt_skip: self.write_prompt_skip,
            swap_mode: Some(self.swap_mode.clone()),
            tx_budget_mode: Some(self.budget_mode.clone()),
            tx_cu_limit: None,
            tx_tip_lamports: Some(self.tip_lamports),
            tx_loaded_data: None,
            tx_heap: None,
            tx_cu_margin_bps: None,
            tx_tip_floor: Some(0),
            tx_tip_ceiling: Some(25_000),
            dlmm_strategy: Some(self.dlmm_strategy.clone()),
        }
    }

    #[cfg(feature = "write")]
    fn write_settings_for_confirm(&self, tip_lamports: u64) -> AppSettings {
        let mut s = self.write_settings_snapshot();
        s.tx_tip_lamports = Some(tip_lamports);
        s
    }

    #[cfg(feature = "write")]
    pub fn open_create(&mut self) {
        if !self.writes_live() {
            self.status_line = if !sol_clmm_manager::WRITE_ENABLED {
                "read-only build · --features write".into()
            } else if self.write_allowed != Some(true) {
                "writes disabled · settings → write access".into()
            } else {
                "create needs a keypair path · setup wallet from file".into()
            };
            return;
        }
        self.create = Some(
            crate::write_flow::CreateWizard::new().with_swap_router(&self.swap_mode),
        );
        self.screen = Screen::CreatePosition;
        self.status_line = "create position · Esc cancel".into();
    }

    #[cfg(feature = "write")]
    pub fn cancel_create(&mut self) {
        self.create = None;
        self.screen = Screen::Positions;
        self.status_line = self.positions_status_line();
    }

    #[cfg(feature = "write")]
    pub fn create_fetch_pool(&mut self, rpc: &solana_client::rpc_client::RpcClient) {
        let Some(wiz) = self.create.as_ref() else {
            return;
        };
        let pool: Pubkey = match wiz.pool_input.trim().parse() {
            Ok(p) => p,
            Err(_) => {
                if let Some(w) = self.create.as_mut() {
                    w.on_preview_failed("invalid pool pubkey".into());
                }
                return;
            }
        };
        let venue = wiz.venue();
        self.status_line = format!("fetching {} pool {pool}…", venue.short());
        let client = Client::new(self.price_mode, self.price_keys());
        let wallet = self.wallet;
        let result = {
            let traced = Rpc::new(rpc, &self.rpc_meter);
            let store = self.store.as_ref();
            client.pool_preview(&traced, venue, &pool, wallet.as_ref(), store, true)
        };
        match result {
            Ok(preview) => {
                let sym = format!("{}/{}", preview.token_a.symbol, preview.token_b.symbol);
                let active = preview.pool.tick_current_index;
                let bal = preview
                    .balances
                    .as_ref()
                    .map(|b| {
                        b.summary_line(
                            &preview.token_a.symbol,
                            &preview.token_b.symbol,
                            preview.token_a.decimals,
                            preview.token_b.decimals,
                        )
                    })
                    .unwrap_or_else(|| "balances ?".into());
                if let Some(w) = self.create.as_mut() {
                    w.on_preview_loaded(preview);
                }
                self.status_line = format!(
                    "{sym} · active {active} · {bal} · ←→ ticks · +/- zoom · Enter"
                );
            }
            Err(e) => {
                if let Some(w) = self.create.as_mut() {
                    w.on_preview_failed(e.to_string());
                }
                self.status_line = format!("pool fetch failed · {e}");
            }
        }
    }

    #[cfg(feature = "write")]
    pub fn open_manage(&mut self) {
        if !self.writes_live() {
            self.status_line = if !sol_clmm_manager::WRITE_ENABLED {
                "read-only build · --features write".into()
            } else if self.write_allowed != Some(true) {
                "writes disabled · settings → write access".into()
            } else {
                "manage needs a keypair path · setup wallet from file".into()
            };
            return;
        }
        if self.detail.is_none() {
            self.status_line = "no detail loaded".into();
            return;
        }
        self.manage_choice = 0;
        self.screen = Screen::Manage;
        self.status_line = "manage · ↑↓ · Enter · Esc · p full pubkey".into();
    }

    #[cfg(feature = "write")]
    pub fn manage_select_next(&mut self) {
        let n = ManageChoice::ALL.len();
        self.manage_choice = (self.manage_choice + 1) % n;
    }

    #[cfg(feature = "write")]
    pub fn manage_select_prev(&mut self) {
        let n = ManageChoice::ALL.len();
        self.manage_choice = if self.manage_choice == 0 {
            n - 1
        } else {
            self.manage_choice - 1
        };
    }

    #[cfg(feature = "write")]
    pub fn manage_close(&mut self) {
        self.screen = Screen::Detail;
        self.status_line = if self.writes_live() {
            "detail · m manage · y history · p pubkey · r refresh · Esc".into()
        } else {
            "detail · y history · Esc back".into()
        };
    }

    #[cfg(feature = "write")]
    pub fn toggle_manage_links(&mut self) {
        if self.links_overlay {
            self.restore_links_overlay();
            return;
        }
        let Some(d) = &self.detail else {
            return;
        };
        let pos = d.row.address.to_string();
        let pool = d.pool.address.to_string();
        self.status_before_links = Some(self.status_line.clone());
        self.links_overlay = true;
        self.status_line = format!("position {pos} · pool {pool} · p hide");
    }

    #[cfg(feature = "write")]
    pub fn manage_activate(&mut self, rpc: &solana_client::rpc_client::RpcClient) {
        let choice = ManageChoice::ALL[self.manage_choice.min(ManageChoice::ALL.len() - 1)];
        match choice {
            ManageChoice::Deposit => self.open_manage_deposit(rpc),
            ManageChoice::Compound => self.open_manage_compound(),
            ManageChoice::Withdraw => self.open_manage_withdraw(
                crate::write_flow::WithdrawKind::Withdraw,
            ),
            ManageChoice::ClaimWithdraw => self.open_manage_withdraw(
                crate::write_flow::WithdrawKind::ClaimWithdraw,
            ),
            other => self.detail_write_op(rpc, other),
        }
    }

    /// Fresh wallet SOL/ATA balances before wrap/swap/deposit (SOL pairs).
    #[cfg(feature = "write")]
    fn refresh_wallet_balances_for_detail(
        &mut self,
        rpc: &solana_client::rpc_client::RpcClient,
    ) -> Option<sol_clmm_manager::WalletPoolBalances> {
        let detail = self.detail.as_ref()?;
        let owner = self.wallet?;
        let traced = Rpc::new(rpc, &self.rpc_meter);
        let bal = sol_clmm_manager::fetch_wallet_pool_balances(
            &traced,
            &owner,
            detail.token_a.mint,
            detail.token_b.mint,
        )
        .ok()?;
        if let Some(d) = self.detail.as_mut() {
            d.wallet_balances = Some(bal.clone());
        }
        Some(bal)
    }

    #[cfg(feature = "write")]
    fn open_manage_deposit(&mut self, rpc: &solana_client::rpc_client::RpcClient) {
        if self.detail.is_none() {
            self.status_line = "no detail loaded".into();
            return;
        }
        // Re-fetch at start of mingling — native SOL vs WSOL may have changed.
        let balances = self.refresh_wallet_balances_for_detail(rpc).or_else(|| {
            self.detail
                .as_ref()
                .and_then(|d| d.wallet_balances.clone())
        });
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let wiz = crate::write_flow::ManageDepositWizard::from_detail(
            detail,
            self.swap_mode.clone(),
            balances,
        );
        self.status_line = wiz.status_hint();
        self.manage_deposit = Some(wiz);
        self.screen = Screen::ManageDeposit;
    }

    #[cfg(feature = "write")]
    pub fn open_wallet(&mut self, _rpc: &solana_client::rpc_client::RpcClient) {
        if !self.writes_live() {
            self.status_line = "wallet tools need write + keypair".into();
            return;
        }
        if self.wallet.is_none() {
            self.status_line = "no wallet".into();
            return;
        }
        if self.bg_rx.is_some() || self.pending != PendingWork::None {
            self.status_line = "busy · wait for current fetch".into();
            return;
        }
        self.wallet_wiz = None;
        self.wallet_loading = true;
        self.load_started = Some(Instant::now());
        self.pending = PendingWork::Wallet;
        self.screen = Screen::Wallet;
        self.status_line = "wallet · loading balances… · Esc cancel".into();
    }

    #[cfg(feature = "write")]
    pub fn spawn_wallet_fetch(&mut self) {
        if self.bg_rx.is_some() {
            return;
        }
        let Some(owner) = self.wallet else {
            self.wallet_loading = false;
            self.load_started = None;
            self.status_line = "no wallet".into();
            return;
        };
        let rpc_url = self.rpc_url.clone();
        let db_path = self.store.as_ref().map(|s| s.path().to_path_buf());
        let (tx, rx) = mpsc::channel();
        self.bg_rx = Some(rx);
        self.wallet_loading = true;
        self.load_started = Some(Instant::now());

        thread::spawn(move || {
            let sol = rpc::connect(&rpc_url);
            let meter = RpcMeter::default();
            let rpc = Rpc::new(&sol, &meter);
            let store = db_path.and_then(|p| Store::open(&p, false).ok());
            let result = sol_clmm_manager::fetch_wallet_token_balances(&rpc, &owner, store.as_ref());
            let _ = tx.send(BgFetchResult::Wallet {
                result: Box::new(result),
                meter,
            });
        });
    }

    #[cfg(feature = "write")]
    pub fn cancel_wallet(&mut self) {
        if self.wallet_loading || matches!(self.pending, PendingWork::Wallet) {
            self.pending = PendingWork::None;
            self.bg_rx = None;
            self.wallet_loading = false;
            self.load_started = None;
        }
        self.wallet_wiz = None;
        self.screen = Screen::Positions;
        self.status_line = self.positions_status_line();
    }

    #[cfg(feature = "write")]
    pub fn wallet_enter(&mut self, rpc: &solana_client::rpc_client::RpcClient) {
        let Some(wiz) = self.wallet_wiz.as_mut() else {
            return;
        };
        // Nested wrap amount confirm
        if wiz.wrap.is_some() {
            self.wallet_wrap_submit();
            return;
        }
        if let Some(swap) = wiz.swap.as_mut() {
            match swap.enter_advance() {
                Ok(Some(ready)) => {
                    let router = ready.router.clone();
                    self.swap_mode = router;
                    self.wallet_swap_submit(rpc, ready);
                }
                Ok(None) => {
                    self.status_line = swap.status_hint();
                }
                Err(e) => {
                    swap.error = Some(e.clone());
                    self.status_line = e;
                }
            }
            return;
        }
        if let Some(cp) = wiz.create_pool.as_mut() {
            match cp.enter_advance() {
                Ok(Some(params)) => self.wallet_create_pool_submit(rpc, params),
                Ok(None) => {
                    self.status_line = cp.status_hint();
                }
                Err(e) => {
                    cp.error = Some(e.clone());
                    self.status_line = e;
                }
            }
            return;
        }
        // Top menu
        match wiz.menu() {
            crate::wallet_flow::WalletMenu::Wrap => {
                let bal = sol_clmm_manager::WalletPoolBalances {
                    a: sol_clmm_manager::SideBalance {
                        mint: sol_clmm_manager::wsol_mint(),
                        ata_amount: wiz
                            .tokens
                            .iter()
                            .find(|t| t.is_wsol)
                            .map(|t| t.amount)
                            .unwrap_or(0),
                        is_wsol: true,
                    },
                    b: sol_clmm_manager::SideBalance {
                        mint: solana_pubkey::Pubkey::default(),
                        ata_amount: 0,
                        is_wsol: false,
                    },
                    sol_lamports: wiz.sol_lamports,
                    rent_reserve_lamports: wiz.rent_reserve,
                };
                wiz.wrap = Some(crate::write_flow::ManageWrapWizard::new(Some(&bal)));
                self.status_line = "wrap SOL · type amount · Enter · Esc".into();
            }
            crate::wallet_flow::WalletMenu::Unwrap => {
                self.wallet_unwrap_submit();
            }
            crate::wallet_flow::WalletMenu::Swap => {
                let router = self.swap_mode.clone();
                let sol = wiz.sol_lamports;
                let rent = wiz.rent_reserve;
                let tokens = wiz.tokens.clone();
                wiz.swap = Some(crate::wallet_flow::SwapWizard::new(
                    sol, &tokens, rent, router,
                ));
                self.status_line = "swap · pick FROM".into();
            }
            crate::wallet_flow::WalletMenu::CreatePool => {
                let sol = wiz.sol_lamports;
                let rent = wiz.rent_reserve;
                let tokens = wiz.tokens.clone();
                wiz.create_pool = Some(crate::wallet_flow::CreatePoolWizard::new(
                    sol, &tokens, rent,
                ));
                self.status_line = "create pool · venue".into();
            }
        }
    }

    #[cfg(feature = "write")]
    fn wallet_wrap_submit(&mut self) {
        let Some(wiz) = self.wallet_wiz.as_ref() else {
            return;
        };
        let Some(wrap) = wiz.wrap.as_ref() else {
            return;
        };
        let lamports = match wrap.lamports() {
            Ok(v) => v,
            Err(e) => {
                if let Some(w) = self.wallet_wiz.as_mut().and_then(|w| w.wrap.as_mut()) {
                    w.error = Some(e.clone());
                }
                self.status_line = e;
                return;
            }
        };
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "wrap needs keypair".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let plan = sol_clmm_manager::plan_wrap_sol(payer.pubkey(), lamports);
        let tip = self.tip_lamports;
        let confirm =
            crate::write_flow::WriteConfirm::with_tip(plan, Vec::new(), tip, None);
        self.confirm_scroll = 0;
        self.write_confirm = Some(confirm);
        self.wallet_wiz = None;
        self.screen = Screen::WriteConfirm;
        self.status_line = "wrap · ↑↓ tip · Enter simulate".into();
    }

    #[cfg(feature = "write")]
    fn wallet_unwrap_submit(&mut self) {
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "unwrap needs keypair".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let plan = sol_clmm_manager::plan_unwrap_sol(payer.pubkey());
        let tip = self.tip_lamports;
        let confirm =
            crate::write_flow::WriteConfirm::with_tip(plan, Vec::new(), tip, None);
        self.confirm_scroll = 0;
        self.write_confirm = Some(confirm);
        self.wallet_wiz = None;
        self.screen = Screen::WriteConfirm;
        self.status_line = "unwrap · ↑↓ tip · Enter simulate".into();
    }

    #[cfg(feature = "write")]
    fn wallet_swap_submit(
        &mut self,
        _rpc: &solana_client::rpc_client::RpcClient,
        ready: crate::wallet_flow::SwapReady,
    ) {
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "swap needs keypair".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let tip = self.tip_lamports;
        let mut settings = self.write_settings_for_confirm(tip);
        settings.swap_mode = Some(ready.router.clone());
        self.swap_mode = ready.router.clone();
        let client = Client::new(self.price_mode, self.price_keys());
        match client.assemble_wallet_swap(
            &settings,
            payer.pubkey(),
            ready.input_mint,
            ready.output_mint,
            ready.amount,
            ready.slippage_bps,
            ready.input_is_native_sol,
        ) {
            Ok((plan, tip_opt)) => {
                let tip = tip_opt.unwrap_or(tip);
                let mut confirm =
                    crate::write_flow::WriteConfirm::with_tip(plan, Vec::new(), tip, tip_opt);
                // History Δ uses these mints (from → to). SOL/WSOL combines native+ATA.
                confirm.attach_pool_context(
                    None,
                    None,
                    &ready.input_symbol,
                    &ready.output_symbol,
                    &ready.input_mint,
                    &ready.output_mint,
                    ready.input_decimals,
                    ready.output_decimals,
                );
                self.confirm_scroll = 0;
                self.write_confirm = Some(confirm);
                self.wallet_wiz = None;
                self.screen = Screen::WriteConfirm;
                self.status_line = "swap · ↑↓ tip · Enter simulate".into();
            }
            Err(e) => {
                if let Some(w) = self.wallet_wiz.as_mut().and_then(|w| w.swap.as_mut()) {
                    w.error = Some(e.to_string());
                }
                self.status_line = format!("swap: {e}");
            }
        }
    }

    #[cfg(feature = "write")]
    fn wallet_create_pool_submit(
        &mut self,
        rpc: &solana_client::rpc_client::RpcClient,
        params: sol_clmm_manager::CreatePoolParams,
    ) {
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "create pool needs keypair".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let tip = self.tip_lamports;
        let settings = self.write_settings_for_confirm(tip);
        let client = Client::new(self.price_mode, self.price_keys());
        match client.assemble_create_pool(rpc, payer.pubkey(), &params, &settings) {
            Ok((plan, tip_opt, extra)) => {
                let tip = tip_opt.unwrap_or(tip);
                let confirm =
                    crate::write_flow::WriteConfirm::with_tip(plan, extra, tip, tip_opt);
                self.confirm_scroll = 0;
                self.write_confirm = Some(confirm);
                self.wallet_wiz = None;
                self.screen = Screen::WriteConfirm;
                self.status_line = "create pool · ↑↓ tip · Enter simulate".into();
            }
            Err(e) => {
                if let Some(w) = self
                    .wallet_wiz
                    .as_mut()
                    .and_then(|w| w.create_pool.as_mut())
                {
                    w.error = Some(e.to_string());
                }
                self.status_line = format!("create pool: {e}");
            }
        }
    }

    #[cfg(feature = "write")]
    fn open_manage_compound(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            self.status_line = "no detail loaded".into();
            return;
        };
        if detail.fee_a == 0 && detail.fee_b == 0 {
            self.status_line = "no claimable fees to compound".into();
            return;
        }
        let wiz = crate::write_flow::ManageCompoundWizard::from_detail(
            detail,
            self.swap_mode.clone(),
        );
        self.status_line = wiz.status_hint();
        self.manage_compound = Some(wiz);
        self.screen = Screen::ManageCompound;
    }

    #[cfg(feature = "write")]
    fn open_manage_withdraw(&mut self, kind: crate::write_flow::WithdrawKind) {
        let Some(detail) = self.detail.as_ref() else {
            self.status_line = "no detail loaded".into();
            return;
        };
        if detail.position_liquidity == 0
            && matches!(kind, crate::write_flow::WithdrawKind::Withdraw)
        {
            self.status_line = "no liquidity to withdraw".into();
            return;
        }
        if detail.position_liquidity == 0
            && matches!(kind, crate::write_flow::WithdrawKind::ClaimWithdraw)
            && detail.fee_a == 0
            && detail.fee_b == 0
        {
            self.status_line = "nothing to claim or withdraw".into();
            return;
        }
        let wiz = crate::write_flow::ManageWithdrawWizard::from_detail(detail, kind);
        self.status_line = wiz.status_hint();
        self.manage_withdraw = Some(wiz);
        self.screen = Screen::ManageWithdraw;
    }

    #[cfg(feature = "write")]
    pub fn manage_withdraw_back(&mut self) {
        self.manage_withdraw = None;
        self.screen = Screen::Manage;
        self.status_line = "manage · ↑↓ · Enter · Esc · p pubkey".into();
    }

    #[cfg(feature = "write")]
    pub fn manage_withdraw_submit(&mut self) {
        let Some(detail) = self.detail.clone() else {
            self.status_line = "no detail loaded".into();
            return;
        };
        let Some(wiz) = self.manage_withdraw.as_ref() else {
            return;
        };
        let kind = wiz.kind;
        let liq = match wiz.liquidity_delta() {
            Ok(v) => v,
            Err(e) => {
                if let Some(w) = self.manage_withdraw.as_mut() {
                    w.error = Some(e);
                }
                return;
            }
        };
        if matches!(kind, crate::write_flow::WithdrawKind::Withdraw) && liq == 0 {
            if let Some(w) = self.manage_withdraw.as_mut() {
                w.error = Some("liquidity to withdraw is 0".into());
            }
            return;
        }
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "no keypair path".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let tip = self.tip_lamports;
        let client = Client::new(self.price_mode, self.price_keys());
        let sol_leg = if sol_clmm_manager::pool_has_wsol(
            &detail.token_a.mint,
            &detail.token_b.mint,
        ) {
            sol_clmm_manager::SolLegMode::NativeSol
        } else {
            sol_clmm_manager::SolLegMode::Wsol
        };
        let plan = match kind {
            crate::write_flow::WithdrawKind::Withdraw => {
                client.plan_withdraw(payer.pubkey(), &detail, liq, sol_leg)
            }
            crate::write_flow::WithdrawKind::ClaimWithdraw => {
                client.plan_claim_and_withdraw(payer.pubkey(), &detail, liq, sol_leg)
            }
        };
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                if let Some(w) = self.manage_withdraw.as_mut() {
                    w.error = Some(e.to_string());
                }
                self.status_line = format!("{} plan failed · {e}", kind.title());
                return;
            }
        };
        if plan.instructions.is_empty() {
            self.status_line = format!("{}: no instructions", kind.title());
            return;
        }
        let mut confirm =
            crate::write_flow::WriteConfirm::with_tip(plan, Vec::new(), tip, None);
        Self::attach_confirm_from_detail(&mut confirm, &detail);
        self.manage_withdraw = None;
        self.confirm_scroll = 0;
        self.write_confirm = Some(confirm);
        self.screen = Screen::WriteConfirm;
        self.status_line = format!(
            "{} · ↑↓ priority fee · Enter simulate",
            kind.title()
        );
    }

    #[cfg(feature = "write")]
    pub fn manage_compound_back(&mut self) {
        let leave = self
            .manage_compound
            .as_mut()
            .map(|w| w.back())
            .unwrap_or(true);
        if leave {
            self.manage_compound = None;
            self.screen = Screen::Manage;
            self.status_line = "manage · ↑↓ · Enter · Esc · p pubkey".into();
        } else if let Some(w) = &self.manage_compound {
            self.status_line = w.status_hint();
        }
    }

    #[cfg(feature = "write")]
    pub fn manage_compound_advance(&mut self) {
        let ready = {
            let Some(w) = self.manage_compound.as_mut() else {
                return;
            };
            matches!(
                w.advance(),
                crate::write_flow::ManageCompoundAdvance::Ready
            )
        };
        if let Some(w) = &self.manage_compound {
            self.status_line = w.status_hint();
        }
        if ready {
            self.submit_manage_compound();
        }
    }

    #[cfg(feature = "write")]
    fn submit_manage_compound(&mut self) {
        let Some(detail) = self.detail.clone() else {
            self.status_line = "no detail loaded".into();
            return;
        };
        let Some(wiz) = self.manage_compound.as_ref() else {
            return;
        };
        let with_swap = wiz.with_swap();
        let slip = wiz.swap_slippage_bps();
        let router = wiz.swap_router.clone();

        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "no keypair path".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let tip = self.tip_lamports;
        let mut settings = self.write_settings_for_confirm(tip);
        settings.swap_mode = Some(router.clone());
        self.swap_mode = router;
        let client = Client::new(self.price_mode, self.price_keys());
        let result =
            client.assemble_compound(payer.pubkey(), &detail, with_swap, slip, &settings);
        let (plan, router_tip) = match result {
            Ok(v) => v,
            Err(e) => {
                if let Some(w) = self.manage_compound.as_mut() {
                    w.error = Some(e.to_string());
                }
                self.status_line = format!("compound plan failed · {e}");
                return;
            }
        };
        if plan.instructions.is_empty() {
            self.status_line = "compound: no instructions".into();
            return;
        }
        let mut confirm =
            crate::write_flow::WriteConfirm::with_tip(plan, Vec::new(), tip, router_tip);
        Self::attach_confirm_from_detail(&mut confirm, &detail);
        self.manage_compound = None;
        self.confirm_scroll = 0;
        self.write_confirm = Some(confirm);
        self.screen = Screen::WriteConfirm;
        self.status_line = "compound · ↑↓ priority fee · Enter simulate".into();
    }

    #[cfg(feature = "write")]
    pub fn manage_deposit_back(&mut self) {
        let leave = self
            .manage_deposit
            .as_mut()
            .map(|w| w.back())
            .unwrap_or(true);
        if leave {
            self.manage_deposit = None;
            self.screen = Screen::Manage;
            self.status_line = "manage · ↑↓ · Enter · Esc · p pubkey".into();
        } else if let Some(w) = &self.manage_deposit {
            self.status_line = w.status_hint();
        }
    }

    #[cfg(feature = "write")]
    pub fn manage_deposit_advance(&mut self, rpc: &solana_client::rpc_client::RpcClient) {
        let ready = {
            let Some(w) = self.manage_deposit.as_mut() else {
                return;
            };
            matches!(
                w.advance(),
                crate::write_flow::ManageDepositAdvance::Ready
            )
        };
        if let Some(w) = &self.manage_deposit {
            self.status_line = w.status_hint();
        }
        if ready {
            self.submit_manage_deposit(rpc);
        }
    }

    #[cfg(feature = "write")]
    fn submit_manage_deposit(&mut self, _rpc: &solana_client::rpc_client::RpcClient) {
        let Some(detail) = self.detail.clone() else {
            self.status_line = "no detail loaded".into();
            return;
        };
        let Some(wiz) = self.manage_deposit.as_ref() else {
            return;
        };
        let (amount_a, amount_b) = match wiz.amounts_raw() {
            Ok(v) => v,
            Err(e) => {
                if let Some(w) = self.manage_deposit.as_mut() {
                    w.error = Some(e);
                }
                return;
            }
        };
        let shape = wiz.shape();
        let swap_to_match = wiz.swap_to_match();
        let slip = wiz.swap_slippage_bps();
        let sol_leg = wiz.sol_leg();
        let router = wiz.swap_router.clone();

        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "no keypair path".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let tip = self.tip_lamports;
        let mut settings = self.write_settings_for_confirm(tip);
        settings.swap_mode = Some(router.clone());
        self.swap_mode = router;
        let client = Client::new(self.price_mode, self.price_keys());
        let strategy = Some(Client::dlmm_strategy_from_settings(&settings));
        let result = client.assemble_manage_deposit(
            payer.pubkey(),
            &detail,
            amount_a,
            amount_b,
            shape,
            swap_to_match,
            slip,
            sol_leg,
            &settings,
            strategy,
        );
        let (plan, router_tip) = match result {
            Ok(v) => v,
            Err(e) => {
                if let Some(w) = self.manage_deposit.as_mut() {
                    w.error = Some(e.to_string());
                }
                self.status_line = format!("deposit plan failed · {e}");
                return;
            }
        };
        if plan.instructions.is_empty() {
            self.status_line = "deposit: no instructions".into();
            return;
        }
        let mut confirm =
            crate::write_flow::WriteConfirm::with_tip(plan, Vec::new(), tip, router_tip);
        Self::attach_confirm_from_detail(&mut confirm, &detail);
        self.manage_deposit = None;
        self.confirm_scroll = 0;
        self.write_confirm = Some(confirm);
        self.screen = Screen::WriteConfirm;
        self.status_line = "deposit · ↑↓ priority fee · Enter simulate".into();
    }

    #[cfg(feature = "write")]
    fn detail_write_op(
        &mut self,
        rpc: &solana_client::rpc_client::RpcClient,
        kind: ManageChoice,
    ) {
        let Some(detail) = self.detail.clone() else {
            self.status_line = "no detail loaded".into();
            return;
        };
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "no keypair path".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let _ = rpc; // plan only — simulate after tip pick
        let authority = payer.pubkey();
        let tip = self.tip_lamports;
        let client = Client::new(self.price_mode, self.price_keys());
        let sol_leg = if sol_clmm_manager::pool_has_wsol(
            &detail.token_a.mint,
            &detail.token_b.mint,
        ) {
            sol_clmm_manager::SolLegMode::NativeSol
        } else {
            sol_clmm_manager::SolLegMode::Wsol
        };
        let plan = match kind {
            ManageChoice::Deposit
            | ManageChoice::Compound
            | ManageChoice::Withdraw
            | ManageChoice::ClaimWithdraw => {
                unreachable!("opens wizard");
            }
            ManageChoice::Claim => client.plan_claim(authority, &detail),
            ManageChoice::Close => client.plan_close_and_claim(authority, &detail, sol_leg),
        };
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                self.status_line = format!("{}: {e}", kind.title());
                return;
            }
        };
        if plan.instructions.is_empty() {
            self.status_line = format!("{}: no instructions assembled", kind.title());
            return;
        }
        let mut confirm =
            crate::write_flow::WriteConfirm::with_tip(plan, Vec::new(), tip, None);
        Self::attach_confirm_from_detail(&mut confirm, &detail);
        self.confirm_scroll = 0;
        self.write_confirm = Some(confirm);
        self.screen = Screen::WriteConfirm;
        self.status_line = format!(
            "{} · ↑↓ priority fee · Enter simulate",
            kind.title()
        );
    }

    #[cfg(feature = "write")]
    fn attach_confirm_from_detail(
        confirm: &mut crate::write_flow::WriteConfirm,
        detail: &PositionDetail,
    ) {
        confirm.attach_pool_context(
            Some(&detail.row.address.to_string()),
            Some(&detail.pool.address.to_string()),
            &detail.token_a.symbol,
            &detail.token_b.symbol,
            &detail.token_a.mint,
            &detail.token_b.mint,
            detail.token_a.decimals,
            detail.token_b.decimals,
        );
    }

    #[cfg(feature = "write")]
    pub fn create_submit_review(&mut self, rpc: &solana_client::rpc_client::RpcClient) {
        let Some(wiz) = self.create.as_ref() else {
            return;
        };
        let params = match wiz.build_params() {
            Ok(p) => p,
            Err(e) => {
                if let Some(w) = self.create.as_mut() {
                    w.error = Some(e);
                }
                return;
            }
        };
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                self.status_line = "no keypair path".into();
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                self.status_line = format!("keypair: {e}");
                return;
            }
        };
        let mint = crate::write_flow::new_position_signer();
        let tip = self.tip_lamports;
        let mut settings = self.write_settings_for_confirm(tip);
        if let Some(w) = self.create.as_ref() {
            settings.swap_mode = Some(w.swap_router.clone());
            // Keep app settings in sync when user Tab-cycled router on create.
            self.swap_mode = w.swap_router.clone();
        }
        let client = Client::new(self.price_mode, self.price_keys());
        match client.assemble_create_position(
            rpc,
            payer.pubkey(),
            &params,
            mint.pubkey(),
            &settings,
        ) {
            Ok((plan, router_tip)) => {
                let mut confirm = crate::write_flow::WriteConfirm::with_tip(
                    plan,
                    vec![mint],
                    tip,
                    router_tip,
                );
                if let Some(w) = self.create.as_ref()
                    && let Some(p) = w.preview.as_ref()
                {
                    confirm.attach_pool_context(
                        None,
                        Some(&p.pool.address.to_string()),
                        &p.token_a.symbol,
                        &p.token_b.symbol,
                        &p.token_a.mint,
                        &p.token_b.mint,
                        p.token_a.decimals,
                        p.token_b.decimals,
                    );
                }
                let _ = (rpc, payer, settings); // tip first · Enter simulates
                self.create = None;
                self.confirm_scroll = 0;
                self.write_confirm = Some(confirm);
                self.screen = Screen::WriteConfirm;
                self.status_line = format!(
                    "create · ↑↓ priority fee · Enter simulate · tip {}",
                    tip_label(tip)
                );
            }
            Err(e) => {
                if let Some(w) = self.create.as_mut() {
                    w.error = Some(e.to_string());
                }
                self.status_line = format!("assemble: {e}");
            }
        }
    }

    #[cfg(feature = "write")]
    pub fn write_confirm_send(&mut self, _rpc: &solana_client::rpc_client::RpcClient) {
        if self.write_confirm.is_none() || self.bg_rx.is_some() {
            return;
        }
        if !self
            .write_confirm
            .as_ref()
            .is_some_and(|c| c.phase == crate::write_flow::ConfirmPhase::Review)
        {
            return;
        }
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                if let Some(c) = self.write_confirm.as_mut() {
                    c.phase = crate::write_flow::ConfirmPhase::Failed;
                    c.last_error = Some("no keypair path".into());
                }
                return;
            }
        };
        // Validate keypair early so Failed shows without spinning the bar.
        if let Err(e) = read_keypair_file(&path) {
            if let Some(c) = self.write_confirm.as_mut() {
                c.phase = crate::write_flow::ConfirmPhase::Failed;
                c.last_error = Some(format!("keypair: {e}"));
            }
            return;
        }
        let tip = self.write_confirm.as_ref().expect("confirm").tip_lamports;
        let router_tip = self.write_confirm.as_ref().expect("confirm").router_tip;
        let settings = self.write_settings_for_confirm(tip);
        let price_mode = self.price_mode;
        let keys = self.price_keys();
        let rpc_url = self.rpc_url.clone();
        let plan = self.write_confirm.as_ref().expect("confirm").plan.clone();
        let extra_bytes: Vec<[u8; 64]> = self
            .write_confirm
            .as_ref()
            .expect("confirm")
            .extra_signers
            .iter()
            .map(|k| k.to_bytes())
            .collect();
        // Mints + labels for before/after wallet balance Δ
        // (detail → create preview → confirm context, e.g. wallet swap from/to).
        // Owner = signer pubkey (not `self.wallet`, which can be unset).
        let bal_owner = read_keypair_file(&path)
            .ok()
            .map(|k| k.pubkey())
            .or(self.wallet);
        let (mint_a, mint_b, sym_a, sym_b, dec_a, dec_b) = self
            .detail
            .as_ref()
            .map(|d| {
                (
                    Some(d.token_a.mint),
                    Some(d.token_b.mint),
                    Some(d.token_a.symbol.clone()),
                    Some(d.token_b.symbol.clone()),
                    Some(d.token_a.decimals),
                    Some(d.token_b.decimals),
                )
            })
            .or_else(|| {
                self.create.as_ref().and_then(|w| {
                    w.preview.as_ref().map(|p| {
                        (
                            Some(p.token_a.mint),
                            Some(p.token_b.mint),
                            Some(p.token_a.symbol.clone()),
                            Some(p.token_b.symbol.clone()),
                            Some(p.token_a.decimals),
                            Some(p.token_b.decimals),
                        )
                    })
                })
            })
            .or_else(|| {
                let c = self.write_confirm.as_ref()?;
                let ma = c.mint_a.as_ref()?.parse::<Pubkey>().ok()?;
                let mb = c.mint_b.as_ref()?.parse::<Pubkey>().ok()?;
                Some((
                    Some(ma),
                    Some(mb),
                    c.symbol_a.clone(),
                    c.symbol_b.clone(),
                    c.decimals_a,
                    c.decimals_b,
                ))
            })
            .unwrap_or((None, None, None, None, None, None));

        if let Some(c) = self.write_confirm.as_mut() {
            c.phase = crate::write_flow::ConfirmPhase::Busy;
            c.last_error = None;
            c.paid_label = None;
            c.signature = None;
            c.wallet_balances = None;
        }
        self.load_started = Some(Instant::now());
        self.status_line = format!("sending · tip {} · waiting…", tip_label(tip));

        let (tx, rx) = mpsc::channel();
        self.bg_rx = Some(rx);
        thread::spawn(move || {
            let fetch_bal =
                |owner: Pubkey, ma: Pubkey, mb: Pubkey| -> Option<sol_clmm_manager::WalletPoolBalances> {
                    let sol = rpc::connect(&rpc_url);
                    let meter = RpcMeter::default();
                    let traced = Rpc::new(&sol, &meter);
                    sol_clmm_manager::fetch_wallet_pool_balances(&traced, &owner, ma, mb).ok()
                };
            let bal_before = match (bal_owner, mint_a, mint_b) {
                (Some(owner), Some(ma), Some(mb)) => fetch_bal(owner, ma, mb),
                _ => None,
            };
            let result = (|| -> anyhow::Result<sol_clmm_manager::SendOutcome> {
                let rpc = solana_client::rpc_client::RpcClient::new(rpc_url.clone());
                let payer = read_keypair_file(&path)
                    .map_err(|e| anyhow::anyhow!("keypair: {e}"))?;
                let extras: Vec<solana_keypair::Keypair> = extra_bytes
                    .iter()
                    .map(|b| {
                        solana_keypair::Keypair::try_from(&b[..])
                            .map_err(|e| anyhow::anyhow!("extra signer: {e}"))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                let extra_refs: Vec<&solana_keypair::Keypair> = extras.iter().collect();
                let client = Client::new(price_mode, keys);
                client.execute_plan_signers(
                    &rpc,
                    &payer,
                    &extra_refs,
                    &plan,
                    &settings,
                    router_tip,
                )
            })();
            // Every successful tx changes SOL / ATA — refresh immediately.
            let balances = if result.is_ok() {
                match (bal_owner, mint_a, mint_b) {
                    (Some(owner), Some(ma), Some(mb)) => fetch_bal(owner, ma, mb),
                    _ => None,
                }
            } else {
                None
            };
            let token_delta = match (
                &bal_before,
                &balances,
                mint_a,
                mint_b,
                sym_a,
                sym_b,
                dec_a,
                dec_b,
            ) {
                (
                    Some(before),
                    Some(after),
                    Some(ma),
                    Some(mb),
                    Some(sa),
                    Some(sb),
                    Some(da),
                    Some(db),
                ) => {
                    // WSOL leg: native + ATA so wrap/unwrap/compound shows real SOL Δ.
                    let (sym_a, dec_a, delta_a) = if before.a.is_wsol {
                        let b = before.sol_lamports as i128 + before.a.ata_amount as i128;
                        let a = after.sol_lamports as i128 + after.a.ata_amount as i128;
                        ("SOL".into(), 9u8, (a - b) as i64)
                    } else {
                        (
                            sa,
                            da,
                            after.a.ata_amount as i64 - before.a.ata_amount as i64,
                        )
                    };
                    let (sym_b, dec_b, delta_b) = if before.b.is_wsol {
                        let b = before.sol_lamports as i128 + before.b.ata_amount as i128;
                        let a = after.sol_lamports as i128 + after.b.ata_amount as i128;
                        ("SOL".into(), 9u8, (a - b) as i64)
                    } else {
                        (
                            sb,
                            db,
                            after.b.ata_amount as i64 - before.b.ata_amount as i64,
                        )
                    };
                    Some(sol_clmm_manager::WriteTokenDelta {
                        mint_a: ma.to_string(),
                        mint_b: mb.to_string(),
                        symbol_a: sym_a,
                        symbol_b: sym_b,
                        decimals_a: dec_a,
                        decimals_b: dec_b,
                        delta_a,
                        delta_b,
                    })
                }
                _ => None,
            };
            let _ = tx.send(BgFetchResult::Write {
                result: Box::new(result),
                balances,
                token_delta,
            });
        });
    }

    /// `[` / `]` · cycle tip presets (Tip phase only — no sim until Enter).
    #[cfg(feature = "write")]
    pub fn write_confirm_nudge_tip(&mut self, up: bool) {
        let Some(c) = self.write_confirm.as_mut() else {
            return;
        };
        if c.phase != crate::write_flow::ConfirmPhase::Tip {
            return;
        }
        c.nudge_tip(up);
        let tip = c.tip_lamports;
        self.tip_lamports = tip;
        if let Some(store) = &self.store {
            let _ = store.set_tx_tip_lamports(tip);
        }
        self.status_line = format!(
            "priority fee → {} · Enter simulate · Esc cancel",
            tip_label(tip)
        );
    }

    /// Tip → Enter: run simulation · then Review to send.
    #[cfg(feature = "write")]
    pub fn write_confirm_simulate(&mut self, rpc: &solana_client::rpc_client::RpcClient) {
        let (tip, router_tip, plan) = {
            let Some(c) = self.write_confirm.as_mut() else {
                return;
            };
            if c.phase != crate::write_flow::ConfirmPhase::Tip {
                return;
            }
            c.apply_tip_choice();
            (c.tip_lamports, c.router_tip, c.plan.clone())
        };
        self.tip_lamports = tip;
        if let Some(store) = &self.store {
            let _ = store.set_tx_tip_lamports(tip);
        }
        let path = match self.keypair_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                if let Some(c) = self.write_confirm.as_mut() {
                    c.set_error("no keypair path".into());
                    c.phase = crate::write_flow::ConfirmPhase::Failed;
                }
                return;
            }
        };
        let payer = match read_keypair_file(&path) {
            Ok(k) => k,
            Err(e) => {
                if let Some(c) = self.write_confirm.as_mut() {
                    c.set_error(format!("keypair: {e}"));
                    c.phase = crate::write_flow::ConfirmPhase::Failed;
                }
                return;
            }
        };
        let settings = self.write_settings_for_confirm(tip);
        let client = Client::new(self.price_mode, self.price_keys());
        let extras: Vec<&solana_keypair::Keypair> = self
            .write_confirm
            .as_ref()
            .map(|c| c.extra_signers.iter().collect())
            .unwrap_or_default();
        self.status_line = "simulating…".into();
        match client.prepare_and_simulate_signers(
            rpc,
            &payer,
            &extras,
            &plan,
            &settings,
            router_tip,
        ) {
            Ok((prepared, logs)) => {
                if let Some(c) = self.write_confirm.as_mut() {
                    c.budget_label = Some(prepared.budget.label());
                    c.sim_preview = logs;
                    c.phase = crate::write_flow::ConfirmPhase::Review;
                    c.last_error = None;
                    c.error_lines.clear();
                }
                self.confirm_scroll = 0;
                self.status_line = format!(
                    "sim OK · tip {} · Enter send · ↑↓ or [ ] back to tip",
                    tip_label(tip)
                );
            }
            Err(e) => {
                if let Some(c) = self.write_confirm.as_mut() {
                    c.set_error(e.to_string());
                    c.phase = crate::write_flow::ConfirmPhase::Failed;
                    c.choice = 0;
                }
                self.confirm_scroll = 0;
                self.status_line =
                    "sim failed · ↑↓ scroll logs · ←→ Change tip / Cancel".into();
            }
        }
    }

    /// From Review, `[` goes back to tip pick (clear sim).
    #[cfg(feature = "write")]
    pub fn write_confirm_back_to_tip(&mut self) {
        let Some(c) = self.write_confirm.as_mut() else {
            return;
        };
        if c.phase != crate::write_flow::ConfirmPhase::Review {
            return;
        }
        c.phase = crate::write_flow::ConfirmPhase::Tip;
        c.sim_preview.clear();
        c.budget_label = None;
        c.last_error = None;
        c.error_lines.clear();
        self.confirm_scroll = 0;
        c.choice = TIP_PRESETS
            .iter()
            .position(|&p| p == c.tip_lamports)
            .unwrap_or(3);
        self.status_line = format!(
            "↑↓ priority fee · Enter simulate · tip {}",
            tip_label(c.tip_lamports)
        );
    }

    #[cfg(feature = "write")]
    pub fn confirm_scroll_by(&mut self, delta: i32) {
        if delta < 0 {
            self.confirm_scroll = self.confirm_scroll.saturating_sub((-delta) as u16);
        } else {
            self.confirm_scroll = self.confirm_scroll.saturating_add(delta as u16);
        }
    }

    #[cfg(feature = "write")]
    pub fn toggle_confirm_links(&mut self) {
        if self.links_overlay {
            self.restore_links_overlay();
            return;
        }
        let Some(c) = &self.write_confirm else {
            return;
        };
        let mut parts = Vec::new();
        if let Some(pos) = &c.position_pk {
            parts.push(format!("position {pos}"));
        }
        if let Some(pool) = &c.pool_pk {
            parts.push(format!("pool {pool}"));
        }
        if let Some(sig) = &c.signature {
            parts.push(format!("tx {sig}"));
        }
        if parts.is_empty() {
            self.status_line = "nothing to copy yet".into();
            return;
        }
        self.status_before_links = Some(self.status_line.clone());
        self.links_overlay = true;
        self.status_line = format!("{} · p hide", parts.join(" · "));
    }

    /// Failed → back to tip picker (do **not** auto-simulate — user must pick fee + Enter).
    #[cfg(feature = "write")]
    pub fn write_confirm_retry(&mut self, _rpc: &solana_client::rpc_client::RpcClient) {
        let Some(c) = self.write_confirm.as_mut() else {
            return;
        };
        c.phase = crate::write_flow::ConfirmPhase::Tip;
        c.last_error = None;
        c.error_lines.clear();
        c.sim_preview.clear();
        c.budget_label = None;
        // Keep tip highlight on current preset.
        c.choice = TIP_PRESETS
            .iter()
            .position(|&p| p == c.tip_lamports)
            .unwrap_or(3);
        self.confirm_scroll = 0;
        self.status_line = format!(
            "↑↓ priority fee · Enter simulate · tip {}",
            tip_label(c.tip_lamports)
        );
    }

    #[cfg(feature = "write")]
    pub fn write_confirm_close(&mut self) {
        self.dismiss_links_overlay();
        let op = self.write_confirm.as_ref().map(|c| c.plan.op);
        let position_pk = self
            .write_confirm
            .as_ref()
            .and_then(|c| c.position_pk.clone());
        let back_manage = self.detail.is_some() && position_pk.is_some();
        let was_done = self
            .write_confirm
            .as_ref()
            .is_some_and(|c| c.phase == crate::write_flow::ConfirmPhase::Done);
        let closed = was_done
            && matches!(
                op,
                Some(sol_clmm_manager::WriteOp::CloseAndClaim)
            );
        let created = was_done
            && matches!(
                op,
                Some(sol_clmm_manager::WriteOp::CreatePosition)
            );
        self.write_confirm = None;
        self.confirm_scroll = 0;

        // Close removes the account — never re-fetch detail. Drop stale row + refresh list.
        if closed {
            if let Some(pk) = position_pk.as_deref()
                && let Ok(addr) = pk.parse::<Pubkey>()
            {
                self.positions.retain(|p| p.address != addr);
                self.sync_table_selection_after_filter();
            }
            self.detail = None;
            self.detail_error = None;
            self.detail_loading = false;
            self.screen = Screen::Positions;
            self.request_list_refresh();
            self.status_line = "closed · refreshing list…".into();
            return;
        }

        // Create / other writes from list → refresh list.
        if created || !back_manage {
            self.screen = Screen::Positions;
            self.status_line = self.positions_status_line();
            if was_done {
                self.request_list_refresh();
                self.status_line = "landed · refreshing list…".into();
            }
            return;
        }

        // Claim / withdraw (position still exists) → manage + refresh detail.
        self.screen = Screen::Manage;
        self.status_line = "manage · ↑↓ · Enter · Esc · p full pubkey".into();
        if was_done {
            self.request_detail_refresh();
        }
    }
}

/// UI-safe RPC string. `privacy`: only `https://` + first 4 host chars + `…`.
pub fn format_rpc_url(url: &str, privacy: bool) -> String {
    let raw = url.trim();
    let (scheme, rest) = if let Some(r) = raw.strip_prefix("https://") {
        ("https://", r)
    } else if let Some(r) = raw.strip_prefix("http://") {
        ("http://", r)
    } else {
        ("", raw)
    };

    // Drop path/query — private RPC keys often live there.
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);

    if privacy {
        let head: String = host.chars().take(4).collect();
        if head.is_empty() {
            format!("{scheme}…")
        } else {
            format!("{scheme}{head}…")
        }
    } else if host.len() > 42 {
        format!("{scheme}{}…", &host[..42])
    } else {
        format!("{scheme}{host}")
    }
}

#[cfg(test)]
mod privacy_tests {
    use super::format_rpc_url;

    #[test]
    fn privacy_masks_host_and_drops_query() {
        let u = "https://mainnet.helius-rpc.com/?api-key=SECRET";
        assert_eq!(format_rpc_url(u, true), "https://main…");
        let open = format_rpc_url(u, false);
        assert!(open.starts_with("https://mainnet.helius-rpc.com"));
        assert!(!open.contains("SECRET"));
    }
}
