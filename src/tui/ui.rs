//! Rendering for the TUI. Pure functions over [`App`] state.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap,
};
use ratatui::Frame;

use super::{App, ClickRegion, ClickTarget, ConfirmAction, Overlay, SortKey, StepStatus, Tab, UiAction};
use crate::docker::model::{UpdateInfo, UpdateStatus};
use crate::util::format_bytes;

/// Record a clickable region.
fn push_region(regions: &mut Vec<ClickRegion>, x: u16, y: u16, width: u16, target: ClickTarget) {
    regions.push(ClickRegion {
        rect: Rect {
            x,
            y,
            width,
            height: 1,
        },
        target,
    });
}

/// Top-level draw entry point.
pub fn draw(f: &mut Frame, app: &App, regions: &mut Vec<ClickRegion>) {
    let theme = app.theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tabs
            Constraint::Min(1),    // body
            Constraint::Length(2), // footer
        ])
        .split(f.area());

    // Base regions (tabs/rows/footer) are only clickable when no overlay is open.
    let base = matches!(app.overlay(), Overlay::None);

    draw_tabs(f, app, chunks[0], regions);
    match app.tab() {
        Tab::Images => draw_images(f, app, chunks[1], regions, base),
        Tab::Containers => draw_containers(f, app, chunks[1], regions, base),
        Tab::Space => draw_space(f, app, chunks[1]),
    }
    draw_footer(f, app, chunks[2], regions, base);

    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => draw_help(f, app),
        Overlay::Prune => draw_prune_menu(f, app, regions),
        Overlay::Palette => draw_palette(f, app, regions),
        Overlay::UpdateDetails => draw_update_details(f, app, regions),
        Overlay::ApplyProgress => draw_apply_progress(f, app),
        Overlay::Confirm(action) => draw_confirm(f, app, action, regions),
        Overlay::Logs | Overlay::Changelog => draw_scroll_overlay(f, app),
    }

    let _ = theme;
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect, regions: &mut Vec<ClickRegion>) {
    let theme = app.theme();
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(format!(" {} ", t.title())))
        .collect();
    let tabs = Tabs::new(titles)
        .select(Tab::ALL.iter().position(|t| *t == app.tab()).unwrap_or(0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border))
                .title(Span::styled(
                    " DockerSmith ",
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                )),
        )
        .style(Style::default().fg(theme.dim))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);

    // Record a click region per tab. Titles render inside the border as
    // ` Title ` separated by the default divider ` | `.
    let mut x = area.x + 1; // inside the left border
    let y = area.y + 1; // the content row
    for t in Tab::ALL {
        let width = t.title().chars().count() as u16 + 2; // ` Title `
        push_region(regions, x, y, width, ClickTarget::Tab(t));
        x += width + 3; // account for the ` | ` divider between tabs
    }
}

fn draw_images(f: &mut Frame, app: &App, area: Rect, regions: &mut Vec<ClickRegion>, base: bool) {
    let theme = app.theme();
    let header = sort_header(app, Tab::Images);

    let rows: Vec<Row> = app
        .images()
        .iter()
        .enumerate()
        .map(|(i, img)| {
            let info = img
                .primary_reference()
                .and_then(|r| app.updates().get(&r).cloned());
            let style = row_style(theme, i == app.selected());
            Row::new(vec![
                Cell::from(img.short_name()),
                Cell::from(Span::styled(
                    img.version.clone().unwrap_or_else(|| "—".to_string()),
                    Style::default().fg(theme.fg),
                )),
                Cell::from(Span::styled(
                    img.created_date(),
                    Style::default().fg(theme.dim),
                )),
                Cell::from(Span::styled(
                    img.source_short(),
                    Style::default().fg(theme.secondary),
                )),
                Cell::from(format_bytes(img.size)),
                Cell::from(update_cell(theme, info.as_ref())),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(26),
            Constraint::Length(12),
            Constraint::Length(11),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Min(16),
        ],
    )
    .header(header)
    .block(list_block(theme, &format!(" Images ({}) ", app.images().len())));
    f.render_widget(table, area);
    record_rows(regions, area, app.images().len(), base);
    record_header(
        regions,
        area,
        &[
            Constraint::Percentage(26),
            Constraint::Length(12),
            Constraint::Length(11),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Min(16),
        ],
        Tab::Images,
        base,
    );
}

fn draw_containers(f: &mut Frame, app: &App, area: Rect, regions: &mut Vec<ClickRegion>, base: bool) {
    let theme = app.theme();
    let header = sort_header(app, Tab::Containers);

    let rows: Vec<Row> = app
        .containers()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let info = app.updates().get(&c.image).cloned();
            let (cpu, mem) = match app.stats().get(&c.image) {
                Some(s) => (
                    format!("{:.1}", s.cpu_percent),
                    format!("{} ({:.0}%)", format_bytes(s.mem_usage), s.mem_percent()),
                ),
                None => ("—".to_string(), "—".to_string()),
            };
            let state_style = if c.is_running() {
                Style::default().fg(theme.ok)
            } else {
                Style::default().fg(theme.dim)
            };
            let state_text = if c.status.is_empty() {
                c.state.clone()
            } else {
                c.status.clone()
            };
            Row::new(vec![
                Cell::from(c.display_name()),
                Cell::from(c.image.clone()),
                Cell::from(Span::styled(state_text, state_style)),
                Cell::from(cpu),
                Cell::from(mem),
                Cell::from(update_cell(theme, info.as_ref())),
            ])
            .style(row_style(theme, i == app.selected()))
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(20),
            Constraint::Percentage(26),
            Constraint::Length(14),
            Constraint::Length(7),
            Constraint::Length(13),
            Constraint::Min(16),
        ],
    )
    .header(header)
    .block(list_block(
        theme,
        &format!(" Containers ({}) ", app.containers().len()),
    ));
    f.render_widget(table, area);
    record_rows(regions, area, app.containers().len(), base);
    record_header(
        regions,
        area,
        &[
            Constraint::Percentage(20),
            Constraint::Percentage(26),
            Constraint::Length(14),
            Constraint::Length(7),
            Constraint::Length(13),
            Constraint::Min(16),
        ],
        Tab::Containers,
        base,
    );
}

/// Build a table header row whose active sort column shows a direction arrow.
fn sort_header(app: &App, tab: Tab) -> Row<'static> {
    let theme = app.theme();
    let (key, desc) = app.sort();
    let style = Style::default().fg(theme.secondary).add_modifier(Modifier::BOLD);
    let active = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let cells: Vec<Cell> = SortKey::columns(tab)
        .iter()
        .map(|(label, k)| {
            if *k == key {
                let arrow = if desc { "↓" } else { "↑" };
                Cell::from(Span::styled(format!("{label} {arrow}"), active))
            } else {
                Cell::from(Span::styled(label.to_string(), style))
            }
        })
        .collect();
    Row::new(cells)
}

/// Record a clickable header region per column so clicking a header sorts by it.
fn record_header(
    regions: &mut Vec<ClickRegion>,
    area: Rect,
    constraints: &[Constraint],
    tab: Tab,
    base: bool,
) {
    if !base {
        return;
    }
    // The header row sits on the first line inside the block border.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: 1,
    };
    let cells = Layout::horizontal(constraints).spacing(1).split(inner);
    for (rect, (_, key)) in cells.iter().zip(SortKey::columns(tab).iter()) {
        regions.push(ClickRegion {
            rect: *rect,
            target: ClickTarget::Header(*key),
        });
    }
}

/// Record a clickable region for each visible table row.
fn record_rows(regions: &mut Vec<ClickRegion>, area: Rect, count: usize, base: bool) {
    if !base {
        return;
    }
    // Inside the block border, a header row occupies the first line; data rows
    // follow. Visible data height = area height minus top border, header, bottom border.
    let visible = area.height.saturating_sub(3) as usize;
    let width = area.width.saturating_sub(2);
    for i in 0..count.min(visible) {
        push_region(
            regions,
            area.x + 1,
            area.y + 2 + i as u16,
            width,
            ClickTarget::Row(i),
        );
    }
}

fn draw_space(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let block = list_block(theme, " Disk usage (docker system df) ");
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let Some(u) = app.usage() else {
        f.render_widget(Paragraph::new("Loading…").style(Style::default().fg(theme.dim)), inner);
        return;
    };

    let entries = [
        ("Images", u.images_total, u.images_reclaimable, u.images_count),
        (
            "Containers",
            u.containers_total,
            u.containers_reclaimable,
            u.containers_count,
        ),
        ("Volumes", u.volumes_total, u.volumes_reclaimable, u.volumes_count),
        (
            "Build cache",
            u.build_cache_total,
            u.build_cache_reclaimable,
            u.build_cache_count,
        ),
    ];

    // Reserve the last two rows of the inner area for the summary line.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);

    let header = Row::new(vec!["TYPE", "TOTAL", "RECLAIMABLE", "ITEMS", "RECLAIMABLE"])
        .style(Style::default().fg(theme.secondary).add_modifier(Modifier::BOLD));

    let bar_width = 20usize;
    let rows: Vec<Row> = entries
        .iter()
        .map(|(label, total, reclaimable, count)| {
            let ratio = if *total > 0 {
                (*reclaimable as f64 / *total as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let filled = (ratio * bar_width as f64).round() as usize;
            let filled = filled.min(bar_width);
            let bar = Line::from(vec![
                Span::styled("█".repeat(filled), Style::default().fg(theme.warn)),
                Span::styled("░".repeat(bar_width - filled), Style::default().fg(theme.dim)),
                Span::styled(
                    format!(" {:>3.0}%", ratio * 100.0),
                    Style::default().fg(theme.dim),
                ),
            ]);
            Row::new(vec![
                Cell::from(Span::styled(
                    (*label).to_string(),
                    Style::default().fg(theme.fg),
                )),
                Cell::from(format_bytes(*total)),
                Cell::from(Span::styled(
                    format_bytes(*reclaimable),
                    Style::default().fg(theme.warn),
                )),
                Cell::from(format!("{count}")),
                Cell::from(bar),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(11),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .column_spacing(2);
    f.render_widget(table, split[0]);

    let summary = Paragraph::new(Line::from(vec![
        Span::styled("TOTAL RECLAIMABLE: ", Style::default().fg(theme.secondary)),
        Span::styled(
            format_bytes(u.total_reclaimable()),
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   press ", Style::default().fg(theme.dim)),
        Span::styled("p", Style::default().fg(theme.accent)),
        Span::styled(" to prune", Style::default().fg(theme.dim)),
    ]));
    f.render_widget(summary, split[1]);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect, regions: &mut Vec<ClickRegion>, base: bool) {
    let theme = app.theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    let status = Paragraph::new(Line::from(vec![
        Span::styled(" ● ", Style::default().fg(theme.ok)),
        Span::styled(app.status_message().to_string(), Style::default().fg(theme.fg)),
    ]));
    f.render_widget(status, chunks[0]);

    // Build clickable shortcut segments for the active tab.
    let mut segments: Vec<(&str, UiAction)> = vec![
        ("q quit", UiAction::Quit),
        (": palette", UiAction::OpenPalette),
        ("u check", UiAction::CheckSelected),
        ("U all", UiAction::CheckAll),
        ("⏎ details", UiAction::Details),
    ];
    match app.tab() {
        Tab::Containers => segments.extend([
            ("a apply", UiAction::Apply),
            ("s start/stop", UiAction::StartStop),
            ("R restart", UiAction::Restart),
            ("L logs", UiAction::Logs),
            ("w changelog", UiAction::Changelog),
            ("x rm", UiAction::Remove),
            ("d defer", UiAction::Defer),
            ("p prune", UiAction::OpenPrune),
        ]),
        Tab::Images => segments.extend([
            ("w changelog", UiAction::Changelog),
            ("d defer", UiAction::Defer),
            ("p prune", UiAction::OpenPrune),
        ]),
        Tab::Space => segments.extend([
            ("p prune", UiAction::OpenPrune),
            ("r refresh", UiAction::Refresh),
        ]),
    }
    segments.push(("o sort", UiAction::CycleSort));
    segments.push(("y select", UiAction::ToggleMouse));
    segments.push(("T theme", UiAction::CycleTheme));
    segments.push(("? help", UiAction::Help));

    let sep = " · ";
    let mut spans: Vec<Span> = Vec::new();
    let mut x = chunks[1].x;
    let y = chunks[1].y;
    let max_x = chunks[1].x + chunks[1].width;
    for (idx, (label, action)) in segments.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(sep, Style::default().fg(theme.border)));
            x += sep.chars().count() as u16;
        }
        let width = label.chars().count() as u16;
        if x + width > max_x {
            break; // ran out of horizontal room
        }
        spans.push(Span::styled(*label, Style::default().fg(theme.dim)));
        if base {
            push_region(regions, x, y, width, ClickTarget::Action(*action));
        }
        x += width;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), chunks[1]);
}

// ── Overlays ───────────────────────────────────────────────────────────────

fn draw_help(f: &mut Frame, app: &App) {
    let theme = app.theme();
    let text = vec![
        Line::from(Span::styled(
            "DockerSmith — keys",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  ⇥ / 1-3      switch tab (Images · Containers · Space)"),
        Line::from("  ↑↓ / j k     move selection"),
        Line::from("  r            refresh"),
        Line::from("  u / U        check update (selected / all)"),
        Line::from("  ⏎            update details (versions, dates, changelog)"),
        Line::from("  a            apply update — pull + recreate container"),
        Line::from("  s            start/stop container"),
        Line::from("  R            restart container"),
        Line::from("  L            view container logs"),
        Line::from("  w            view changelog (What's new?)"),
        Line::from("  x            remove container"),
        Line::from("  d            defer update 30 days"),
        Line::from("  p            prune menu"),
        Line::from("  o / O        cycle sort column / reverse direction"),
        Line::from("  y            select mode (mouse off, so you can copy text)"),
        Line::from("  T            cycle theme"),
        Line::from("  q / Esc      quit / close overlay"),
    ];
    let area = centered_rect(64, 60, f.area());
    f.render_widget(Clear, area);
    let p = Paragraph::new(text)
        .block(overlay_block(theme, " Help "))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn draw_prune_menu(f: &mut Frame, app: &App, regions: &mut Vec<ClickRegion>) {
    let theme = app.theme();
    let reclaimable = app
        .usage()
        .map(|u| format_bytes(u.total_reclaimable()))
        .unwrap_or_else(|| "?".to_string());
    // (label, action) for each selectable prune option, in display order.
    let options = [
        ("  i   dangling images", UiAction::PruneImages(false)),
        ("  I   ALL unused images", UiAction::PruneImages(true)),
        ("  c   stopped containers", UiAction::PruneContainers),
        ("  v   unused volumes", UiAction::PruneVolumes),
        ("  b   build cache", UiAction::PruneBuildCache),
        ("  a   everything unused", UiAction::PruneAll),
    ];
    let mut text = vec![
        Line::from(Span::styled(
            format!("Prune — up to {reclaimable} reclaimable"),
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (label, _) in &options {
        text.push(Line::from(*label));
    }
    text.push(Line::from(""));
    text.push(Line::from(Span::styled(
        "  Esc  cancel",
        Style::default().fg(theme.dim),
    )));

    let area = centered_rect(50, 45, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text).block(overlay_block(theme, " Prune ")),
        area,
    );

    // Options begin after the title + blank line, inside the top border.
    let first_y = area.y + 1 + 2;
    let width = area.width.saturating_sub(2);
    for (i, (_, action)) in options.iter().enumerate() {
        push_region(
            regions,
            area.x + 1,
            first_y + i as u16,
            width,
            ClickTarget::Action(*action),
        );
    }
}

fn draw_palette(f: &mut Frame, app: &App, regions: &mut Vec<ClickRegion>) {
    let theme = app.theme();
    let matches = app.palette_matches();
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("› ", Style::default().fg(theme.accent)),
        Span::styled(
            app.palette_query().to_string(),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled("▏", Style::default().fg(theme.accent)),
    ]));
    lines.push(Line::from(""));
    for (i, (label, _)) in matches.iter().enumerate() {
        let style = if i == app.palette_index() {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(format!("  {label}"), style)));
    }
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matching commands)",
            Style::default().fg(theme.dim),
        )));
    }
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(overlay_block(theme, " Command palette ")),
        area,
    );

    // The list starts after the query line + blank line, inside the top border.
    let first_y = area.y + 1 + 2;
    let width = area.width.saturating_sub(2);
    for i in 0..matches.len() {
        push_region(
            regions,
            area.x + 1,
            first_y + i as u16,
            width,
            ClickTarget::Action(UiAction::PaletteItem(i)),
        );
    }
}

fn draw_confirm(f: &mut Frame, app: &App, action: &ConfirmAction, regions: &mut Vec<ClickRegion>) {
    let theme = app.theme();
    let desc = match action {
        ConfirmAction::RemoveContainer(_, name) => format!("Remove container '{name}'?"),
        ConfirmAction::PruneImages(false) => "Prune dangling images?".to_string(),
        ConfirmAction::PruneImages(true) => "Prune ALL unused images?".to_string(),
        ConfirmAction::PruneContainers => "Prune all stopped containers?".to_string(),
        ConfirmAction::PruneVolumes => "Prune all unused volumes?".to_string(),
        ConfirmAction::PruneBuildCache => "Prune the build cache?".to_string(),
        ConfirmAction::PruneAll => "Prune EVERYTHING unused?".to_string(),
        ConfirmAction::ApplyUpdate(_, image) => {
            format!("Pull {image} and recreate this container?\n(preserves volumes, ports, env, and networks)")
        }
    };
    let text = vec![
        Line::from(Span::styled(
            desc,
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[ Yes ]",
                Style::default().fg(theme.ok).add_modifier(Modifier::BOLD),
            ),
            Span::raw("      "),
            Span::styled(
                "[ No ]",
                Style::default().fg(theme.err).add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    let area = centered_rect(56, 22, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .block(overlay_block(theme, " Confirm ")),
        area,
    );

    // Clickable Yes/No buttons on the centered button row (3rd content line).
    let btn_y = area.y + 1 + 2;
    let center = area.x + area.width / 2;
    push_region(regions, center.saturating_sub(10), btn_y, 7, ClickTarget::Action(UiAction::ConfirmYes));
    push_region(regions, center + 3, btn_y, 6, ClickTarget::Action(UiAction::ConfirmNo));
}

fn draw_scroll_overlay(f: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = centered_rect(84, 80, f.area());
    f.render_widget(Clear, area);

    let visible_height = area.height.saturating_sub(2) as usize;
    let start = app
        .overlay_scroll()
        .min(app.overlay_view().len().saturating_sub(1));
    let lines: Vec<Line> = app
        .overlay_view()
        .iter()
        .skip(start)
        .take(visible_height)
        .cloned()
        .collect();

    let title = if app.overlay_title().is_empty() {
        " View ".to_string()
    } else {
        format!(" {} ", app.overlay_title())
    };
    let p = Paragraph::new(lines)
        .block(overlay_block(theme, &title))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn list_block(theme: &crate::theme::Theme, title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(theme.secondary),
        ))
}

fn overlay_block(theme: &crate::theme::Theme, title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
}

fn row_style(theme: &crate::theme::Theme, selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg)
    }
}

fn update_cell(theme: &crate::theme::Theme, info: Option<&UpdateInfo>) -> Line<'static> {
    let Some(info) = info else {
        return Line::from(Span::styled("-", Style::default().fg(theme.dim)));
    };
    let (text, color) = match &info.status {
        UpdateStatus::UpdateAvailable => (
            info.transition().unwrap_or_else(|| "UPDATE".to_string()),
            theme.warn,
        ),
        UpdateStatus::UpToDate => (
            info.latest_label()
                .map(|v| format!("up to date ({v})"))
                .unwrap_or_else(|| "up to date".to_string()),
            theme.ok,
        ),
        UpdateStatus::Checking => ("checking…".to_string(), theme.dim),
        UpdateStatus::LocalOnly => ("local build".to_string(), theme.dim),
        UpdateStatus::Error(_) => ("error".to_string(), theme.err),
    };
    Line::from(Span::styled(text, Style::default().fg(color)))
}

/// The update-details overlay: current vs latest version/date plus changelog link.
fn draw_update_details(f: &mut Frame, app: &App, regions: &mut Vec<ClickRegion>) {
    let theme = app.theme();
    let area = centered_rect(66, 55, f.area());
    f.render_widget(Clear, area);

    let Some((image, info)) = app.selected_update() else {
        f.render_widget(
            Paragraph::new("No image selected.").block(overlay_block(theme, " Update details ")),
            area,
        );
        return;
    };

    let label = |s: &str| Span::styled(format!("{s:<10}"), Style::default().fg(theme.secondary));
    let val = |s: String, c: ratatui::style::Color| Span::styled(s, Style::default().fg(c));
    let dash = "—".to_string();

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        image.clone(),
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    match info {
        None => {
            lines.push(Line::from(Span::styled(
                "Not checked yet — press u to check for updates.",
                Style::default().fg(theme.dim),
            )));
        }
        Some(info) => {
            let status_span = match &info.status {
                UpdateStatus::UpdateAvailable => {
                    val("UPDATE AVAILABLE".to_string(), theme.warn)
                }
                UpdateStatus::UpToDate => val("up to date".to_string(), theme.ok),
                UpdateStatus::Checking => val("checking…".to_string(), theme.dim),
                UpdateStatus::LocalOnly => {
                    val("locally built (no registry image)".to_string(), theme.dim)
                }
                UpdateStatus::Error(e) => val(format!("error: {e}"), theme.err),
            };
            lines.push(Line::from(vec![label("status"), status_span]));
            if let Some(checked) = info.checked_at {
                lines.push(Line::from(vec![
                    label("checked"),
                    Span::styled(
                        crate::util::format_relative(checked),
                        Style::default().fg(theme.dim),
                    ),
                ]));
            }
            lines.push(Line::from(""));

            lines.push(Line::from(vec![
                label("current"),
                val(info.current_version.clone().unwrap_or_else(|| dash.clone()), theme.fg),
                Span::styled(
                    format!("   ({})", info.current_date.clone().unwrap_or_else(|| dash.clone())),
                    Style::default().fg(theme.dim),
                ),
            ]));
            lines.push(Line::from(vec![
                label("latest"),
                val(
                    info.latest_version.clone().unwrap_or_else(|| dash.clone()),
                    if info.status == UpdateStatus::UpdateAvailable {
                        theme.warn
                    } else {
                        theme.fg
                    },
                ),
                Span::styled(
                    format!("   ({})", info.latest_date.clone().unwrap_or_else(|| dash.clone())),
                    Style::default().fg(theme.dim),
                ),
            ]));
            lines.push(Line::from(""));

            match &info.changelog_repo {
                Some(repo) => {
                    lines.push(Line::from(vec![
                        label("changelog"),
                        val(format!("github.com/{repo}"), theme.accent),
                    ]));
                    lines.push(Line::from(Span::styled(
                        "  press w to fetch release notes",
                        Style::default().fg(theme.dim),
                    )));
                }
                None => {
                    lines.push(Line::from(vec![
                        label("changelog"),
                        val("not available for this image".to_string(), theme.dim),
                    ]));
                }
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  u recheck · w changelog · Esc close",
        Style::default().fg(theme.dim),
    )));

    let footer_row = area.y + 1 + (lines.len() as u16).saturating_sub(1);
    f.render_widget(
        Paragraph::new(lines)
            .block(overlay_block(theme, " Update details "))
            .wrap(Wrap { trim: false }),
        area,
    );

    // Clickable action words on the footer row: "  u recheck · w changelog · Esc close".
    let base_x = area.x + 1;
    push_region(regions, base_x + 2, footer_row, 9, ClickTarget::Action(UiAction::CheckSelected));
    push_region(regions, base_x + 14, footer_row, 11, ClickTarget::Action(UiAction::Changelog));
    push_region(regions, base_x + 28, footer_row, 9, ClickTarget::Action(UiAction::CloseOverlay));
}


/// The live progress overlay for an in-flight container update.
fn draw_apply_progress(f: &mut Frame, app: &App) {
    let theme = app.theme();
    let Some(a) = app.apply() else {
        return;
    };
    let area = centered_rect(70, 76, f.area());
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        a.title.clone(),
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Checklist of stages.
    for (stage, status) in &a.steps {
        let (icon, color) = step_icon(theme, *status);
        let label_color = if *status == StepStatus::Pending {
            theme.dim
        } else {
            theme.fg
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {icon}  "), Style::default().fg(color)),
            Span::styled(stage.label().to_string(), Style::default().fg(label_color)),
        ]));
    }
    if let Some(rb) = a.rollback {
        let (icon, _) = step_icon(theme, rb);
        lines.push(Line::from(vec![
            Span::styled(format!("  {icon}  "), Style::default().fg(theme.warn)),
            Span::styled(
                "Roll back to previous container".to_string(),
                Style::default().fg(theme.warn),
            ),
        ]));
    }

    lines.push(Line::from(""));

    // Error detail, if any.
    if let Some(err) = &a.error {
        lines.push(Line::from(Span::styled(
            "ERROR".to_string(),
            Style::default().fg(theme.err).add_modifier(Modifier::BOLD),
        )));
        for l in err.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {l}"),
                Style::default().fg(theme.err),
            )));
        }
        lines.push(Line::from(""));
    }

    // Recent output (tail of the detail log).
    if !a.log.is_empty() {
        lines.push(Line::from(Span::styled(
            "recent output".to_string(),
            Style::default().fg(theme.secondary),
        )));
        let start = a.log.len().saturating_sub(8);
        for l in &a.log[start..] {
            lines.push(Line::from(Span::styled(
                format!("  {l}"),
                Style::default().fg(theme.dim),
            )));
        }
        lines.push(Line::from(""));
    }

    let footer = if !a.finished {
        Span::styled("working…".to_string(), Style::default().fg(theme.accent))
    } else if a.success {
        Span::styled(
            "✔ update complete — press Esc to close".to_string(),
            Style::default().fg(theme.ok),
        )
    } else {
        Span::styled(
            "✖ update failed — previous container restored — press Esc to close".to_string(),
            Style::default().fg(theme.err),
        )
    };
    lines.push(Line::from(footer));

    f.render_widget(
        Paragraph::new(lines)
            .block(overlay_block(theme, " Applying update "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Icon + color for an apply step status.
fn step_icon(
    theme: &crate::theme::Theme,
    status: StepStatus,
) -> (&'static str, ratatui::style::Color) {
    match status {
        StepStatus::Pending => ("○", theme.dim),
        StepStatus::Running => ("◐", theme.accent),
        StepStatus::Done => ("✔", theme.ok),
        StepStatus::Failed => ("✖", theme.err),
    }
}

/// A centered rectangle occupying `percent_x` × `percent_y` of `r`.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
