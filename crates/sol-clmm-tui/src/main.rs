//! TUI binary for Solana CLMM / DLMM positions (Orca · Raydium · Meteora).

mod app;
mod theme;
mod ui;
#[cfg(feature = "write")]
mod wallet_flow;
#[cfg(feature = "write")]
mod write_flow;

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
use sol_clmm_manager::rpc;
use solana_client::rpc_client::RpcClient;
use solana_keypair::{Signer, read_keypair_file};
use solana_pubkey::Pubkey;
use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::Duration;

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

    /// Path to JSON keypair (optional; for write paths)
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
    let db_from_cli = cli.db.is_some();
    let existing_db = sol_clmm_manager::resolve_existing_db(cli.db.as_deref())?;
    let mut client = rpc::connect(&cli.rpc_url);
    let mut app = App::new(cli.rpc_url.clone(), wallet, existing_db, db_from_cli);
    // CLI path wins for this session and is persisted as a path string only (not key bytes).
    if let Some(path) = cli.keypair.as_ref() {
        let p = path.display().to_string();
        app.keypair_path = Some(p.clone());
        if let Some(store) = &app.store {
            let _ = store.set_keypair_path(&p);
        }
    }

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
            #[cfg(feature = "write")]
            PendingWork::Wallet => {
                app.pending = PendingWork::None;
                app.spawn_wallet_fetch();
            }
        }

        let waiting = app.poll_bg_fetch();
        let poll_ms = if waiting || app.is_busy() { 40 } else { 250 };

        if event::poll(Duration::from_millis(poll_ms)).context("poll")?
            && let Event::Key(key) = event::read().context("read")?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            handle_key(app, key.code, client);
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, #[allow(unused_variables)] rpc: &RpcClient) {
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
        Screen::WriteConsent => match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.write_allowed = Some(false);
                app.enter_positions_public();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.setup.selected = (app.setup.selected + 1) % 3;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.setup.selected = if app.setup.selected == 0 {
                    2
                } else {
                    app.setup.selected - 1
                };
            }
            KeyCode::Enter => app.write_consent_activate(),
            _ => {}
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
            #[cfg(feature = "write")]
            KeyCode::Char('n') => {
                if !app.is_busy() {
                    app.open_create();
                }
            }
            #[cfg(feature = "write")]
            KeyCode::Char('w') => {
                if !app.is_busy() {
                    app.open_wallet(rpc);
                }
            }
            KeyCode::Char('p') => app.toggle_selected_position_link(),
            KeyCode::Char('v') => app.cycle_venue_filter(),
            KeyCode::Char('y') if !app.is_busy() => app.open_history(),
            KeyCode::Char('r') => app.request_list_refresh(),
            KeyCode::Enter => app.open_selected_detail(),
            KeyCode::Down | KeyCode::Char('j') if !app.is_busy() => {
                app.select_next();
            }
            KeyCode::Up | KeyCode::Char('k') if !app.is_busy() => {
                app.select_prev();
            }
            _ => {}
        },
        Screen::History => match code {
            KeyCode::Esc | KeyCode::Char('q') => app.close_history(),
            KeyCode::Char('?') | KeyCode::Char('h') => app.open_help(),
            KeyCode::Char('r') => {
                app.reload_history();
                app.status_line = app.history_status_line();
            }
            KeyCode::Enter | KeyCode::Char('p') => app.toggle_selected_history_link(),
            KeyCode::Down | KeyCode::Char('j') => app.history_select_next(),
            KeyCode::Up | KeyCode::Char('k') => app.history_select_prev(),
            _ => {}
        },
        #[cfg(feature = "write")]
        Screen::CreatePosition => {
            use write_flow::CreateAdvance;
            // While typing pool/amount, every char (incl. base58 L/H/j/k) goes to the buffer.
            let typing = app.create.as_ref().is_some_and(|w| {
                matches!(
                    w.step,
                    write_flow::CreateStep::Pool | write_flow::CreateStep::Deposit
                )
            });
            let on_range = app
                .create
                .as_ref()
                .is_some_and(|w| w.step == write_flow::CreateStep::Range);
            let on_deposit = app
                .create
                .as_ref()
                .is_some_and(|w| w.step == write_flow::CreateStep::Deposit);
            match code {
                KeyCode::Esc => {
                    if let Some(w) = app.create.as_mut() {
                        if w.on_back() {
                            app.cancel_create();
                        }
                    } else {
                        app.cancel_create();
                    }
                }
                KeyCode::Enter => {
                    let adv = app
                        .create
                        .as_mut()
                        .map(|w| w.on_enter())
                        .unwrap_or(CreateAdvance::Continue);
                    match adv {
                        CreateAdvance::NeedFetch => app.create_fetch_pool(rpc),
                        CreateAdvance::Ready => app.create_submit_review(rpc),
                        CreateAdvance::Continue => {}
                    }
                }
                KeyCode::Tab if !typing => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_tab();
                    }
                }
                KeyCode::Left if !typing => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_left(false);
                    }
                }
                KeyCode::Right if !typing => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_right(false);
                    }
                }
                KeyCode::Up => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_up();
                    }
                }
                KeyCode::Down => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_down();
                    }
                }
                KeyCode::Char('k') if !typing => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_up();
                    }
                }
                KeyCode::Char('j') if !typing => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_down();
                    }
                }
                KeyCode::Backspace => {
                    if let Some(w) = app.create.as_mut()
                        && let Some(buf) = w.active_input_mut()
                    {
                        buf.pop();
                    }
                }
                KeyCode::Char('H') if !typing => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_left(true);
                    }
                }
                KeyCode::Char('L') if !typing => {
                    if let Some(w) = app.create.as_mut() {
                        w.on_right(true);
                    }
                }
                KeyCode::Char('1') if on_range => {
                    if let Some(w) = app.create.as_mut() {
                        w.apply_pct_range(5.0);
                    }
                }
                KeyCode::Char('2') if on_range => {
                    if let Some(w) = app.create.as_mut() {
                        w.apply_pct_range(10.0);
                    }
                }
                KeyCode::Char('3') if on_range => {
                    if let Some(w) = app.create.as_mut() {
                        w.apply_pct_range(25.0);
                    }
                }
                KeyCode::Char('+') | KeyCode::Char('=') if on_range => {
                    if let Some(w) = app.create.as_mut() {
                        app.status_line = w.on_chart_zoom(1);
                    }
                }
                KeyCode::Char('-') | KeyCode::Char('_') if on_range => {
                    if let Some(w) = app.create.as_mut() {
                        app.status_line = w.on_chart_zoom(-1);
                    }
                }
                KeyCode::Char('0') if on_range => {
                    if let Some(w) = app.create.as_mut() {
                        app.status_line = w.on_chart_zoom(0);
                    }
                }
                // Amount presets (empty field only so "0.45" typing still works).
                KeyCode::Char('4') if on_deposit && amount_field_empty(app) => {
                    if let Some(w) = app.create.as_mut() {
                        w.apply_amount_pct(25);
                    }
                }
                KeyCode::Char('5') if on_deposit && amount_field_empty(app) => {
                    if let Some(w) = app.create.as_mut() {
                        w.apply_amount_pct(50);
                    }
                }
                KeyCode::Char('6') if on_deposit && amount_field_empty(app) => {
                    if let Some(w) = app.create.as_mut() {
                        w.apply_amount_pct(100);
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(w) = app.create.as_mut()
                        && let Some(buf) = w.active_input_mut()
                    {
                        // Strip whitespace from paste / accidental spaces.
                        if !c.is_whitespace() {
                            buf.push(c);
                        }
                    }
                }
                _ => {}
            }
        }
        #[cfg(feature = "write")]
        Screen::WriteConfirm => match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let phase = app
                    .write_confirm
                    .as_ref()
                    .map(|c| c.phase)
                    .unwrap_or(write_flow::ConfirmPhase::Review);
                if matches!(phase, write_flow::ConfirmPhase::Busy) {
                    // ignore while sending
                } else {
                    app.write_confirm_close();
                }
            }
            KeyCode::Char('p') => app.toggle_confirm_links(),
            KeyCode::Char('[') => {
                let phase = app
                    .write_confirm
                    .as_ref()
                    .map(|c| c.phase)
                    .unwrap_or(write_flow::ConfirmPhase::Tip);
                match phase {
                    write_flow::ConfirmPhase::Tip => app.write_confirm_nudge_tip(false),
                    write_flow::ConfirmPhase::Review => app.write_confirm_back_to_tip(),
                    _ => {}
                }
            }
            KeyCode::Char(']') => {
                let phase = app
                    .write_confirm
                    .as_ref()
                    .map(|c| c.phase)
                    .unwrap_or(write_flow::ConfirmPhase::Tip);
                if matches!(phase, write_flow::ConfirmPhase::Tip) {
                    app.write_confirm_nudge_tip(true);
                }
            }
            KeyCode::Enter => {
                let phase = app
                    .write_confirm
                    .as_ref()
                    .map(|c| c.phase)
                    .unwrap_or(write_flow::ConfirmPhase::Tip);
                match phase {
                    write_flow::ConfirmPhase::Tip => app.write_confirm_simulate(rpc),
                    write_flow::ConfirmPhase::Review => app.write_confirm_send(rpc),
                    write_flow::ConfirmPhase::Failed => {
                        let choice = app.write_confirm.as_ref().map(|c| c.choice).unwrap_or(0);
                        if choice == 0 {
                            app.write_confirm_retry(rpc);
                        } else {
                            app.write_confirm_close();
                        }
                    }
                    write_flow::ConfirmPhase::Done => app.write_confirm_close(),
                    write_flow::ConfirmPhase::Busy => {}
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                if let Some(c) = app.write_confirm.as_mut()
                    && c.phase == write_flow::ConfirmPhase::Failed
                {
                    c.choice = 1 - c.choice.min(1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let phase = app
                    .write_confirm
                    .as_ref()
                    .map(|c| c.phase)
                    .unwrap_or(write_flow::ConfirmPhase::Tip);
                if matches!(phase, write_flow::ConfirmPhase::Tip) {
                    app.write_confirm_nudge_tip(false);
                } else {
                    app.confirm_scroll_by(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let phase = app
                    .write_confirm
                    .as_ref()
                    .map(|c| c.phase)
                    .unwrap_or(write_flow::ConfirmPhase::Tip);
                if matches!(phase, write_flow::ConfirmPhase::Tip) {
                    app.write_confirm_nudge_tip(true);
                } else {
                    app.confirm_scroll_by(1);
                }
            }
            KeyCode::PageUp => app.confirm_scroll_by(-8),
            KeyCode::PageDown => app.confirm_scroll_by(8),
            _ => {}
        },
        #[cfg(not(feature = "write"))]
        Screen::CreatePosition
        | Screen::WriteConfirm
        | Screen::Manage
        | Screen::ManageDeposit
        | Screen::Wallet
        | Screen::ManageCompound
        | Screen::ManageWithdraw => {
            if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                app.screen = Screen::Positions;
                app.status_line = app.positions_status_line();
            }
        }
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
            KeyCode::Char('+') | KeyCode::Char('=') if !app.detail_loading => {
                app.detail_chart_zoom(1);
            }
            KeyCode::Char('-') | KeyCode::Char('_') if !app.detail_loading => {
                app.detail_chart_zoom(-1);
            }
            KeyCode::Char('0') if !app.detail_loading => {
                app.detail_chart_zoom(0);
            }
            #[cfg(feature = "write")]
            KeyCode::Char('m') if !app.detail_loading => {
                app.open_manage();
            }
            KeyCode::Char('y') if !app.detail_loading && !app.is_busy() => {
                app.open_position_history();
            }
            _ => {}
        },
        #[cfg(feature = "write")]
        Screen::Manage => match code {
            KeyCode::Esc | KeyCode::Char('q') => app.manage_close(),
            KeyCode::Down | KeyCode::Char('j') => app.manage_select_next(),
            KeyCode::Up | KeyCode::Char('k') => app.manage_select_prev(),
            KeyCode::Enter => app.manage_activate(rpc),
            KeyCode::Char('p') => app.toggle_manage_links(),
            KeyCode::Char('?') | KeyCode::Char('h') => app.open_help(),
            _ => {}
        },
        #[cfg(feature = "write")]
        Screen::ManageDeposit => {
            use crate::write_flow::ManageDepositStep;
            let on_amount = app
                .manage_deposit
                .as_ref()
                .is_some_and(|w| w.step == ManageDepositStep::Amount);
            let on_slip = app
                .manage_deposit
                .as_ref()
                .is_some_and(|w| w.step == ManageDepositStep::SwapSlippage);
            match code {
                KeyCode::Esc => app.manage_deposit_back(),
                KeyCode::Enter => app.manage_deposit_advance(rpc),
                KeyCode::Down | KeyCode::Char('j') if !on_amount => {
                    if let Some(w) = app.manage_deposit.as_mut() {
                        w.select_next();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') if !on_amount => {
                    if let Some(w) = app.manage_deposit.as_mut() {
                        w.select_prev();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Tab if on_slip => {
                    if let Some(w) = app.manage_deposit.as_mut() {
                        w.toggle_router();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Backspace if on_amount => {
                    if let Some(w) = app.manage_deposit.as_mut() {
                        w.input_backspace();
                    }
                }
                KeyCode::Char(c) if on_amount => {
                    if let Some(w) = app.manage_deposit.as_mut() {
                        w.input_char(c);
                    }
                }
                _ => {}
            }
        }
        #[cfg(feature = "write")]
        Screen::Wallet => {
            if app.wallet_loading || matches!(app.pending, PendingWork::Wallet) {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    app.cancel_wallet();
                }
                return;
            }
            let in_sub = app
                .wallet_wiz
                .as_ref()
                .is_some_and(|w| w.wrap.is_some() || w.swap.is_some() || w.create_pool.is_some());
            match code {
                KeyCode::Esc => {
                    if in_sub {
                        if let Some(w) = app.wallet_wiz.as_mut() {
                            w.wrap = None;
                            w.swap = None;
                            w.create_pool = None;
                            w.error = None;
                            app.status_line = w.status_hint();
                        }
                    } else {
                        app.cancel_wallet();
                    }
                }
                KeyCode::Enter => app.wallet_enter(rpc),
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(w) = app.wallet_wiz.as_mut() {
                        w.down();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(w) = app.wallet_wiz.as_mut() {
                        w.up();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Tab => {
                    if let Some(w) = app.wallet_wiz.as_mut() {
                        w.tab();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Backspace => {
                    if let Some(w) = app.wallet_wiz.as_mut() {
                        w.input_backspace();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(w) = app.wallet_wiz.as_mut() {
                        w.input_char(c);
                    }
                }
                _ => {}
            }
        }
        #[cfg(feature = "write")]
        Screen::ManageCompound => {
            use crate::write_flow::ManageCompoundStep;
            let on_slip = app
                .manage_compound
                .as_ref()
                .is_some_and(|w| w.step == ManageCompoundStep::SwapSlippage);
            match code {
                KeyCode::Esc => app.manage_compound_back(),
                KeyCode::Enter => app.manage_compound_advance(),
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(w) = app.manage_compound.as_mut() {
                        w.select_next();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(w) = app.manage_compound.as_mut() {
                        w.select_prev();
                        app.status_line = w.status_hint();
                    }
                }
                KeyCode::Tab if on_slip => {
                    if let Some(w) = app.manage_compound.as_mut() {
                        w.toggle_router();
                        app.status_line = w.status_hint();
                    }
                }
                _ => {}
            }
        }
        #[cfg(feature = "write")]
        Screen::ManageWithdraw => match code {
            KeyCode::Esc => app.manage_withdraw_back(),
            KeyCode::Enter => app.manage_withdraw_submit(),
            KeyCode::Down => {
                if let Some(w) = app.manage_withdraw.as_mut() {
                    w.select_next();
                    app.status_line = w.status_hint();
                }
            }
            KeyCode::Up => {
                if let Some(w) = app.manage_withdraw.as_mut() {
                    w.select_prev();
                    app.status_line = w.status_hint();
                }
            }
            KeyCode::Backspace => {
                if let Some(w) = app.manage_withdraw.as_mut() {
                    w.input_backspace();
                    app.status_line = w.status_hint();
                }
            }
            KeyCode::Char(c) => {
                if let Some(w) = app.manage_withdraw.as_mut() {
                    // j/k are typing digits elsewhere; here digits/. go to % field.
                    if c == 'j' {
                        w.select_next();
                    } else if c == 'k' {
                        w.select_prev();
                    } else {
                        w.input_char(c);
                    }
                    app.status_line = w.status_hint();
                }
            }
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

#[cfg(feature = "write")]
fn amount_field_empty(app: &App) -> bool {
    app.create
        .as_ref()
        .is_some_and(|w| w.deposit_amount_input.is_empty())
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
