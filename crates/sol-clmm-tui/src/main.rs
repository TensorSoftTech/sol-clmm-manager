//! TUI binary for Solana CLMM / DLMM positions (Orca · Raydium · Meteora).

mod app;
mod theme;
mod ui;

use anyhow::{Context, Result};
use app::{App, PendingWork, Screen, SetupStep};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use solana_client::rpc_client::RpcClient;
use solana_keypair::{Signer, read_keypair_file};
use solana_pubkey::Pubkey;
use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::Duration;
use sol_clmm_manager::rpc;

#[derive(Debug, Parser)]
#[command(
    name = "sol-clmm-tui",
    about = "WIP TUI for Solana CLMM positions (Orca Whirlpools · Raydium · Meteora). Not financial advice."
)]
struct Cli {
    /// Solana HTTP RPC (public mainnet by default)
    #[arg(long, env = "RPC_URL", default_value = rpc::DEFAULT_RPC_URL)]
    rpc_url: String,

    /// Wallet pubkey for read-only access (no private key needed)
    #[arg(long, env = "PUBKEY")]
    pubkey: Option<String>,

    /// Path to JSON keypair (optional; for later write paths)
    #[arg(long, env = "KEYPAIR")]
    keypair: Option<PathBuf>,

    /// SQLite database path (skips first-run DB wizard when set)
    #[arg(long, env = "SOL_CLMM_DB")]
    db: Option<PathBuf>,
}

fn main() -> Result<()> {
    color_eyre::install().ok();
    let cli = Cli::parse();

    let wallet = resolve_wallet(cli.pubkey.as_deref(), cli.keypair.as_ref())?;
    let existing_db = sol_clmm_manager::resolve_existing_db(cli.db.as_deref())?;
    let mut client = rpc::connect(&cli.rpc_url);
    let mut app = App::new(cli.rpc_url.clone(), wallet, existing_db);

    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, &mut app, &mut client);
    restore_terminal(&mut terminal)?;
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    client: &mut RpcClient,
) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if app.rpc_dirty {
            *client = rpc::connect(&app.rpc_url);
            app.rpc_dirty = false;
        }

        // Kick off background work so the load bar can animate while RPC runs.
        match app.pending {
            PendingWork::None => {}
            PendingWork::List { refresh_meta } => {
                app.pending = PendingWork::None;
                app.spawn_list_fetch(refresh_meta);
            }
            PendingWork::Detail { refresh_meta } => {
                app.pending = PendingWork::None;
                app.spawn_detail_fetch(refresh_meta);
            }
        }

        let waiting = app.poll_bg_fetch();
        let poll_ms = if waiting || app.is_busy() { 40 } else { 250 };

        if event::poll(Duration::from_millis(poll_ms)).context("poll")? {
            if let Event::Key(key) = event::read().context("read")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                handle_key(app, key.code);
            }
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode) {
    match app.screen {
        Screen::DbSetup => match app.db.phase {
            app::DbSetupPhase::ChooseLocation | app::DbSetupPhase::ConfirmExist => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    if matches!(app.db.phase, app::DbSetupPhase::ChooseLocation) {
                        app.quit();
                    } else {
                        app.db_back();
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => app.db_select_next(),
                KeyCode::Up | KeyCode::Char('k') => app.db_select_prev(),
                KeyCode::Enter => app.db_confirm(),
                _ => {}
            },
            app::DbSetupPhase::CustomInput => match code {
                KeyCode::Esc => app.db_back(),
                KeyCode::Enter => app.db_confirm(),
                KeyCode::Backspace => app.db_input_backspace(),
                KeyCode::Char(c) => app.db_input_char(c),
                _ => {}
            },
        },
        Screen::Setup => match app.setup.step {
            SetupStep::Choose => match code {
                KeyCode::Esc | KeyCode::Char('q') => app.quit(),
                KeyCode::Down | KeyCode::Char('j') => app.setup_select_next(),
                KeyCode::Up | KeyCode::Char('k') => app.setup_select_prev(),
                KeyCode::Enter => app.setup_confirm_choice(),
                _ => {}
            },
            SetupStep::Input => match code {
                KeyCode::Esc => app.setup_back(),
                KeyCode::Enter => app.setup_submit(),
                KeyCode::Backspace => app.setup_input_backspace(),
                KeyCode::Char(c) => app.setup_input_char(c),
                _ => {}
            },
        },
        Screen::RpcOffer => match app.rpc_offer.step {
            SetupStep::Choose => match code {
                KeyCode::Char('q') => app.quit(),
                KeyCode::Esc => app.rpc_back(),
                KeyCode::Down | KeyCode::Char('j') => app.rpc_select_next(),
                KeyCode::Up | KeyCode::Char('k') => app.rpc_select_prev(),
                KeyCode::Enter => app.rpc_confirm_choice(),
                _ => {}
            },
            SetupStep::Input => match code {
                KeyCode::Esc => app.rpc_back(),
                KeyCode::Enter => app.rpc_submit_private(),
                KeyCode::Backspace => app.rpc_input_backspace(),
                KeyCode::Char(c) => app.rpc_input_char(c),
                _ => {}
            },
        },
        Screen::Positions => match code {
            KeyCode::Char('q') => app.quit(),
            KeyCode::Esc => app.quit(),
            KeyCode::Char('?') | KeyCode::Char('h') => {
                if !app.is_busy() {
                    app.open_help();
                }
            }
            KeyCode::Char('s') => {
                if !app.is_busy() {
                    app.open_settings();
                }
            }
            KeyCode::Char('p') => app.toggle_selected_position_link(),
            KeyCode::Char('v') => app.cycle_venue_filter(),
            KeyCode::Char('r') => app.request_list_refresh(),
            KeyCode::Enter => app.open_selected_detail(),
            KeyCode::Down | KeyCode::Char('j') => {
                if !app.is_busy() {
                    app.select_next();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !app.is_busy() {
                    app.select_prev();
                }
            }
            _ => {}
        },
        Screen::Settings => match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.screen = Screen::Positions;
                app.status_line = app.positions_status_line();
            }
            KeyCode::Char('?') | KeyCode::Char('h') => app.open_help(),
            KeyCode::Down | KeyCode::Char('j') => app.settings_select_next(),
            KeyCode::Up | KeyCode::Char('k') => app.settings_select_prev(),
            KeyCode::Enter => app.settings_activate(),
            _ => {}
        },
        Screen::SettingsRpc => match code {
            KeyCode::Esc => app.settings_rpc_cancel(),
            KeyCode::Enter => app.settings_rpc_submit(),
            KeyCode::Backspace => app.settings_rpc_backspace(),
            KeyCode::Char(c) => app.settings_rpc_char(c),
            _ => {}
        },
        Screen::SettingsApiKey => match code {
            KeyCode::Esc => app.settings_api_key_cancel(),
            KeyCode::Enter => app.settings_api_key_submit(),
            KeyCode::Backspace => app.settings_api_key_backspace(),
            KeyCode::Char(c) => app.settings_api_key_char(c),
            _ => {}
        },
        Screen::Detail => match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if !app.detail_loading {
                    app.close_detail();
                }
            }
            KeyCode::Char('?') | KeyCode::Char('h') => {
                if !app.detail_loading {
                    app.open_help();
                }
            }
            KeyCode::Char('p') => app.toggle_detail_position_link(),
            KeyCode::Char('r') => app.request_detail_refresh(),
            _ => {}
        },
        Screen::Help => match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Char('h') => {
                app.close_help();
            }
            KeyCode::Down | KeyCode::Char('j') => app.help_scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => app.help_scroll_by(-1),
            KeyCode::PageDown => app.help_scroll_by(8),
            KeyCode::PageUp => app.help_scroll_by(-8),
            KeyCode::Home => app.help_scroll = 0,
            _ => {}
        },
    }
}

fn resolve_wallet(
    pubkey_arg: Option<&str>,
    keypair_path: Option<&PathBuf>,
) -> Result<Option<Pubkey>> {
    let from_pubkey = match pubkey_arg {
        Some(s) => Some(
            s.parse::<Pubkey>()
                .with_context(|| format!("invalid --pubkey / PUBKEY: {s}"))?,
        ),
        None => None,
    };

    let from_keypair = match keypair_path {
        Some(path) => {
            let kp = read_keypair_file(path)
                .map_err(|e| anyhow::anyhow!("read keypair {}: {e}", path.display()))?;
            Some(kp.pubkey())
        }
        None => None,
    };

    match (from_pubkey, from_keypair) {
        (Some(a), Some(b)) if a != b => anyhow::bail!(
            "--pubkey ({a}) and --keypair pubkey ({b}) differ; pass one, or make them match"
        ),
        (Some(pk), _) | (_, Some(pk)) => Ok(Some(pk)),
        (None, None) => Ok(None),
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode().context("enable_raw_mode")?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen).context("EnterAlternateScreen")?;
    Terminal::new(CrosstermBackend::new(out)).context("Terminal::new")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    terminal.backend_mut().execute(LeaveAlternateScreen).ok();
    Ok(())
}
