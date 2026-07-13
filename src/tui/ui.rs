//! Rendering for the TUI. Pure functions over [`App`] state.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap,
};
use ratatui::Frame;

use super::{App, ConfirmAction, Overlay, Tab};use crate::docker::model::UpdateStatus;
use crate::util::format_bytes;

/// Top-level draw entry point.
pub fn draw(f: &mut Frame, app: &App) {
    let theme = app.theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tabs
            Constraint::Min(1),    // body
            Constraint::Length(2), // footer
        ])
        .split(f.area());

    draw_tabs(f, app, chunks[0]);
    match app.tab() {
        Tab::Images => draw_images(f, app, chunks[1]),
        Tab::Containers => draw_containers(f, app, chunks[1]),
        Tab::Space => draw_space(f, app, chunks[1]),
    }
    draw_footer(f, app, chunks[2]);

    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => draw_help(f, app),
        Overlay::Prune => draw_prune_menu(f, app),
        Overlay::Palette => draw_palette(f, app),
        Overlay::Confirm(action) => draw_confirm(f, app, action),
        Overlay::Logs | Overlay::Changelog => draw_scroll_overlay(f, app),
    }

    let _ = theme;
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
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
}

fn draw_images(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let header = Row::new(vec!["IMAGE", "SIZE", "USED BY", "UPDATE"])
        .style(Style::default().fg(theme.secondary).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .images()
        .iter()
        .enumerate()
        .map(|(i, img)| {
            let update = img
                .primary_reference()
                .and_then(|r| app.updates().get(&r))
                .cloned()
                .unwrap_or(UpdateStatus::Unknown);
            let style = row_style(theme, i == app.selected());
            Row::new(vec![
                Cell::from(img.display_name()),
                Cell::from(format_bytes(img.size)),
                Cell::from(if img.is_unused() {
                    "—".to_string()
                } else {
                    format!("{} container(s)", img.containers.max(0))
                }),
                Cell::from(update_span(theme, &update)),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(50),
            Constraint::Length(12),
            Constraint::Length(16),
            Constraint::Length(14),
        ],
    )
    .header(header)
    .block(list_block(theme, &format!(" Images ({}) ", app.images().len())));
    f.render_widget(table, area);
}

fn draw_containers(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let header = Row::new(vec!["NAME", "IMAGE", "STATE", "CPU%", "MEM", "UPDATE"])
        .style(Style::default().fg(theme.secondary).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .containers()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let update = app
                .updates()
                .get(&c.image)
                .cloned()
                .unwrap_or(UpdateStatus::Unknown);
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
                Cell::from(update_span(theme, &update)),
            ])
            .style(row_style(theme, i == app.selected()))
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(22),
            Constraint::Percentage(30),
            Constraint::Length(14),
            Constraint::Length(7),
            Constraint::Length(14),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(list_block(
        theme,
        &format!(" Containers ({}) ", app.containers().len()),
    ));
    f.render_widget(table, area);
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

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
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

    let keys = match app.tab() {
        Tab::Containers => {
            "q quit · : palette · ⇥ tab · u/U check · a apply · s start/stop · R restart · L logs · w changelog · x rm · d defer · p prune · T theme · ? help"
        }
        Tab::Images => "q quit · : palette · ⇥ tab · u/U check · w changelog · d defer · p prune · T theme · ? help",
        Tab::Space => "q quit · : palette · ⇥ tab · p prune · r refresh · T theme · ? help",
    };
    let help = Paragraph::new(Span::styled(keys, Style::default().fg(theme.dim)));
    f.render_widget(help, chunks[1]);
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
        Line::from("  a            apply update (opt-in; container tab)"),
        Line::from("  s            start/stop container"),
        Line::from("  R            restart container"),
        Line::from("  L            view container logs"),
        Line::from("  w            view changelog (What's new?)"),
        Line::from("  x            remove container"),
        Line::from("  d            defer update 30 days"),
        Line::from("  p            prune menu"),
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

fn draw_prune_menu(f: &mut Frame, app: &App) {
    let theme = app.theme();
    let reclaimable = app
        .usage()
        .map(|u| format_bytes(u.total_reclaimable()))
        .unwrap_or_else(|| "?".to_string());
    let text = vec![
        Line::from(Span::styled(
            format!("Prune — up to {reclaimable} reclaimable"),
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  i   dangling images"),
        Line::from("  I   ALL unused images"),
        Line::from("  c   stopped containers"),
        Line::from("  v   unused volumes"),
        Line::from("  b   build cache"),
        Line::from("  a   everything unused"),
        Line::from(""),
        Line::from(Span::styled("  Esc  cancel", Style::default().fg(theme.dim))),
    ];
    let area = centered_rect(50, 45, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text).block(overlay_block(theme, " Prune ")),
        area,
    );
}

fn draw_palette(f: &mut Frame, app: &App) {
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
}

fn draw_confirm(f: &mut Frame, app: &App, action: &ConfirmAction) {
    let theme = app.theme();
    let desc = match action {
        ConfirmAction::RemoveContainer(_, name) => format!("Remove container '{name}'?"),
        ConfirmAction::PruneImages(false) => "Prune dangling images?".to_string(),
        ConfirmAction::PruneImages(true) => "Prune ALL unused images?".to_string(),
        ConfirmAction::PruneContainers => "Prune all stopped containers?".to_string(),
        ConfirmAction::PruneVolumes => "Prune all unused volumes?".to_string(),
        ConfirmAction::PruneBuildCache => "Prune the build cache?".to_string(),
        ConfirmAction::PruneAll => "Prune EVERYTHING unused?".to_string(),
        ConfirmAction::ApplyUpdate(_, image) => format!("Pull {image} and recreate container?"),
    };
    let text = vec![
        Line::from(Span::styled(
            desc,
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  y confirm      n cancel",
            Style::default().fg(theme.dim),
        )),
    ];
    let area = centered_rect(56, 22, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .block(overlay_block(theme, " Confirm ")),
        area,
    );
}

fn draw_scroll_overlay(f: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = centered_rect(84, 80, f.area());
    f.render_widget(Clear, area);

    let visible_height = area.height.saturating_sub(2) as usize;
    let start = app
        .overlay_scroll()
        .min(app.overlay_lines().len().saturating_sub(1));
    let lines: Vec<Line> = app
        .overlay_lines()
        .iter()
        .skip(start)
        .take(visible_height)
        .map(|l| Line::from(l.clone()))
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

fn update_span(theme: &crate::theme::Theme, status: &UpdateStatus) -> Span<'static> {
    let color = match status {
        UpdateStatus::UpdateAvailable => theme.warn,
        UpdateStatus::UpToDate => theme.ok,
        UpdateStatus::Error(_) => theme.err,
        _ => theme.dim,
    };
    Span::styled(status.label().to_string(), Style::default().fg(color))
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
