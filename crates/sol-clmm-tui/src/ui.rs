use crate::app::{
    ApiKeyTarget, App, DbExistChoice, DbPathChoice, DbSetupPhase, PendingWork, RpcChoice, Screen,
    SettingsItem, SetupStep, WalletSource,
};
use crate::theme;
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table,
};
use sol_clmm_manager::rpc::{self, lamports_to_sol};
use sol_clmm_manager::store::display_path;
use sol_clmm_manager::{
    IndexSpace, LiquiditySample, TokenSide, format_price, format_usd, format_usd_axis, scale_u128,
};

pub fn draw(frame: &mut Frame, app: &mut App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::BG)),
        frame.area(),
    );

    match app.screen {
        Screen::DbSetup => draw_db_setup(frame, app),
        Screen::Setup => draw_setup(frame, app),
        Screen::RpcOffer => draw_rpc_offer(frame, app),
        Screen::WriteConsent => draw_write_consent(frame, app),
        Screen::Positions => draw_positions_screen(frame, app),
        Screen::History => draw_history_screen(frame, app),
        Screen::Settings => draw_settings(frame, app),
        Screen::SettingsRpc => draw_settings_rpc(frame, app),
        Screen::SettingsApiKey => draw_settings_api_key(frame, app),
        Screen::Detail => draw_detail(frame, app),
        Screen::Help => draw_help(frame, app),
        #[cfg(feature = "write")]
        Screen::Manage => draw_manage(frame, app),
        #[cfg(not(feature = "write"))]
        Screen::Manage => draw_detail(frame, app),
        #[cfg(feature = "write")]
        Screen::ManageDeposit => draw_manage_deposit(frame, app),
        #[cfg(not(feature = "write"))]
        Screen::ManageDeposit => draw_detail(frame, app),
        #[cfg(feature = "write")]
        Screen::Wallet => draw_wallet(frame, app),
        #[cfg(not(feature = "write"))]
        Screen::Wallet => draw_detail(frame, app),
        #[cfg(feature = "write")]
        Screen::ManageCompound => draw_manage_compound(frame, app),
        #[cfg(not(feature = "write"))]
        Screen::ManageCompound => draw_detail(frame, app),
        #[cfg(feature = "write")]
        Screen::ManageWithdraw => draw_manage_withdraw(frame, app),
        #[cfg(not(feature = "write"))]
        Screen::ManageWithdraw => draw_detail(frame, app),
        Screen::CreatePosition => draw_create_position(frame, app),
        Screen::WriteConfirm => draw_write_confirm(frame, app),
    }
}

fn draw_create_position(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    #[cfg(feature = "write")]
    {
        use crate::write_flow::CreateStep;
        let show_chart = app
            .create
            .as_ref()
            .is_some_and(|w| w.step == CreateStep::Range && w.preview.is_some());

        let (progress_line, body_title, body_lines) = match &app.create {
            Some(w) => {
                let title = format!("create · {} · tx v1", w.step_title());
                let lines = create_wizard_lines(w);
                (create_progress_line(w.progress_index()), title, lines)
            }
            None => (
                Line::from(""),
                "create position · tx v1".into(),
                vec![Line::from(Span::styled(
                    "create wizard empty · Esc",
                    theme::muted(),
                ))],
            ),
        };

        // Fit content height — avoid a huge empty bordered panel.
        let body_inner = (body_lines.len() as u16).saturating_add(1).max(4);
        let body_h = body_inner
            .saturating_add(2)
            .min(area.height.saturating_sub(FOOTER_H + 4));

        if show_chart {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(body_h.min(10)),
                    Constraint::Min(8),
                    Constraint::Length(FOOTER_H),
                ])
                .split(area);
            frame.render_widget(
                Paragraph::new(progress_line).block(panel("wizard", false)),
                chunks[0],
            );
            frame.render_widget(
                Paragraph::new(body_lines).block(panel(&body_title, false)),
                chunks[1],
            );
            if let Some(w) = &app.create
                && let Some(p) = &w.preview
            {
                let (view, view_max) = p.chart_series_view();
                draw_liquidity_chart(
                    frame,
                    chunks[2],
                    &view,
                    view_max,
                    p.pool.liquidity,
                    None,
                    p.pool.index_space,
                    p.pool.tick_current_index,
                    &p.token_a.symbol,
                    &p.token_b.symbol,
                    p.token_a.decimals,
                    p.token_b.decimals,
                    Some(p.tick_lower),
                    Some(p.tick_upper),
                    w.range_focus.label(),
                    p.effective_half_spacings(),
                );
            }
            draw_footer(frame, app, chunks[3]);
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(body_h),
                    Constraint::Min(0),
                    Constraint::Length(FOOTER_H),
                ])
                .split(area);
            frame.render_widget(
                Paragraph::new(progress_line).block(panel("wizard", false)),
                chunks[0],
            );
            frame.render_widget(
                Paragraph::new(body_lines).block(panel(&body_title, false)),
                chunks[1],
            );
            // leftover space stays blank (no empty bordered void)
            draw_footer(frame, app, chunks[3]);
        }
    }

    #[cfg(not(feature = "write"))]
    {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(FOOTER_H)])
            .split(area);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "rebuild with --features write",
                theme::danger(),
            ))
            .block(panel("create position · tx v1", false)),
            chunks[0],
        );
        draw_footer(frame, app, chunks[1]);
    }
}

#[cfg(feature = "write")]
fn create_progress_line(cur: usize) -> Line<'static> {
    use crate::write_flow::CreateWizard;
    let mut spans = vec![Span::styled("path  ".to_string(), theme::header_key())];
    for (i, stage) in CreateWizard::PROGRESS_STAGES.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" → ".to_string(), theme::dim()));
        }
        if i == cur {
            spans.push(Span::styled(format!("[{stage}]"), theme::title()));
        } else if i < cur {
            spans.push(Span::styled((*stage).to_string(), theme::ok()));
        } else {
            spans.push(Span::styled((*stage).to_string(), theme::dim()));
        }
    }
    Line::from(spans)
}

#[cfg(feature = "write")]
fn style_wizard_lines(raw: Vec<String>) -> Vec<Line<'static>> {
    raw.into_iter()
        .map(|s| {
            if let Some(value) = s.strip_prefix("▸▸") {
                // ▸ is 3 UTF-8 bytes — never slice by byte index.
                let display = if value.is_empty() {
                    "type here…".to_string()
                } else {
                    value.to_string()
                };
                let value_style = if value.is_empty() {
                    theme::dim()
                } else {
                    theme::body()
                };
                Line::from(vec![
                    Span::styled("▸ ".to_string(), theme::input_cursor()),
                    Span::styled(display, value_style),
                    Span::styled("█".to_string(), theme::accent_blink()),
                ])
            } else if s.starts_with('!') {
                Line::from(Span::styled(s, theme::danger()))
            } else if s.starts_with('▸') {
                Line::from(Span::styled(s, theme::title()))
            } else if s.is_empty() {
                Line::from("")
            } else if s.starts_with("pubkey ok") || s.starts_with("example") {
                Line::from(Span::styled(s, theme::muted()))
            } else if s.starts_with("pool address") || s.starts_with("amount ") {
                Line::from(Span::styled(s, theme::header_key()))
            } else {
                Line::from(Span::styled(s, theme::body()))
            }
        })
        .collect()
}

#[cfg(feature = "write")]
fn create_wizard_lines(w: &crate::write_flow::CreateWizard) -> Vec<Line<'static>> {
    style_wizard_lines(w.lines())
}

#[cfg(feature = "write")]
fn draw_manage(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    let mut head = vec![Line::from(Span::styled("manage position", theme::title()))];
    if let Some(d) = &app.detail {
        head.push(Line::from(vec![
            Span::styled("position  ", theme::header_key()),
            Span::styled(d.row.address.to_string(), theme::accent_text()),
        ]));
        head.push(Line::from(vec![
            Span::styled("pool      ", theme::header_key()),
            Span::styled(d.pool.address.to_string(), theme::ok()),
            Span::styled(
                format!("  · {} / {}", d.token_a.symbol, d.token_b.symbol),
                theme::muted(),
            ),
        ]));
        head.push(Line::from(vec![
            Span::styled("liquidity ", theme::header_key()),
            Span::styled(
                format!("{}  +  {}", d.amount_a_ui, d.amount_b_ui),
                theme::body(),
            ),
            Span::styled(format!("  · L={}", d.position_liquidity), theme::dim()),
        ]));
        head.push(Line::from(Span::styled(
            "p · full position + pool pubkey in status (copy)",
            theme::muted(),
        )));
    }
    frame.render_widget(
        Paragraph::new(head).block(panel("context", false)),
        chunks[0],
    );

    let items: Vec<ListItem> = crate::app::ManageChoice::ALL
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mark = if i == app.manage_choice { "▸" } else { " " };
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!("{mark} {}", c.title()),
                    if i == app.manage_choice {
                        theme::accent_text()
                    } else {
                        theme::body()
                    },
                )),
                Line::from(Span::styled(format!("    {}", c.detail()), theme::muted())),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.manage_choice));
    frame.render_stateful_widget(
        List::new(items)
            .block(panel("↑↓ · Enter run · Esc back", false))
            .highlight_symbol(""),
        chunks[1],
        &mut state,
    );
    draw_footer(frame, app, chunks[2]);
}

#[cfg(feature = "write")]
fn draw_manage_deposit(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(FOOTER_H)])
        .split(area);

    let lines = app
        .manage_deposit
        .as_ref()
        .map(|w| style_wizard_lines(w.lines()))
        .unwrap_or_else(|| vec![Line::from("no deposit wizard")]);
    frame.render_widget(
        Paragraph::new(lines).block(panel("deposit · existing position", false)),
        chunks[0],
    );
    draw_footer(frame, app, chunks[1]);
}

#[cfg(feature = "write")]
fn draw_wallet(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(FOOTER_H)])
        .split(area);

    let loading = app.wallet_loading || matches!(app.pending, PendingWork::Wallet);
    if loading {
        draw_panel_load(
            frame,
            chunks[0],
            app.load_bar_ratio(),
            "wallet",
            "loading balances…",
        );
        draw_footer(frame, app, chunks[1]);
        return;
    }

    let lines = app
        .wallet_wiz
        .as_ref()
        .map(|w| style_wizard_lines(w.lines()))
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                "balances failed · Esc back · w retry",
                theme::danger(),
            ))]
        });
    let title = match app.wallet_wiz.as_ref() {
        Some(w) if w.wrap.is_some() => "wallet · wrap SOL",
        Some(w) if w.swap.is_some() => "wallet · swap",
        Some(w) if w.create_pool.is_some() => "wallet · create pool",
        _ => "wallet · wrap · unwrap · swap · create pool",
    };
    frame.render_widget(Paragraph::new(lines).block(panel(title, false)), chunks[0]);
    draw_footer(frame, app, chunks[1]);
}

#[cfg(feature = "write")]
fn draw_manage_compound(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(FOOTER_H)])
        .split(area);

    let lines = app
        .manage_compound
        .as_ref()
        .map(|w| style_wizard_lines(w.lines()))
        .unwrap_or_else(|| vec![Line::from("no compound wizard")]);
    frame.render_widget(
        Paragraph::new(lines).block(panel("compound · claim → deposit", false)),
        chunks[0],
    );
    draw_footer(frame, app, chunks[1]);
}

#[cfg(feature = "write")]
fn draw_manage_withdraw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(FOOTER_H)])
        .split(area);

    let title = app
        .manage_withdraw
        .as_ref()
        .map(|w| format!("{} · % of liquidity", w.kind.title()))
        .unwrap_or_else(|| "withdraw".into());
    let lines = app
        .manage_withdraw
        .as_ref()
        .map(|w| style_wizard_lines(w.lines()))
        .unwrap_or_else(|| vec![Line::from("no withdraw wizard")]);
    frame.render_widget(Paragraph::new(lines).block(panel(&title, false)), chunks[0]);
    draw_footer(frame, app, chunks[1]);
}

fn draw_write_confirm(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    #[cfg(not(feature = "write"))]
    {
        let _ = app;
        frame.render_widget(
            Paragraph::new(Span::styled(
                "rebuild with --features write",
                theme::danger(),
            ))
            .block(panel("confirm write", false)),
            area,
        );
    }
    #[cfg(feature = "write")]
    {
        let tip_phase = app
            .write_confirm
            .as_ref()
            .is_some_and(|c| c.phase == crate::write_flow::ConfirmPhase::Tip);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(if tip_phase {
                // Room for priority-fee preset list.
                [
                    Constraint::Length(6),
                    Constraint::Min(12),
                    Constraint::Length(FOOTER_H),
                ]
            } else {
                [
                    Constraint::Length(10),
                    Constraint::Min(8),
                    Constraint::Length(FOOTER_H),
                ]
            })
            .split(area);

        let busy = app
            .write_confirm
            .as_ref()
            .is_some_and(|c| c.phase == crate::write_flow::ConfirmPhase::Busy);
        if busy {
            draw_panel_load(
                frame,
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(8), Constraint::Length(FOOTER_H)])
                    .split(area)[0],
                app.load_bar_ratio(),
                "confirm write",
                "sending · waiting for confirmation…",
            );
            draw_footer(frame, app, chunks[2]);
            return;
        }

        let Some(c) = &app.write_confirm else {
            frame.render_widget(
                Paragraph::new(Span::styled("confirm empty · Esc", theme::muted()))
                    .block(panel("confirm write", false)),
                chunks[0],
            );
            draw_footer(frame, app, chunks[2]);
            return;
        };

        let mut head = vec![
            Line::from(Span::styled(
                format!("{} · {}", c.plan.op.label(), c.plan.venue_label),
                theme::title(),
            )),
            Line::from(Span::styled(c.plan.summary.clone(), theme::body())),
        ];
        if let Some(pos) = c.position_pk.as_deref() {
            head.push(Line::from(vec![
                Span::styled("position  ", theme::header_key()),
                Span::styled(pos.to_string(), theme::accent_text()),
            ]));
        }
        if let Some(pool) = c.pool_pk.as_deref() {
            let mut spans = vec![
                Span::styled("pool      ", theme::header_key()),
                Span::styled(pool.to_string(), theme::ok()),
            ];
            if let Some(lbl) = c.pool_label.as_deref() {
                spans.push(Span::styled(format!("  · {lbl}"), theme::muted()));
            }
            head.push(Line::from(spans));
        }
        head.push(Line::from(Span::styled(
            format!(
                "ixs {} · {}",
                c.plan.instructions.len(),
                c.plan.expected.note
            ),
            theme::dim(),
        )));
        if c.phase != crate::write_flow::ConfirmPhase::Tip {
            head.push(Line::from(Span::styled(
                format!("priority fee · {}", crate::app::tip_label(c.tip_lamports)),
                theme::accent_text(),
            )));
        }
        if let Some(b) = c.budget_label.as_deref() {
            head.push(Line::from(Span::styled(
                format!("v1 config · {b}"),
                theme::muted(),
            )));
        }
        match c.phase {
            crate::write_flow::ConfirmPhase::Tip => {
                head.push(Line::from(Span::styled(
                    "↑↓ pick priority fee · Enter simulate · Esc cancel",
                    theme::accent_text(),
                )));
            }
            crate::write_flow::ConfirmPhase::Review => {
                head.push(Line::from(Span::styled(
                    "sim OK · Enter send · [ back to tip · ↑↓ logs · Esc",
                    theme::accent_text(),
                )));
            }
            crate::write_flow::ConfirmPhase::Failed => {
                head.push(Line::from(Span::styled(
                    "sim failed · ←→ choice · ↑↓ scroll · Enter",
                    theme::danger(),
                )));
                for (i, label) in crate::write_flow::WriteConfirm::failed_choices()
                    .iter()
                    .enumerate()
                {
                    let mark = if i == c.choice { "▸" } else { " " };
                    head.push(Line::from(Span::styled(
                        format!("{mark} {label}"),
                        theme::body(),
                    )));
                }
            }
            crate::write_flow::ConfirmPhase::Done => {
                head.push(Line::from(Span::styled("landed", theme::ok())));
                if let Some(paid) = c.paid_label.as_deref() {
                    head.push(Line::from(Span::styled(paid.to_string(), theme::ok())));
                }
                if let Some(bal) = c.wallet_balances.as_ref() {
                    let (sa, sb, da, db) = app
                        .detail
                        .as_ref()
                        .map(|d| {
                            (
                                d.token_a.symbol.as_str(),
                                d.token_b.symbol.as_str(),
                                d.token_a.decimals,
                                d.token_b.decimals,
                            )
                        })
                        .unwrap_or(("A", "B", 9, 6));
                    head.push(Line::from(Span::styled(
                        format!("wallet · {}", bal.summary_line(sa, sb, da, db)),
                        theme::body(),
                    )));
                }
                head.push(Line::from(Span::styled(
                    "saved to sqlite · Enter back to manage",
                    theme::muted(),
                )));
            }
            crate::write_flow::ConfirmPhase::Busy => {}
        }
        frame.render_widget(
            Paragraph::new(head).block(panel("confirm write", false)),
            chunks[0],
        );

        if c.phase == crate::write_flow::ConfirmPhase::Tip {
            let mut tip_lines = vec![
                Line::from(Span::styled(
                    "priority fee (tx v1) · selected below",
                    theme::header_key(),
                )),
                Line::from(""),
            ];
            for (i, &preset) in crate::app::TIP_PRESETS.iter().enumerate() {
                let mark = if i == c.choice { "▸" } else { " " };
                let style = if i == c.choice {
                    theme::title()
                } else {
                    theme::body()
                };
                tip_lines.push(Line::from(Span::styled(
                    format!("{mark} {}", crate::app::tip_label(preset)),
                    style,
                )));
            }
            if let Some(rt) = c.router_tip {
                tip_lines.push(Line::from(""));
                tip_lines.push(Line::from(Span::styled(
                    format!("router tip floor · {rt} lamports (Jupiter/Titan)"),
                    theme::muted(),
                )));
            }
            tip_lines.push(Line::from(""));
            tip_lines.push(Line::from(Span::styled(
                "no simulation yet — choose fee then Enter",
                theme::muted(),
            )));
            frame.render_widget(
                Paragraph::new(tip_lines).block(panel("↑↓ · Enter simulate", false)),
                chunks[1],
            );
            draw_footer(frame, app, chunks[2]);
            return;
        }

        let log_title = match c.phase {
            crate::write_flow::ConfirmPhase::Failed => "simulation error · ↑↓ scroll",
            crate::write_flow::ConfirmPhase::Done => "result",
            _ => "simulation logs · ↑↓ scroll",
        };
        let mut log_lines: Vec<Line> = Vec::new();
        match c.phase {
            crate::write_flow::ConfirmPhase::Failed => {
                for line in &c.error_lines {
                    log_lines.push(Line::from(Span::styled(line.clone(), theme::danger())));
                }
                for log in &c.sim_preview {
                    log_lines.push(Line::from(Span::styled(log.clone(), theme::dim())));
                }
            }
            crate::write_flow::ConfirmPhase::Done => {
                if let Some(sig) = c.signature.as_deref() {
                    log_lines.push(Line::from(Span::styled(format!("tx {sig}"), theme::body())));
                }
                log_lines.push(Line::from(Span::styled(
                    "p · full pubkey / tx in status · Enter / Esc back",
                    theme::muted(),
                )));
            }
            _ => {
                for log in &c.sim_preview {
                    log_lines.push(Line::from(Span::styled(log.clone(), theme::dim())));
                }
            }
        }
        let scroll = app.confirm_scroll;
        frame.render_widget(
            Paragraph::new(log_lines)
                .scroll((scroll, 0))
                .block(panel(log_title, false)),
            chunks[1],
        );
        draw_footer(frame, app, chunks[2]);
    }
}

fn draw_write_consent(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(area);
    let intro = Paragraph::new(vec![
        Line::from(Span::styled(
            "Writes are a solid POC — burner / dust only (not production size).",
            theme::warn(),
        )),
        Line::from(Span::styled(
            "AUTOSWAP (Jupiter/Titan) does NOT work as intended — prefer no-swap.",
            theme::warn(),
        )),
        Line::from(Span::styled(
            "Claim / deposit / wrap paths look good as POC · always read sim logs.",
            theme::dim(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "tx v1 · simulate always · keypair path may be saved (never secret bytes).",
            theme::dim(),
        )),
        Line::from(Span::styled(
            "SOL as native · we wrap/unwrap · Jupiter wrapAndUnwrapSol=false.",
            theme::dim(),
        )),
    ])
    .block(panel("allow writes? · POC", false));
    frame.render_widget(intro, chunks[0]);

    let items: Vec<ListItem> = App::write_consent_choices()
        .iter()
        .map(|s| ListItem::new(Line::from(Span::styled(format!("  {s}"), theme::body()))))
        .collect();
    let mut state = ListState::default().with_selected(Some(app.setup.selected.min(2)));
    let list = List::new(items)
        .block(panel("↑↓ · Enter", false))
        .highlight_symbol("▸ ")
        .highlight_style(theme::highlight());
    frame.render_stateful_widget(list, chunks[1], &mut state);
    draw_footer(frame, app, chunks[2]);
}

fn panel<'a>(title: &'a str, warn: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(if warn {
            theme::border_warn()
        } else {
            theme::border()
        })
        .title(Span::styled(format!(" {title} "), theme::title()))
        .style(theme::body())
}

fn draw_db_setup(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(6),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    let default_disp = display_path(&app.db.default_path);
    let intro = Paragraph::new(vec![
        Line::from(Span::styled("sol-clmm-tui", theme::title())),
        Line::from(""),
        Line::from(Span::styled(
            "Local SQLite stores wallet, RPC, settings, and token symbols/decimals.",
            theme::muted(),
        )),
        Line::from(Span::styled(
            format!("Suggested (macOS/Linux): {default_disp}"),
            theme::dim(),
        )),
    ])
    .block(panel("database · first run", true));
    frame.render_widget(intro, chunks[0]);

    match app.db.phase {
        DbSetupPhase::ChooseLocation => {
            let mut items: Vec<ListItem> = DbPathChoice::ALL
                .iter()
                .map(|c| {
                    ListItem::new(Line::from(Span::styled(
                        format!("  {}", c.label(&default_disp)),
                        theme::body(),
                    )))
                })
                .collect();
            if let Some(err) = &app.db.error {
                items.push(ListItem::new(Line::from("")));
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("error: {err}"),
                    theme::danger(),
                ))));
            }
            let mut state = ListState::default().with_selected(Some(app.db.selected));
            let list = List::new(items)
                .block(panel("↑↓ choose · Enter", false))
                .highlight_symbol("▸ ")
                .highlight_style(theme::highlight());
            frame.render_stateful_widget(list, chunks[1], &mut state);
        }
        DbSetupPhase::CustomInput => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "SQLite file path (directories are created)",
                    theme::muted(),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("▸ ", theme::input_cursor()),
                    Span::styled(app.db.input.clone(), theme::body()),
                    Span::styled("█", theme::accent_blink()),
                ]),
            ];
            if let Some(err) = &app.db.error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("error: {err}"),
                    theme::danger(),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines).block(panel("custom path", false)),
                chunks[1],
            );
        }
        DbSetupPhase::ConfirmExist => {
            let path = app
                .db
                .pending_path
                .as_ref()
                .map(|p| display_path(p))
                .unwrap_or_default();
            let header = Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("SQLite already at {path}"),
                    theme::warn(),
                )),
                Line::from(Span::styled(
                    "Load it (keep wallet / RPC / cache) or overwrite. Esc → pick another path.",
                    theme::muted(),
                )),
            ]);
            let mid = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(4)])
                .split(chunks[1]);
            frame.render_widget(header.block(panel("load database", false)), mid[0]);
            let items: Vec<ListItem> = DbExistChoice::ALL
                .iter()
                .map(|c| {
                    ListItem::new(Line::from(Span::styled(
                        format!("  {}", c.label()),
                        theme::body(),
                    )))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(app.db.selected));
            let list = List::new(items)
                .block(panel("↑↓ · Enter · Esc other path", false))
                .highlight_symbol("▸ ")
                .highlight_style(theme::highlight());
            frame.render_stateful_widget(list, mid[1], &mut state);
        }
    }

    draw_footer(frame, app, chunks[2]);
}

fn draw_setup(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(6),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    let intro = Paragraph::new(vec![
        Line::from(Span::styled("sol-clmm-tui", theme::title())),
        Line::from(""),
        Line::from(Span::styled(
            "Wallet or keypair was not provided.",
            theme::warn(),
        )),
        Line::from(Span::styled(
            "Choose how to identify the wallet for read access.",
            theme::muted(),
        )),
    ])
    .block(panel("setup · WIP · funds at risk", true));
    frame.render_widget(intro, chunks[0]);

    match app.setup.step {
        SetupStep::Choose => {
            let items: Vec<ListItem> = WalletSource::ALL
                .iter()
                .map(|s| {
                    ListItem::new(Line::from(Span::styled(
                        format!("  {}", s.label()),
                        theme::body(),
                    )))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(app.setup.selected));
            let list = List::new(items)
                .block(panel("↑↓ choose · Enter", false))
                .highlight_symbol("▸ ")
                .highlight_style(theme::highlight());
            frame.render_stateful_widget(list, chunks[1], &mut state);
        }
        SetupStep::Input => {
            let source = app.setup.wallet_source();
            let mut lines = vec![
                Line::from(Span::styled(source.prompt(), theme::muted())),
                Line::from(""),
                Line::from(vec![
                    Span::styled("▸ ", theme::input_cursor()),
                    Span::styled(app.setup.input.clone(), theme::body()),
                    Span::styled("█", theme::accent_blink()),
                ]),
            ];
            if let Some(err) = &app.setup.error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("error: {err}"),
                    theme::danger(),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines).block(panel(source.label(), false)),
                chunks[1],
            );
        }
    }

    draw_footer(frame, app, chunks[2]);
}

fn draw_rpc_offer(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(6),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    let intro = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("wallet ", theme::header_key()),
            Span::styled(app.format_wallet(), theme::header_val()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Public Solana RPC detected — it works, but is slow and rate-limited.",
            theme::warn(),
        )),
        Line::from(Span::styled(
            "A private RPC (Helius, Triton, QuickNode, …) loads much better.",
            theme::muted(),
        )),
        Line::from(Span::styled(
            format!("current: {}", app.format_rpc()),
            theme::dim(),
        )),
    ])
    .block(panel("rpc · performance", true));
    frame.render_widget(intro, chunks[0]);

    match app.rpc_offer.step {
        SetupStep::Choose => {
            let items: Vec<ListItem> = RpcChoice::ALL
                .iter()
                .map(|c| {
                    ListItem::new(Line::from(Span::styled(
                        format!("  {}", c.label()),
                        theme::body(),
                    )))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(app.rpc_offer.selected));
            let list = List::new(items)
                .block(panel("↑↓ choose · Enter", false))
                .highlight_symbol("▸ ")
                .highlight_style(theme::highlight());
            frame.render_stateful_widget(list, chunks[1], &mut state);
        }
        SetupStep::Input => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "paste private HTTPS RPC URL, then Enter",
                    theme::muted(),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("▸ ", theme::input_cursor()),
                    Span::styled(app.rpc_offer.input.clone(), theme::body()),
                    Span::styled("█", theme::accent_blink()),
                ]),
            ];
            if let Some(err) = &app.rpc_offer.error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("error: {err}"),
                    theme::danger(),
                )));
            }
            frame.render_widget(
                Paragraph::new(lines).block(panel("private RPC", false)),
                chunks[1],
            );
        }
    }

    draw_footer(frame, app, chunks[2]);
}

fn draw_settings(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(6),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    let intro = Paragraph::new(vec![
        Line::from(Span::styled("settings", theme::title())),
        Line::from(Span::styled(
            format!("wallet {} · rpc {}", app.format_wallet(), app.format_rpc()),
            theme::muted(),
        )),
        Line::from(Span::styled(
            format!("sqlite {}", app.db_path_display()),
            theme::dim(),
        )),
        Line::from(Span::styled(
            "privacy trim hides full pubkey in the UI (first 4 chars).",
            theme::dim(),
        )),
    ])
    .block(panel("settings", false));
    frame.render_widget(intro, chunks[0]);

    let items: Vec<ListItem> = SettingsItem::ALL
        .iter()
        .map(|item| {
            ListItem::new(Line::from(Span::styled(
                format!(
                    "  {}",
                    item.label(
                        app.privacy_trim,
                        app.price_mode,
                        app.jupiter_api_key.is_some(),
                        app.titan_api_key.is_some(),
                        app.api_paid_fast,
                        &app.swap_mode,
                        &app.budget_mode,
                        app.tip_lamports,
                        app.write_allowed,
                        &app.dlmm_strategy,
                    )
                ),
                theme::body(),
            )))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.settings_selected));
    let list = List::new(items)
        .block(panel("↑↓ · Enter", false))
        .highlight_symbol("▸ ")
        .highlight_style(theme::highlight());
    frame.render_stateful_widget(list, chunks[1], &mut state);

    draw_footer(frame, app, chunks[2]);
}

fn draw_settings_api_key(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(FOOTER_H)])
        .split(area);

    let (title, hint) = match app.api_key_target {
        ApiKeyTarget::Jupiter => (
            "Jupiter API key",
            "portal · api.jup.ag (Price + Swap v2 /build) · empty = keyless throttle · developers.jup.ag",
        ),
        ApiKeyTarget::Titan => (
            "Titan API key",
            "Portal needs a key (Sandbox free) · developer.titan.exchange · empty clears",
        ),
    };

    let mut lines = vec![
        Line::from(Span::styled(hint, theme::muted())),
        Line::from(Span::styled(
            "stored in local sqlite · never sent except to that API",
            theme::dim(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("▸ ", theme::input_cursor()),
            Span::styled(app.settings_api_key.input.clone(), theme::body()),
            Span::styled("█", theme::accent_blink()),
        ]),
    ];
    if let Some(err) = &app.settings_api_key.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            theme::danger(),
        )));
    }
    frame.render_widget(Paragraph::new(lines).block(panel(title, false)), chunks[0]);
    draw_footer(frame, app, chunks[1]);
}

fn draw_settings_rpc(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(FOOTER_H)])
        .split(area);

    let mut lines = vec![
        Line::from(Span::styled("paste new HTTPS RPC URL", theme::muted())),
        Line::from(""),
        Line::from(vec![
            Span::styled("▸ ", theme::input_cursor()),
            Span::styled(app.settings_rpc.input.clone(), theme::body()),
            Span::styled("█", theme::accent_blink()),
        ]),
    ];
    if let Some(err) = &app.settings_rpc.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            theme::danger(),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel("change RPC", false)),
        chunks[0],
    );
    draw_footer(frame, app, chunks[1]);
}

fn draw_help(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);

    let lines = help_lines();
    let scroll = app.help_scroll as usize;
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(scroll)
        .map(|(style_kind, text)| {
            let style = match style_kind {
                HelpStyle::Title => theme::title(),
                HelpStyle::Key => theme::accent_text(),
                HelpStyle::Body => theme::body(),
                HelpStyle::Dim => theme::dim(),
                HelpStyle::Warn => theme::warn(),
            };
            Line::from(Span::styled(text, style))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(visible)
            .block(panel("help · ↑↓ scroll · Esc / ? / h back", false))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        chunks[1],
    );
    draw_footer(frame, app, chunks[2]);
}

#[derive(Clone, Copy)]
enum HelpStyle {
    Title,
    Key,
    Body,
    Dim,
    Warn,
}

fn help_lines() -> Vec<(HelpStyle, String)> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<(HelpStyle, String)>, s: HelpStyle, t: &str| {
        out.push((s, t.to_string()));
    };

    push(
        &mut out,
        HelpStyle::Warn,
        "Unofficial · WIP · not financial advice · funds at risk",
    );
    push(
        &mut out,
        HelpStyle::Dim,
        "Fees / amounts / ≈USD / volume are estimates. Cross-check before acting.",
    );
    push(&mut out, HelpStyle::Body, "");

    push(&mut out, HelpStyle::Title, "Navigation");
    push(
        &mut out,
        HelpStyle::Key,
        "  ↑↓ / j k     move selection or scroll",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  Enter        confirm / open position detail",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  Esc          back (or quit from positions list)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  q            quit (or back from detail / settings / help)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  ? / h        this help (from list, detail, or settings)",
    );
    push(&mut out, HelpStyle::Body, "");

    push(&mut out, HelpStyle::Title, "Positions list");
    push(
        &mut out,
        HelpStyle::Key,
        "  r            refresh list · prices · pool snapshots",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  v            cycle venue filter (all → Orca → Raydium → Meteora)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  s            settings (RPC, wallet, USD price mode, API keys)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  p            full position pubkey in status (again to hide)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  y            write history (all writes from list · this position from detail)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  w            wallet (write) · load bar → wrap / unwrap / swap / create pool",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  n            create position (write) · paste pool + range",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Columns: venue · pool · ≈ position USD · pool TVL · claim-sim fees · tick/bin range.",
    );
    push(
        &mut out,
        HelpStyle::Dim,
        "  Fees are claim-simulated (owed + fee-growth), not the stale on-chain owed field alone.",
    );
    push(&mut out, HelpStyle::Body, "");

    push(&mut out, HelpStyle::Title, "Position detail");
    push(
        &mut out,
        HelpStyle::Key,
        "  r            refresh this position (fees, rewards, chart, snapshot)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  y            history for this position only",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  p            full position + pool pubkey (copy from status)",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  + / -        zoom in (tighter) / out (wider) · starts full-width",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  0            fit chart to position range",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  m            manage (write) · deposit / compound / claim / withdraw / close",
    );
    push(&mut out, HelpStyle::Key, "  Esc / q      back to list");
    push(
        &mut out,
        HelpStyle::Body,
        "  Shows balances, claimable fees + rewards, ≈ USD, vault TVL, liquidity chart.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Chart: colored = in-range depth · dim = outside · ┃ = current · ▌▐ = bounds.",
    );
    push(
        &mut out,
        HelpStyle::Dim,
        "  Status shows zoom min/max when you hit the fetched window edge.",
    );
    push(&mut out, HelpStyle::Body, "");

    push(&mut out, HelpStyle::Title, "≈ Volume (SQLite snapshots)");
    push(
        &mut out,
        HelpStyle::Body,
        "  Each list/detail refresh stores fee_growth + L + prices for unique pools (full history).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Volume ≈ sum over every consecutive segment: Δfee_growth × mean(L) / fee_rate.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Need ≥2 snapshots — press r more than once over time. ≈24h extrapolates the window rate.",
    );
    push(
        &mut out,
        HelpStyle::Dim,
        "  Rough only — not an indexer. Adaptive fees / hops make it imperfect.",
    );
    push(&mut out, HelpStyle::Body, "");

    push(&mut out, HelpStyle::Title, "Settings · USD pricing");
    push(
        &mut out,
        HelpStyle::Body,
        "  on-chain  — Orca / Raydium / Meteora hops vs stables / SOL (no HTTPS).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  jupiter   — Price API (optional key; free tier throttled).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  titan     — Portal quotes (API key required).",
    );
    push(&mut out, HelpStyle::Body, "");
    push(
        &mut out,
        HelpStyle::Title,
        "Writes — solid POC · burner wallet",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  Smoke so far: open on Raydium + Meteora · SOL/USDC only. Other pools / create still TBD.",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  Claim / deposit / withdraw / close / create + wallet wrap are POC · prefer no-swap.",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  P0: correct autoswap · compound live · reward≠A/B → opt-in swap into deposit.",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  P1 final boss: rebalance / move OOR → close → swap → open in range.",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  P2: history (y) · pool ATA Δ + wallet swap from→to · richer plan notes still TODO.",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  Dust only · read sim logs · read-only build if unsure (no --features write).",
    );
    push(&mut out, HelpStyle::Body, "");
    push(
        &mut out,
        HelpStyle::Title,
        "Writes (cargo --features write)",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  tx v1 only · CU + tip live in message config (no ComputeBudget ixs on explorers).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  priority fee: ↑↓ on confirm tip screen · OFF / fixed / auto. Enter simulates.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  SQLite may store keypair path + pubkey — never secret key bytes.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  SOL: wrap at start · Jupiter wrapAndUnwrapSol=false · unwrap/close WSOL at end.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  DLMM StrategyType: Spot/Curve/BidAsk × OneSide|Balanced (ImBalanced TODO).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Detail: m manage → deposit/compound/claim/withdraw/close (wrap lives under w).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Create: n · venue · pool pubkey · range chart · side/amount · review.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Wallet: w · load bar → wrap/unwrap · Jupiter/Titan swap · create pool (Meteora stub).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  History Δ: wallet swap shows from→to (e.g. USDC − · SOL +); Esc cancels load.",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  Range step: ←→ nudge · Tab lower/upper · H/L shift both · 1/2/3 = ±5/10/25%.",
    );
    push(
        &mut out,
        HelpStyle::Key,
        "  Range step: starts full-width · + tighter · - wider · 0 fit · mix = % by value.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Deposit: 2-side or 1-side · 4/5/6 = 25/50/max · out-of-range correct side = no swap.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Out-of-range wrong side = full swap; in-range 1-side = proportional ratio swap.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  1-side USDC: no SOL wrap · autoswap asks slippage (RTSE / default 50 / change bps).",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Privacy trim shortens wallet / private RPC in the header. Change RPC or wallet anytime.",
    );
    push(&mut out, HelpStyle::Body, "");

    push(&mut out, HelpStyle::Title, "Wallet · RPC tips");
    push(
        &mut out,
        HelpStyle::Body,
        "  Pubkey-only = reads. --keypair PATH may be saved (path only). Writes need --features write.",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Public RPC works but rate-limits; a private HTTPS RPC is faster for multi-venue refresh.",
    );
    push(
        &mut out,
        HelpStyle::Dim,
        "  Footer shows session RPC / Jupiter / Titan call counts and SQLite DB size.",
    );
    push(&mut out, HelpStyle::Body, "");

    push(&mut out, HelpStyle::Title, "First run");
    push(
        &mut out,
        HelpStyle::Body,
        "  1) Choose SQLite path  2) wallet pubkey or keypair  3) keep public RPC or paste private",
    );
    push(
        &mut out,
        HelpStyle::Body,
        "  Then wait for the list load bar. Open a row with Enter. Press ? anytime for this screen.",
    );

    out
}

fn draw_positions_screen(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);
    draw_positions(frame, app, chunks[1]);
    draw_footer(frame, app, chunks[2]);
}

fn draw_history_screen(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);
    draw_history(frame, app, chunks[1]);
    draw_footer(frame, app, chunks[2]);
}

fn draw_detail(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(15),
            Constraint::Min(6),
            Constraint::Length(FOOTER_H),
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);

    if app.detail_loading && app.detail.is_none() {
        draw_panel_load(
            frame,
            chunks[1],
            app.load_bar_ratio(),
            "position",
            "refreshing this position…",
        );
        frame.render_widget(
            Paragraph::new(Span::styled("waiting for position…", theme::dim()))
                .block(panel("liquidity distribution", false)),
            chunks[2],
        );
        draw_footer(frame, app, chunks[3]);
        return;
    }

    if let Some(err) = &app.detail_error {
        frame.render_widget(
            Paragraph::new(Span::styled(err.clone(), theme::danger()))
                .block(panel("position · error", true)),
            chunks[1],
        );
        frame.render_widget(
            Paragraph::new("").block(panel("liquidity distribution", false)),
            chunks[2],
        );
        draw_footer(frame, app, chunks[3]);
        return;
    }

    let Some(d) = &app.detail else {
        frame.render_widget(
            Paragraph::new("no detail").block(panel("position", false)),
            chunks[1],
        );
        draw_footer(frame, app, chunks[3]);
        return;
    };

    let fee_bps = d.pool.fee_rate as f64 / 100.0;
    let range = if d.in_range {
        Span::styled("IN RANGE", theme::ok())
    } else {
        Span::styled("OUT OF RANGE", theme::warn())
    };

    let pair = format!("{} / {}", d.token_a.symbol, d.token_b.symbol);

    let stats = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("venue ", theme::header_key()),
            Span::styled(d.row.venue.full(), theme::accent_text()),
            Span::styled("  · ", theme::dim()),
            Span::styled(d.row.venue.short(), theme::muted()),
        ]),
        Line::from(vec![
            Span::styled("position ", theme::header_key()),
            Span::styled(short_pk(&d.row.address), theme::accent_text()),
            Span::styled("  ", theme::dim()),
            range,
        ]),
        Line::from(vec![
            Span::styled("pool ", theme::header_key()),
            Span::styled(pair, theme::body()),
            Span::styled(
                format!(
                    "  fee {:.2} bps  {} {}",
                    fee_bps,
                    d.pool.index_space.label(),
                    d.pool.tick_spacing
                ),
                theme::muted(),
            ),
        ]),
        Line::from(vec![
            Span::styled("mints ", theme::header_key()),
            Span::styled(
                format!(
                    "{} · {}",
                    short_pk(&d.token_a.mint),
                    short_pk(&d.token_b.mint)
                ),
                theme::dim(),
            ),
        ]),
        Line::from(vec![
            Span::styled("in position ", theme::header_key()),
            Span::styled(
                format!("{}  +  {}", d.amount_a_ui, d.amount_b_ui),
                theme::ok(),
            ),
            Span::styled(
                if sol_clmm_manager::WRITE_ENABLED && app.writes_live() {
                    "  · m manage"
                } else {
                    ""
                },
                theme::muted(),
            ),
        ]),
        Line::from({
            let wallet_line = d
                .wallet_balances
                .as_ref()
                .map(|b| {
                    b.summary_line(
                        &d.token_a.symbol,
                        &d.token_b.symbol,
                        d.token_a.decimals,
                        d.token_b.decimals,
                    )
                })
                .unwrap_or_else(|| "—".into());
            vec![
                Span::styled("wallet ", theme::header_key()),
                Span::styled(wallet_line, theme::body()),
            ]
        }),
        Line::from(vec![
            Span::styled("fees (claim) ", theme::header_key()),
            Span::styled(format!("{}  +  {}", d.fee_a_ui, d.fee_b_ui), theme::warn()),
            Span::styled(format!("  · Σ {}", format_usd(d.fees_usd)), theme::dim()),
        ]),
        Line::from({
            if d.rewards.is_empty() {
                vec![
                    Span::styled("rewards ", theme::header_key()),
                    Span::styled("none", theme::dim()),
                ]
            } else {
                let parts: Vec<String> = d.rewards.iter().map(|r| r.amount_ui.clone()).collect();
                vec![
                    Span::styled("rewards ", theme::header_key()),
                    Span::styled(parts.join("  +  "), theme::warn()),
                    Span::styled(format!("  · Σ {}", format_usd(d.rewards_usd)), theme::dim()),
                ]
            }
        }),
        Line::from({
            match &d.usd {
                Some(u) => vec![
                    Span::styled("≈ position ", theme::header_key()),
                    Span::styled(format_usd(u.position_usd), theme::ok()),
                    Span::styled(
                        format!(
                            "  · px {}={} {}={} · {}",
                            d.token_a.symbol,
                            format_usd(u.price_a.price),
                            d.token_b.symbol,
                            format_usd(u.price_b.price),
                            u.note
                        ),
                        theme::dim(),
                    ),
                ],
                None => vec![
                    Span::styled("≈ position ", theme::header_key()),
                    Span::styled("— · fetch prices / change mode in settings", theme::dim()),
                ],
            }
        }),
        Line::from({
            match &d.usd {
                Some(u) => vec![
                    Span::styled("≈ pool TVL ", theme::header_key()),
                    Span::styled(format_usd(u.pool_tvl_usd), theme::ok()),
                    Span::styled(
                        format!(
                            "  = vault {} + {}",
                            format_usd(u.pool_vault_a_usd),
                            format_usd(u.pool_vault_b_usd),
                        ),
                        theme::muted(),
                    ),
                ],
                None => vec![
                    Span::styled("≈ pool TVL ", theme::header_key()),
                    Span::styled("—", theme::dim()),
                ],
            }
        }),
        Line::from({
            match &d.period {
                Some(p) => {
                    let mut spans = vec![
                        Span::styled("≈ volume ", theme::header_key()),
                        Span::styled(format_usd(p.volume_usd), theme::ok()),
                    ];
                    if let Some(v24) = p.volume_usd_24h {
                        spans.push(Span::styled(
                            format!("  · ≈24h {}", format_usd(Some(v24))),
                            theme::muted(),
                        ));
                    }
                    spans.push(Span::styled(
                        format!(
                            "  · fees {} + {} · {}",
                            p.fees_a_ui, p.fees_b_ui, p.volume_note
                        ),
                        theme::dim(),
                    ));
                    spans
                }
                None => vec![
                    Span::styled("≈ volume ", theme::header_key()),
                    Span::styled("—", theme::dim()),
                    Span::styled(
                        "  · need ≥2 snapshots · r on list / reopen detail builds the chain",
                        theme::dim(),
                    ),
                ],
            }
        }),
        Line::from(vec![
            Span::styled("vaults ", theme::header_key()),
            Span::styled(
                format!("{}  +  {}", d.vault_a_ui, d.vault_b_ui),
                theme::muted(),
            ),
            Span::styled("  · p = full pubkey · p again hides", theme::dim()),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{} ", d.pool.index_space.label()),
                theme::header_key(),
            ),
            Span::styled(
                format!("[{}, {}]", d.row.tick_lower, d.row.tick_upper),
                theme::body(),
            ),
            Span::styled("  current ", theme::header_key()),
            Span::styled(d.pool.tick_current_index.to_string(), theme::warn()),
            Span::styled("  L ", theme::header_key()),
            Span::styled(compact_u128(d.position_liquidity), theme::muted()),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{} ", d.pool.index_space.arrays_label()),
                theme::header_key(),
            ),
            Span::styled(
                format!(
                    "{} loaded · {} initialized · graph [{}, {}]",
                    d.arrays_loaded, d.initialized_ticks, d.series_min_tick, d.series_max_tick
                ),
                theme::dim(),
            ),
        ]),
        Line::from(vec![
            Span::styled("legend ", theme::header_key()),
            Span::styled(format!("█ {}", d.token_a.symbol), theme::token_a()),
            Span::styled("  ", theme::dim()),
            Span::styled(format!("█ {}", d.token_b.symbol), theme::token_b()),
            Span::styled("  ", theme::dim()),
            Span::styled("▄▀ both", theme::ok()),
            Span::styled("  · ", theme::dim()),
            Span::styled("┃ current", theme::warn()),
            Span::styled("  · dim = outside position", theme::dim()),
        ]),
    ])
    .block(panel("position · pool", false));
    frame.render_widget(stats, chunks[1]);

    let pool_tvl = d.usd.as_ref().and_then(|u| u.pool_tvl_usd);
    let (view_lo, view_hi) = sol_clmm_manager::chart_view_window(
        d.pool.tick_current_index,
        d.pool.tick_spacing,
        app.detail_chart_half_spacings,
        (d.series_min_tick, d.series_max_tick),
    );
    let (view, view_max) = sol_clmm_manager::slice_liquidity_series(&d.series, view_lo, view_hi);
    draw_liquidity_chart(
        frame,
        chunks[2],
        &view,
        view_max,
        d.pool.liquidity,
        pool_tvl,
        d.pool.index_space,
        d.pool.tick_current_index,
        &d.token_a.symbol,
        &d.token_b.symbol,
        d.token_a.decimals,
        d.token_b.decimals,
        Some(d.row.tick_lower),
        Some(d.row.tick_upper),
        "",
        app.detail_chart_half_spacings,
    );

    draw_footer(frame, app, chunks[3]);
}

/// Scaled bar chart: Y = liquidity (≈ USD when TVL known), X = human B/A price.
/// Optional `range_lo`/`range_hi` draw bound markers; `focus` labels which edge is active.
#[allow(clippy::too_many_arguments)]
fn draw_liquidity_chart(
    frame: &mut Frame,
    area: Rect,
    series: &[LiquiditySample],
    max_l: u64,
    pool_liquidity: u128,
    pool_tvl_usd: Option<f64>,
    index_space: IndexSpace,
    current_tick: i32,
    sym_a: &str,
    sym_b: &str,
    decimals_a: u8,
    decimals_b: u8,
    range_lo: Option<i32>,
    range_hi: Option<i32>,
    focus: &str,
    zoom_half_spacings: i32,
) {
    let y_usd = pool_tvl_usd.is_some();
    let idx_lab = index_space.label();
    // USD mode: full chart height = pool TVL (active L). Series peaks above active L
    // are clipped — never invent a Y max like TVL × (max_bar / pool_L).
    let pool_l_scaled = scale_u128(pool_liquidity.max(1)).max(1) as f64;
    let title = if y_usd {
        format!(
            "liquidity · Y ≈ USD · X = {sym_b}/{sym_a} · {idx_lab} · zoom ±{zoom_half_spacings} · +/-/0"
        )
    } else {
        format!(
            "liquidity · Y = relative depth · X = {sym_b}/{sym_a} · {idx_lab} · zoom ±{zoom_half_spacings} · +/-/0"
        )
    };
    let block = panel(&title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if series.is_empty() || inner.width < 8 || inner.height < 4 {
        frame.render_widget(
            Paragraph::new(Span::styled("no liquidity series", theme::dim())),
            inner,
        );
        return;
    }

    let y_axis_w = if y_usd { 9u16 } else { 10u16 };
    let x_axis_h = 2u16;
    let chart_w = inner.width.saturating_sub(y_axis_w);
    let chart_h = inner.height.saturating_sub(x_axis_h);
    if chart_w == 0 || chart_h == 0 {
        return;
    }

    let chart = Rect {
        x: inner.x + y_axis_w,
        y: inner.y,
        width: chart_w,
        height: chart_h,
    };
    let y_lbl = Rect {
        x: inner.x,
        y: inner.y,
        width: y_axis_w,
        height: chart_h,
    };
    let x_lbl = Rect {
        x: chart.x,
        y: chart.y + chart_h,
        width: chart_w,
        height: x_axis_h,
    };

    frame.render_widget(Clear, chart);

    let max_l = max_l.max(1) as f64;
    // Bar denominator: active pool L in USD mode; series max otherwise.
    let bar_denom = if y_usd { pool_l_scaled.max(1.0) } else { max_l };
    let n = series.len();
    let cols = chart_w as usize;
    // Column closest to a tick/bin index.
    let col_for = |target: i32| -> Option<usize> {
        if n == 0 || cols == 0 {
            return None;
        }
        let mut best_i = 0usize;
        let mut best_d = i32::MAX;
        for (i, s) in series.iter().enumerate() {
            let d = (s.tick - target).abs();
            if d < best_d {
                best_d = d;
                best_i = i;
            }
        }
        Some(if cols == 1 {
            0
        } else {
            best_i * (cols - 1) / (n.saturating_sub(1).max(1))
        })
    };
    let current_col = col_for(current_tick);
    let lo_col = range_lo.and_then(col_for);
    let hi_col = range_hi.and_then(col_for);
    // map each column to a sample
    for col in 0..cols {
        let idx = if cols == 1 {
            0
        } else {
            col * (n.saturating_sub(1)) / (cols - 1)
        };
        let sample = &series[idx.min(n - 1)];
        let h = ((sample.liquidity as f64 / bar_denom) * chart_h as f64).round() as u16;
        let h = h.clamp(1, chart_h);
        let is_current = current_col == Some(col);
        for row in 0..h {
            let y = chart.y + chart_h - 1 - row;
            let x = chart.x + col as u16;
            let cell = Rect {
                x,
                y,
                width: 1,
                height: 1,
            };
            let style = if is_current {
                theme::warn()
            } else {
                match sample.side {
                    TokenSide::A => theme::token_a(),
                    TokenSide::B => theme::token_b(),
                    TokenSide::Both => {
                        if row < h / 2 {
                            theme::token_b()
                        } else {
                            theme::token_a()
                        }
                    }
                    TokenSide::Outside => theme::outside(),
                }
            };
            let ch = if is_current {
                "┃"
            } else if sample.side == TokenSide::Both && row == h / 2 {
                "▄"
            } else {
                "█"
            };
            frame.render_widget(Paragraph::new(Span::styled(ch, style)), cell);
        }
        // Extend current-price marker to full chart height.
        if is_current {
            for row in h..chart_h {
                let y = chart.y + chart_h - 1 - row;
                frame.render_widget(
                    Paragraph::new(Span::styled("│", theme::warn())),
                    Rect {
                        x: chart.x + col as u16,
                        y,
                        width: 1,
                        height: 1,
                    },
                );
            }
        }
        // Range bounds: ▌ lower · ▐ upper (accent when focused).
        let is_lo = lo_col == Some(col) && !is_current;
        let is_hi = hi_col == Some(col) && !is_current;
        if is_lo || is_hi {
            let focused = (is_lo && focus == "lower") || (is_hi && focus == "upper");
            let style = if focused {
                theme::accent_text()
            } else {
                theme::muted()
            };
            let ch = if is_lo { "▌" } else { "▐" };
            for row in 0..chart_h {
                let y = chart.y + chart_h - 1 - row;
                frame.render_widget(
                    Paragraph::new(Span::styled(ch, style)),
                    Rect {
                        x: chart.x + col as u16,
                        y,
                        width: 1,
                        height: 1,
                    },
                );
            }
        }
    }

    // Y scale: pin max / mid / 0 to top / middle / bottom (USD when TVL known).
    let y_w = y_axis_w as usize;
    let pad = |s: String| {
        if s.len() >= y_w {
            s.chars().take(y_w).collect::<String>()
        } else {
            format!("{s:>width$}", width = y_w)
        }
    };
    let (top, mid, bot) = if let Some(tvl) = pool_tvl_usd {
        // Axis top = reported pool TVL (matches vault line above), not inflated series peaks.
        let top_u = if tvl.is_finite() && tvl > 0.0 {
            tvl
        } else {
            0.0
        };
        let mid_u = top_u / 2.0;
        (
            pad(format_usd_axis(top_u)),
            pad(format_usd_axis(mid_u)),
            pad("$0".into()),
        )
    } else {
        // Raw CLMM L is not human money — show relative depth vs peak bar.
        (pad("100%".into()), pad("50%".into()), pad("0%".into()))
    };
    let mid_row = (chart_h / 2) as usize;
    let bot_row = chart_h.saturating_sub(1) as usize;
    for row in 0..chart_h as usize {
        let text = if row == 0 {
            top.as_str()
        } else if row == mid_row {
            mid.as_str()
        } else if row == bot_row {
            bot.as_str()
        } else {
            continue;
        };
        let style = if row == 0 {
            theme::muted()
        } else {
            theme::dim()
        };
        frame.render_widget(
            Paragraph::new(Span::styled(text, style)),
            Rect {
                x: y_lbl.x,
                y: y_lbl.y + row as u16,
                width: y_axis_w,
                height: 1,
            },
        );
    }

    // X scale: left / current / right — human B per A (decimal-adjusted).
    let left = series.first().unwrap();
    let right = series.last().unwrap();
    let left_p = format_price(index_space.human_price_at(left.tick, decimals_a, decimals_b));
    let cur_p = format_price(index_space.human_price_at(current_tick, decimals_a, decimals_b));
    let right_p = format_price(index_space.human_price_at(right.tick, decimals_a, decimals_b));
    let w = (chart_w as usize / 3).max(1);
    let scale_line = format!("{left_p:<w$}{cur_p:^w$}{right_p:>w$}");
    let bound_hint = match (range_lo, range_hi) {
        (Some(lo), Some(hi)) => {
            let lo_p = format_price(index_space.human_price_at(lo, decimals_a, decimals_b));
            let hi_p = format_price(index_space.human_price_at(hi, decimals_a, decimals_b));
            format!("  · ▌{lo_p}…{hi_p}▐")
        }
        _ => String::new(),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{sym_b} per {sym_a}"), theme::dim()),
                Span::styled("  · ┃ = current", theme::warn()),
                Span::styled(bound_hint, theme::muted()),
            ]),
            Line::from(Span::styled(scale_line, theme::muted())),
        ]),
        x_lbl,
    );
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let wallet = app.format_wallet();

    let (slot, bal) = match &app.status {
        Some(s) => {
            let bal = s
                .balance_lamports
                .map(|l| format!("{:.4} SOL", lamports_to_sol(l)))
                .unwrap_or_else(|| "—".into());
            (s.slot.to_string(), bal)
        }
        None => ("…".into(), "…".into()),
    };

    let rpc_tag = if rpc::is_public_rpc(&app.rpc_url) {
        Span::styled("public", theme::warn())
    } else {
        Span::styled("private", theme::ok())
    };

    let loading = app.list_loading
        || app.detail_loading
        || app.pending != PendingWork::None
        || app.status_line.starts_with("loading")
        || app.status_line.starts_with("refreshing");

    let line1 = Line::from(vec![
        Span::styled("sol-clmm", theme::title()),
        Span::styled(" · ", theme::dim()),
        Span::styled("rpc ", theme::header_key()),
        Span::styled(app.format_rpc(), theme::header_val()),
        Span::styled(" (", theme::dim()),
        rpc_tag,
        Span::styled(")", theme::dim()),
        Span::styled(" · ", theme::dim()),
        Span::styled("slot ", theme::header_key()),
        Span::styled(slot, theme::header_val()),
        Span::styled(" · ", theme::dim()),
        Span::styled("bal ", theme::header_key()),
        Span::styled(bal, theme::ok()),
        if loading {
            Span::styled("  ◌ loading", theme::warn())
        } else {
            Span::raw("")
        },
    ]);

    let line2 = Line::from(vec![
        Span::styled("wallet ", theme::header_key()),
        Span::styled(wallet, theme::muted()),
        if app.privacy_trim {
            Span::styled("  [privacy]", theme::warn())
        } else {
            Span::raw("")
        },
    ]);

    frame.render_widget(
        Paragraph::new(vec![line1, line2])
            .block(panel("WIP · not financial advice · funds at risk", true)),
        area,
    );
}

fn draw_positions(frame: &mut Frame, app: &mut App, area: Rect) {
    let loading = app.list_loading || matches!(app.pending, PendingWork::List { .. });
    if loading {
        draw_panel_load(
            frame,
            area,
            app.load_bar_ratio(),
            "positions",
            "refreshing list · symbols · prices…",
        );
        return;
    }

    let header = Row::new([
        Cell::from("venue"),
        Cell::from("pool"),
        Cell::from("≈ USD"),
        Cell::from("TVL"),
        Cell::from("fees"),
        Cell::from("range"),
    ])
    .style(theme::table_header());

    let indices = app.filtered_indices();
    let rows: Vec<Row> = if indices.is_empty() {
        let hint = if app.positions.is_empty() {
            "no CLMM / DLMM positions · r refresh".into()
        } else {
            format!(
                "no rows for {} · press v for all venues",
                app.venue_filter.label()
            )
        };
        vec![Row::new([
            Cell::from(Span::styled(app.venue_filter.short(), theme::warn())),
            Cell::from(Span::styled(hint, theme::muted())),
            Cell::from("—"),
            Cell::from("—"),
            Cell::from("—"),
            Cell::from("—"),
        ])]
    } else {
        indices
            .iter()
            .filter_map(|&i| app.positions.get(i))
            .map(|p| {
                let usd = format_usd(p.position_usd);
                let tvl = format_usd(p.pool_tvl_usd);
                // Claim-sim raw amounts (compact); Σ USD when priced.
                let fees_amt = format!(
                    "{}/{}",
                    sol_clmm_manager::format_amount(p.fee_a as u128, p.decimals_a),
                    sol_clmm_manager::format_amount(p.fee_b as u128, p.decimals_b),
                );
                let fees = match p.fees_usd {
                    Some(_) => format!("{} ({})", fees_amt, format_usd(p.fees_usd)),
                    None => fees_amt,
                };
                Row::new([
                    Cell::from(Span::styled(p.venue.full(), theme::accent_text())),
                    Cell::from(p.pool_label.clone()),
                    Cell::from(Span::styled(usd, theme::ok())),
                    Cell::from(Span::styled(tvl, theme::muted())),
                    Cell::from(fees),
                    Cell::from(format!("[{}, {}]", p.tick_lower, p.tick_upper)),
                ])
                .style(theme::body())
            })
            .collect()
    };

    let widths = [
        Constraint::Length(14),
        Constraint::Min(14),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(10),
        Constraint::Length(14),
    ];

    let title = format!(
        "positions · {} · ? help · v venue · y history · r refresh{} · Enter detail",
        app.venue_filter.label(),
        if app.writes_live() {
            " · n create · w wallet"
        } else {
            ""
        },
    );
    let table = Table::new(rows, widths)
        .header(header)
        .block(panel(&title, false))
        .row_highlight_style(theme::highlight())
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn draw_history(frame: &mut Frame, app: &mut App, area: Rect) {
    let scoped = matches!(
        app.history_filter,
        crate::app::HistoryFilter::Position { .. }
    );
    // Always same columns so widths type-check; scoped hides pool as "·".
    // Δ is Length · note is Min → note starts right after Δ (not pushed far right).
    let header = Row::new([
        Cell::from("when"),
        Cell::from("op"),
        Cell::from(if scoped { "·" } else { "pool" }),
        Cell::from("Δ"),
        Cell::from("note"),
    ])
    .style(theme::table_header());

    let widths = [
        Constraint::Length(16),
        Constraint::Length(10),
        Constraint::Length(if scoped { 1 } else { 12 }),
        Constraint::Length(24),
        Constraint::Min(24),
    ];

    let empty_hint = if scoped {
        "no writes for this position yet"
    } else {
        "no writes yet · land a manage/create tx first"
    };

    let rows: Vec<Row> = if app.history.is_empty() {
        vec![Row::new([
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled(empty_hint, theme::muted())),
            Cell::from("—"),
            Cell::from("—"),
            Cell::from("—"),
        ])]
    } else {
        app.history
            .iter()
            .map(|r| {
                let deltas = truncate_history_note(&r.deltas_label(), 24);
                let note = r
                    .summary
                    .as_deref()
                    .or(r.paid_label.as_deref())
                    .unwrap_or("—");
                let note = truncate_history_note(note, 96);
                let pool = if scoped { "·".into() } else { r.pair_label() };
                Row::new([
                    Cell::from(r.time_label()),
                    Cell::from(Span::styled(r.op.clone(), theme::accent_text())),
                    Cell::from(Span::styled(pool, theme::body())),
                    Cell::from(Span::styled(deltas, theme::ok())),
                    Cell::from(Span::styled(note, theme::muted())),
                ])
                .style(theme::body())
            })
            .collect()
    };

    let title = match &app.history_filter {
        crate::app::HistoryFilter::All => format!(
            "history · all · {} · Enter/p sig · r reload · Esc list",
            app.history.len()
        ),
        crate::app::HistoryFilter::Position { label, .. } => format!(
            "history · {label} · {} · Enter/p sig · r reload · Esc detail",
            app.history.len()
        ),
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(panel(&title, false))
        .row_highlight_style(theme::highlight())
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(table, area, &mut app.history_table_state);
}

fn truncate_history_note(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Footer: status/keys + session RPC counter/history (each row needs borders).
const FOOTER_H: u16 = 6;

/// Slow left→right fill inside the panel that is refreshing (wraps and repeats).
fn draw_panel_load(frame: &mut Frame, area: Rect, ratio: f64, title: &str, label: &str) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let filled = if inner_w == 0 {
        0
    } else {
        ((ratio.clamp(0.0, 1.0) * inner_w as f64).round() as usize).min(inner_w)
    };
    let empty = inner_w.saturating_sub(filled);
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(bar, theme::accent_text())),
            Line::from(""),
            Line::from(Span::styled(label, theme::warn())),
        ])
        .block(panel(title, true)),
        area,
    );
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(3)])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            app.status_line.as_str(),
            theme::muted(),
        )))
        .block(panel("keys", false)),
        chunks[0],
    );

    let max_chars = area.width.saturating_sub(4) as usize;
    let rpc_line = app.rpc_meter.footer_line(max_chars.max(16));
    let failed = app.rpc_meter.history().iter().any(|c| !c.ok);
    let db_size = app
        .store
        .as_ref()
        .map(|s| s.file_size_label())
        .unwrap_or_else(|| "—".into());
    let title = format!("{} · db {}", app.rpc_meter.footer_title().trim(), db_size);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            rpc_line,
            if failed { theme::warn() } else { theme::dim() },
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(format!(" {title} "), theme::title()))
                .style(theme::body()),
        ),
        chunks[1],
    );
}

fn short_pk(pk: &solana_pubkey::Pubkey) -> String {
    let s = pk.to_string();
    if s.len() <= 8 {
        s
    } else {
        format!("{}…{}", &s[..4], &s[s.len() - 4..])
    }
}

fn compact_u128(v: u128) -> String {
    if v >= 1_000_000_000_000 {
        format!("{:.2}T", v as f64 / 1e12)
    } else if v >= 1_000_000_000 {
        format!("{:.2}B", v as f64 / 1e9)
    } else if v >= 1_000_000 {
        format!("{:.2}M", v as f64 / 1e6)
    } else {
        v.to_string()
    }
}
