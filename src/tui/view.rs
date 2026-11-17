//! Drawing the TUI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};

use super::model::{App, Kind, Location, Mark, Popup, mark_text};
use crate::paths;
use crate::units::{format_bytes, format_count, format_hours};

const PALETTE: [Color; 10] = [
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Blue,
    Color::LightRed,
    Color::LightGreen,
    Color::LightCyan,
    Color::LightMagenta,
    Color::LightBlue,
];

/// Each grant's color, in config order.
fn grant_color(app: &App, id: &str) -> Color {
    app.cfg
        .grants
        .iter()
        .position(|g| g.id == id)
        .map_or(Color::Reset, |i| PALETTE[i % PALETTE.len()])
}

const KEYS: &str = "↑↓ move  →/⏎ open  ← back  s sort  g grant  i info  e estimate  a archive  r restore  c compute  R refresh  ? help  q quit";

const HELP: &str = "\
Moving
  ↑ ↓ j k        move          PgUp PgDn Home End
  → ⏎ l          open a directory
  ← ⌫ h          back up
  s              sort by size, name, file count, or oldest first
  R              measure again

Looking
  i              details: grant, marks, sizes from past scans and usage samples
  c              compute panel: jobs per grant over 30 days

Changing
  g              add the directory to a grant, or remove it (edits the config,
                 keeping a .bak copy)
  e              estimate what archiving it would save
  a              archive it (asks first; runs as a Slurm task)
  r              restore an archived directory (asks first)

  e, a and r leave this screen and run the same commands as the CLI,
  with the same questions and checks.

Marks
  environment, cache, excluded   counted in usage, never archived
  archived ID / archiving ID     holds a stomp archive
  grants A, B                    contains grant directories
  proposed #N                    proposed by a policy (stomp candidates)

Press any key to close.";

fn age_short(t: Option<i64>) -> String {
    let Some(t) = t else { return String::new() };
    let s = jiff::Timestamp::now().as_second() - t;
    match s {
        s if s < 3600 => "now".into(),
        s if s < 86_400 => format!("{}h", s / 3600),
        s if s < 60 * 86_400 => format!("{}d", s / 86_400),
        s if s < 730 * 86_400 => format!("{}mo", s / (30 * 86_400)),
        s => format!("{}y", s / (365 * 86_400)),
    }
}

fn bar(fraction: f64, width: usize) -> String {
    let filled = (fraction.clamp(0.0, 1.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "·".repeat(width - filled))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let panel_height = app
        .compute
        .as_ref()
        .map_or(0, |c| (c.rows.len() as u16 + 4).clamp(5, 12));
    let [header, main, panel, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(panel_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_header(frame, app, header);
    draw_list(frame, app, main);
    if panel_height > 0 {
        draw_compute(frame, app, panel);
    }
    let footer_text = match &app.status {
        Some(s) => Line::from(Span::styled(s.clone(), Style::new().fg(Color::Yellow))),
        None => Line::from(Span::styled(KEYS, Style::new().fg(Color::DarkGray))),
    };
    frame.render_widget(Paragraph::new(footer_text), footer);
    if let Some(popup) = &app.popup {
        draw_popup(frame, app, popup);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let mut first = vec![Span::styled(
        " stomp ",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    match &app.location {
        Location::Roots => first.push(Span::raw("your roots")),
        Location::Dir(d) => first.push(Span::raw(paths::display(d))),
    }
    if let Some(g) = app.here_grant() {
        first.push(Span::raw("   "));
        first.push(Span::styled(
            format!("grant {}", g.id),
            Style::new()
                .fg(grant_color(app, &g.id))
                .add_modifier(Modifier::BOLD),
        ));
        if let Some(end) = g.end {
            first.push(Span::styled(
                format!(" (ends {end})"),
                Style::new().fg(Color::DarkGray),
            ));
        }
    }
    let known: Vec<_> = app.entries.iter().filter_map(|e| e.usage).collect();
    let bytes: u64 = known.iter().map(|u| u.bytes).sum();
    let files: u64 = known.iter().map(|u| u.files).sum();
    let mut second = format!(" {} in {} files", format_bytes(bytes), format_count(files));
    if !app.pending.is_empty() {
        second += &format!(
            " so far · measuring {} director{}…",
            app.pending.len(),
            if app.pending.len() == 1 { "y" } else { "ies" }
        );
    } else if app.instant {
        second += " · live CephFS sizes";
    }
    if let Some(q) = &app.quota
        && let Some(max) = q.max_bytes
    {
        second += &format!(
            " · quota {} of {} ({:.0}%)",
            format_bytes(q.used.bytes),
            format_bytes(max),
            100.0 * q.used.bytes as f64 / max.max(1) as f64
        );
    }
    second += &format!(" · sorted by {}", app.sort.label());
    let text = Text::from(vec![
        Line::from(first),
        Line::from(Span::styled(second, Style::new().fg(Color::DarkGray))),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    if let Some(err) = &app.error {
        frame.render_widget(
            Paragraph::new(err.clone()).style(Style::new().fg(Color::Red)),
            area,
        );
        return;
    }
    if app.entries.is_empty() {
        frame.render_widget(Paragraph::new(" (empty)"), area);
        return;
    }
    let largest = app
        .entries
        .iter()
        .filter_map(|e| e.usage.map(|u| u.bytes))
        .max()
        .unwrap_or(0)
        .max(1);
    let rows: Vec<Row> = app
        .entries
        .iter()
        .map(|e| {
            let (size, fraction, files, age) = match e.usage {
                Some(u) => (
                    format_bytes(u.bytes),
                    u.bytes as f64 / largest as f64,
                    if e.kind == Kind::Dir {
                        format_count(u.files)
                    } else {
                        String::new()
                    },
                    age_short(u.newest_change),
                ),
                None => ("…".into(), 0.0, String::new(), String::new()),
            };
            let excluded = e.marks.iter().any(|m| matches!(m, Mark::Excluded(_)));
            let mut name_style = match &e.grant {
                Some(g) => Style::new().fg(grant_color(app, g)),
                None => Style::new(),
            };
            if excluded || e.marks.contains(&Mark::ArchiveData) {
                name_style = Style::new().fg(Color::DarkGray);
            }
            if e.kind == Kind::Dir {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            let mut name = vec![Span::styled(
                match e.kind {
                    Kind::Dir => format!("{}/", e.name),
                    Kind::Link => format!("{}@", e.name),
                    _ => e.name.clone(),
                },
                name_style,
            )];
            for m in &e.marks {
                let color = match m {
                    Mark::Archived { .. } => Color::Green,
                    Mark::HoldsGrants(_) => Color::Blue,
                    _ => Color::DarkGray,
                };
                name.push(Span::styled(
                    format!("  [{}]", mark_text(m)),
                    Style::new().fg(color),
                ));
            }
            if let Some(id) = e.proposal {
                name.push(Span::styled(
                    format!("  [proposed #{id}]"),
                    Style::new().fg(Color::Yellow),
                ));
            }
            let grant = e.grant.clone().unwrap_or_default();
            let grant_style = Style::new().fg(e
                .grant
                .as_deref()
                .map_or(Color::Reset, |g| grant_color(app, g)));
            Row::new(vec![
                Cell::from(Line::from(size).right_aligned()),
                Cell::from(Span::styled(
                    bar(fraction, 10),
                    Style::new().fg(Color::DarkGray),
                )),
                Cell::from(Line::from(files).right_aligned()),
                Cell::from(Line::from(age).right_aligned()),
                Cell::from(Span::styled(grant, grant_style)),
                Cell::from(Line::from(name)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(11),
        Constraint::Length(5),
        Constraint::Length(12),
        Constraint::Fill(1),
    ];
    let header = Row::new(vec![
        Cell::from(Line::from("SIZE").right_aligned()),
        Cell::from(""),
        Cell::from(Line::from("FILES").right_aligned()),
        Cell::from(Line::from("AGE").right_aligned()),
        Cell::from("GRANT"),
        Cell::from("NAME"),
    ])
    .style(Style::new().fg(Color::DarkGray));
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    app.page = area.height.saturating_sub(2).max(1) as usize;
    app.table.select(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut app.table);
}

fn draw_compute(frame: &mut Frame, app: &App, area: Rect) {
    let Some(panel) = &app.compute else { return };
    let here = app.here_grant().map(|g| g.id.clone());
    let rows: Vec<Row> = panel
        .rows
        .iter()
        .map(|r| {
            let mut style = Style::new().fg(grant_color(app, &r.grant));
            if here.as_deref() == Some(r.grant.as_str()) {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            Row::new(vec![
                Cell::from(Span::styled(r.grant.clone(), style)),
                Cell::from(Line::from(r.jobs.to_string()).right_aligned()),
                Cell::from(Line::from(format_hours(r.cpu_hours)).right_aligned()),
                Cell::from(Line::from(format_hours(r.gpu_hours)).right_aligned()),
            ])
        })
        .collect();
    let synced = match panel.synced {
        Some(t) => format!("accounting imported {}", crate::cmd::age(Some(t))),
        None => "no accounting imported yet: stomp jobs sync".into(),
    };
    let title = format!(" Compute, last 30 days · {synced} ");
    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from("GRANT"),
            Cell::from(Line::from("JOBS").right_aligned()),
            Cell::from(Line::from("CPU-HOURS").right_aligned()),
            Cell::from(Line::from("GPU-HOURS").right_aligned()),
        ])
        .style(Style::new().fg(Color::DarkGray)),
    )
    .block(Block::bordered().title(title));
    if panel.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(" No jobs in the last 30 days.")
                .block(Block::bordered().title(" Compute ")),
            area,
        );
    } else {
        frame.render_widget(table, area);
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(4)).max(10);
    let h = height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn draw_popup(frame: &mut Frame, app: &App, popup: &Popup) {
    let area = frame.area();
    match popup {
        Popup::Help => {
            let rect = centered(area, 84, HELP.lines().count() as u16 + 2);
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(HELP).block(Block::bordered().title(" stomp tui ")),
                rect,
            );
        }
        Popup::Message { title, text } => {
            let rect = centered(area, 76, text.lines().count() as u16 + 6);
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(format!("{text}\n\nPress any key."))
                    .wrap(Wrap { trim: false })
                    .block(Block::bordered().title(format!(" {title} "))),
                rect,
            );
        }
        Popup::Info { title, lines } => {
            let rect = centered(area, 90, lines.len() as u16 + 4);
            frame.render_widget(Clear, rect);
            let mut text = lines.join("\n");
            text += "\n\nPress any key.";
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(Block::bordered().title(format!(" {title} "))),
                rect,
            );
        }
        Popup::Grants {
            path,
            options,
            selected,
        } => {
            let rect = centered(area, 64, options.len() as u16 + 2);
            frame.render_widget(Clear, rect);
            let items: Vec<ListItem> = options
                .iter()
                .map(|(label, grant)| {
                    let color = grant.as_deref().map_or(Color::Red, |g| grant_color(app, g));
                    ListItem::new(Span::styled(label.clone(), Style::new().fg(color)))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
                    .block(
                        Block::bordered()
                            .title(format!(" Grant for {} ", paths::display(path)))
                            .title_bottom(" ⏎ choose · Esc cancel "),
                    ),
                rect,
                &mut state,
            );
        }
    }
}
