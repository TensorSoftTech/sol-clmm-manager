use sol_clmm_client::Client;
use sol_clmm_manager::rpc::{self, Rpc, RpcMeter, RpcStatus};
use sol_clmm_manager::store::{
    self, default_db_path, display_path, expand_tilde, AppSettings, Store,
};
use sol_clmm_manager::{
    PositionDetail, PositionRow, PriceApiKeys, PriceMode, VenueFilter,
};
use anyhow::Result;
use ratatui::widgets::TableState;
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
    Positions,
    Settings,
    SettingsRpc,
    SettingsApiKey,
    Detail,
    Help,
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
            Self::UseExisting => "use existing database",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItem {
    PrivacyTrim,
    PriceMode,
    JupiterKey,
    TitanKey,
    ApiPaidFast,
    ChangeRpc,
    ChangeWallet,
    Back,
}

impl SettingsItem {
    pub const ALL: [SettingsItem; 8] = [
        SettingsItem::PrivacyTrim,
        SettingsItem::PriceMode,
        SettingsItem::JupiterKey,
        SettingsItem::TitanKey,
        SettingsItem::ApiPaidFast,
        SettingsItem::ChangeRpc,
        SettingsItem::ChangeWallet,
        SettingsItem::Back,
    ];

    pub fn label(
        self,
        privacy_trim: bool,
        price_mode: PriceMode,
        jupiter_key: bool,
        titan_key: bool,
        api_paid_fast: bool,
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
            Self::PriceMode => format!(
                "USD estimate · {}  [Enter cycles]",
                price_mode.short()
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
    List { refresh_meta: bool },
    Detail { refresh_meta: bool },
}

/// Result of a background list/detail fetch (keeps UI animating).
pub enum BgFetchResult {
    List {
        status: Result<RpcStatus>,
        positions: Result<Vec<PositionRow>>,
        meter: RpcMeter,
    },
    Detail {
        result: Result<PositionDetail>,
        meter: RpcMeter,
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
    pub status: Option<RpcStatus>,
    pub status_line: String,
    pub should_quit: bool,
    pub pending: PendingWork,
    pub rpc_dirty: bool,
    pub detail: Option<PositionDetail>,
    pub detail_error: Option<String>,
    pub detail_loading: bool,
    /// True while list RPC is in flight (survives clearing `pending` before fetch).
    pub list_loading: bool,
    /// `p` Solscan overlay active — second `p` restores the previous status line.
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
}

impl App {
    pub fn new(rpc_url: String, wallet: Option<Pubkey>, existing_db: Option<PathBuf>) -> Self {
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
            jupiter_api_key: std::env::var("JUPITER_API_KEY").ok().filter(|s| !s.is_empty()),
            titan_api_key: std::env::var("TITAN_API_KEY").ok().filter(|s| !s.is_empty()),
            api_paid_fast: false,
            rpc_url: rpc_url.clone(),
            cli_rpc_url: rpc_url,
            cli_wallet: wallet,
            wallet: None,
            positions: Vec::new(),
            venue_filter: VenueFilter::All,
            table_state: TableState::default(),
            status: None,
            status_line: String::new(),
            should_quit: false,
            pending: PendingWork::None,
            rpc_dirty: false,
            detail: None,
            detail_error: None,
            detail_loading: false,
            list_loading: false,
            links_overlay: false,
            status_before_links: None,
            load_started: None,
            bg_rx: None,
            rpc_meter: RpcMeter::default(),
            help_return: Screen::Positions,
            help_scroll: 0,
        };

        if let Some(path) = existing_db {
            match Store::open(&path, false) {
                Ok(store) => {
                    app.finish_store(store);
                }
                Err(e) => {
                    app.screen = Screen::DbSetup;
                    app.db.error = Some(format!("open {}: {e:#}", display_path(&path)));
                    app.db.input = display_path(&path);
                    app.db.phase = DbSetupPhase::ChooseLocation;
                    app.status_line =
                        "could not open saved DB · ↑↓ choose path · Enter · q quit".into();
                }
            }
        } else {
            app.screen = Screen::DbSetup;
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
        if let Some(k) = &settings.jupiter_api_key {
            if !k.is_empty() {
                self.jupiter_api_key = Some(k.clone());
            }
        }
        if let Some(k) = &settings.titan_api_key {
            if !k.is_empty() {
                self.titan_api_key = Some(k.clone());
            }
        }
        self.api_paid_fast = settings.api_paid_fast;
        if let Some(url) = &settings.rpc_url {
            self.rpc_url = url.clone();
            self.rpc_dirty = true;
        }
        if self.cli_wallet.is_none() {
            if let Some(pk) = &settings.wallet_pubkey {
                if let Ok(pk) = pk.parse::<Pubkey>() {
                    self.wallet = Some(pk);
                }
            }
        }
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

    fn persist_wallet(&self, pubkey: &Pubkey, keypair_path: Option<&str>) {
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
            self.enter_positions();
        }
    }

    fn enter_positions(&mut self) {
        self.screen = Screen::Positions;
        self.detail = None;
        self.detail_error = None;
        self.begin_list_load(false, "loading from RPC…");
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
        format!(
            "ok · {kind} · {shown}/{total} · {venue} · ? help · v venue · Enter detail · p Solscan · s settings · r refresh · q"
        )
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
            Screen::Detail => "detail · p pubkey/Solscan · r refresh · ? help · Esc back".into(),
            Screen::Settings => "settings · ↑↓ · Enter · ? help · Esc back".into(),
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
        self.list_loading
            || self.detail_loading
            || self.bg_rx.is_some()
            || self.pending != PendingWork::None
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
                    self.status_line =
                        "type SQLite path · Enter · Esc back".into();
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
            self.status_line = format!(
                "DB exists at {} · ↑↓ · Enter",
                display_path(&path)
            );
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
                self.status_line =
                    "choose where to store SQLite · ↑↓ · Enter · q quit".into();
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
            SettingsItem::JupiterKey => {
                self.api_key_target = ApiKeyTarget::Jupiter;
                self.screen = Screen::SettingsApiKey;
                self.settings_api_key = WizardState::new();
                self.settings_api_key.step = SetupStep::Input;
                self.settings_api_key.input =
                    self.jupiter_api_key.clone().unwrap_or_default();
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
                self.settings_api_key.input =
                    self.titan_api_key.clone().unwrap_or_default();
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
                self.status_line =
                    "change wallet · ↑↓ choose · Enter · Esc quit".into();
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
                    "Jupiter API key cleared · keyless lite + throttle".into()
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
            let result = api.position_detail(&rpc, &row, store.as_ref(), refresh_meta);
            let _ = tx.send(BgFetchResult::Detail {
                result,
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
                self.load_started = None;
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
                        self.status_line = self.positions_status_line();
                    }
                    Err(e) => {
                        self.status_line =
                            format!("rpc error: {e:#} · press r to retry · q to quit");
                    }
                }
            }
            BgFetchResult::Detail {
                result,
                meter,
            } => {
                self.rpc_meter.merge(&meter);
                self.detail_loading = false;
                self.load_started = None;
                match result {
                    Ok(d) => {
                        self.detail = Some(d);
                        self.detail_error = None;
                        self.status_line =
                            "detail · p pubkey/Solscan · r refresh · ? help · Esc back".into();
                    }
                    Err(e) => {
                        self.detail_error = Some(format!("{e:#}"));
                        self.status_line = "detail error · r retry · Esc back".into();
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
                self.enter_positions();
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
            self.rpc_offer.error =
                Some("that still looks like a public Solana endpoint".into());
            return;
        }
        self.rpc_url = raw.to_string();
        self.rpc_dirty = true;
        self.persist_rpc();
        self.rpc_offer.error = None;
        self.enter_positions();
    }

    /// Hide Solscan overlay and restore the previous status (second `p`).
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

    /// Toggle full position pubkey + Solscan in the status line (`p` · press again to hide).
    pub fn toggle_selected_position_link(&mut self) {
        if self.links_overlay {
            self.restore_links_overlay();
            return;
        }
        let Some(row) = self.selected_row() else {
            self.status_line = "select a position first · then p for pubkey / Solscan".into();
            return;
        };
        let pk = row.address.to_string();
        let url = sol_clmm_manager::solscan_account_url(&row.address);
        self.status_before_links = Some(self.status_line.clone());
        self.links_overlay = true;
        self.status_line = format!("position {pk} · {url} · p again to hide");
    }

    pub fn toggle_detail_position_link(&mut self) {
        if self.links_overlay {
            self.restore_links_overlay();
            return;
        }
        let Some(d) = &self.detail else {
            self.status_line = "no detail loaded · p for pubkey / Solscan".into();
            return;
        };
        let pk = d.row.address.to_string();
        let pos_url = sol_clmm_manager::solscan_account_url(&d.row.address);
        let pool_url = sol_clmm_manager::solscan_account_url(&d.pool.address);
        self.status_before_links = Some(self.status_line.clone());
        self.links_overlay = true;
        self.status_line =
            format!("position {pk} · {pos_url} · pool {pool_url} · p again to hide");
    }

    pub fn select_next(&mut self) {
        self.dismiss_links_overlay();
        let len = self.filtered_indices().len();
        if len == 0 {
            return;
        }
        let i = self.table_state.selected().map(|i| (i + 1) % len).unwrap_or(0);
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

