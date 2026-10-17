//! Terminal presentation uses the same allowlisted projection as public HTML reports.
//! No host identifiers, paths, raw errors or arbitrary metric labels reach the screen.
use crate::{analyzer, model::SnapshotV1, report};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Tabs, Wrap,
    },
    Frame,
};
use serde_json::Value;

const BG: Color = Color::Rgb(12, 18, 31);
const PANEL: Color = Color::Rgb(17, 26, 43);
const BORDER: Color = Color::Rgb(47, 65, 89);
const TEXT: Color = Color::Rgb(222, 231, 242);
const MUTED: Color = Color::Rgb(145, 164, 188);
const TEAL: Color = Color::Rgb(79, 218, 197);
const PURPLE: Color = Color::Rgb(178, 158, 250);
const AMBER: Color = Color::Rgb(248, 195, 104);
const RED: Color = Color::Rgb(249, 125, 135);
const SELECTED: Color = Color::Rgb(35, 53, 76);
const TAB_NAMES: [&str; 4] = ["1 Overview", "2 Findings", "3 Capacity", "4 Compare"];

#[derive(Debug)]
struct CompareRow {
    scope: String,
    metric: Value,
}

pub struct ViewState {
    snapshot: Value,
    findings: Vec<Value>,
    capacity: Value,
    comparison: Option<Value>,
    comparison_rows: Vec<CompareRow>,
    tab: usize,
    finding_selected: usize,
    service_selected: usize,
    compare_selected: usize,
    detail_scroll: u16,
    detail_max_scroll: u16,
    help: bool,
}

impl ViewState {
    pub fn new(
        current: &SnapshotV1,
        before: Option<&SnapshotV1>,
        profile: Option<&str>,
        temporal: bool,
        allow_legacy_identity: bool,
    ) -> Self {
        let options = analyzer::DoctorOptions {
            profile: profile.map(str::to_owned),
            ..Default::default()
        };
        let findings = analyzer::doctor(current, &options);
        let capacity = analyzer::capacity(current, profile, None);
        let comparison = before.map(|before| {
            let comparison = analyzer::compare(before, current, temporal, allow_legacy_identity);
            let before_findings = analyzer::doctor(before, &options);
            let before_capacity = analyzer::capacity(before, profile, None);
            let mut projected = report::public_comparison(
                before,
                current,
                &comparison,
                &before_findings,
                &findings,
                &before_capacity,
                &capacity,
            )["comparison"]
                .take();
            sanitize_value(&mut projected);
            projected
        });
        let mut snapshot = report::public_snapshot(current);
        let mut findings = report::public_findings(&findings);
        let mut capacity = report::public_capacity(&capacity);
        for value in [&mut snapshot, &mut findings, &mut capacity] {
            sanitize_value(value);
        }
        let mut comparison_rows = Vec::new();
        if let Some(comparison) = &comparison {
            for metric in array(&comparison["host_metrics"]) {
                comparison_rows.push(CompareRow {
                    scope: "host".into(),
                    metric: metric.clone(),
                });
            }
            for service in array(&comparison["services"]) {
                for metric in array(&service["metrics"]) {
                    comparison_rows.push(CompareRow {
                        scope: string(&service["service"]),
                        metric: metric.clone(),
                    });
                }
            }
        }
        Self {
            snapshot,
            findings: findings.as_array().cloned().unwrap_or_default(),
            capacity,
            comparison,
            comparison_rows,
            tab: 0,
            finding_selected: 0,
            service_selected: 0,
            compare_selected: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
            help: false,
        }
    }

    /// Returns true only when the viewer should exit. It never invokes collection.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if matches!(key.code, KeyCode::Char('q' | 'Q'))
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return true;
        }
        if self.help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.help = false;
            }
            return false;
        }
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char(value @ '1'..='4') => self.set_tab(value as usize - '1' as usize),
            KeyCode::BackTab | KeyCode::Left => self.set_tab((self.tab + 3) % 4),
            KeyCode::Tab | KeyCode::Right => self.set_tab((self.tab + 1) % 4),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.page(6),
            KeyCode::PageUp => self.page(-6),
            KeyCode::Home => self.home_end(false),
            KeyCode::End => self.home_end(true),
            _ => {}
        }
        false
    }

    fn set_tab(&mut self, tab: usize) {
        self.tab = tab;
        self.detail_scroll = 0;
        self.detail_max_scroll = 0;
    }

    fn move_selection(&mut self, direction: i16) {
        match self.tab {
            0 => shift(
                &mut self.service_selected,
                array(&self.snapshot["services"]).len(),
                direction,
            ),
            1 => {
                shift(&mut self.finding_selected, self.findings.len(), direction);
                self.detail_scroll = 0;
            }
            2 => self.scroll_detail(direction),
            3 => {
                shift(
                    &mut self.compare_selected,
                    self.comparison_rows.len(),
                    direction,
                );
                self.detail_scroll = 0;
            }
            _ => {}
        }
    }

    fn page(&mut self, direction: i16) {
        if self.tab != 0 {
            self.scroll_detail(direction);
        } else {
            self.move_selection(direction);
        }
    }

    fn scroll_detail(&mut self, direction: i16) {
        self.detail_scroll = self
            .detail_scroll
            .saturating_add_signed(direction)
            .min(self.detail_max_scroll);
    }

    fn home_end(&mut self, end: bool) {
        if self.tab == 1 || self.tab == 2 {
            self.detail_scroll = if end { self.detail_max_scroll } else { 0 };
        } else {
            self.move_selection(if end { i16::MAX } else { i16::MIN });
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(TEXT)),
            area,
        );
        if area.width < 70 || area.height < 18 {
            self.draw_narrow(frame, area);
            return;
        }
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(area);
        let capture = string(&self.snapshot["capture"]["input_format"]);
        let cpu = string(&self.snapshot["host"]["cpu_count"]);
        let header = vec![
            Line::from(vec![
                Span::styled(
                    "  FIBRE DOCTOR KIT",
                    Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  /  operator diagnostics", Style::default().fg(MUTED)),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("  {capture} snapshot  |  {cpu} CPU  |  "),
                    Style::default().fg(MUTED),
                ),
                Span::styled("READ ONLY", Style::default().fg(TEAL)),
                Span::styled(
                    "  |  static capture, no automatic refresh",
                    Style::default().fg(MUTED),
                ),
            ]),
        ];
        let header = if area.width < 100 {
            vec![
                header[0].clone(),
                Line::styled(
                    format!("  {capture} snapshot | READ ONLY | No automatic refresh"),
                    Style::default().fg(MUTED),
                ),
            ]
        } else {
            header
        };
        frame.render_widget(Paragraph::new(header), layout[0]);
        frame.render_widget(
            Tabs::new(TAB_NAMES)
                .select(self.tab)
                .divider("  ")
                .padding(" ", " ")
                .style(Style::default().fg(MUTED))
                .highlight_style(
                    Style::default()
                        .bg(SELECTED)
                        .fg(TEAL)
                        .add_modifier(Modifier::BOLD),
                )
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_style(Style::default().fg(BORDER)),
                ),
            layout[1],
        );
        match self.tab {
            0 => self.draw_overview(frame, layout[2]),
            1 => self.draw_findings(frame, layout[2]),
            2 => self.draw_capacity(frame, layout[2]),
            3 => self.draw_compare(frame, layout[2]),
            _ => {}
        }
        let hint = match self.tab {
            0 => "Up/Down service",
            1 => "Up/Down select  PgUp/PgDn details",
            2 => "Up/Down scroll",
            _ => "Up/Down select  PgUp/PgDn details",
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  1-4 / Tab ", Style::default().fg(TEAL)),
                Span::raw("screens  "),
                Span::styled(hint, Style::default().fg(MUTED)),
                Span::styled("  ? ", Style::default().fg(TEAL)),
                Span::raw("help"),
                Span::styled("  q ", Style::default().fg(TEAL)),
                Span::raw("quit"),
            ]))
            .style(Style::default().fg(MUTED))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(BORDER)),
            ),
            layout[3],
        );
        if area.width < 100 {
            frame.render_widget(
                Paragraph::new(" Tab/1-4 views | Up/Down | PgUp/PgDn details | ? help | q quit")
                    .style(Style::default().fg(TEAL).bg(BG))
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_style(Style::default().fg(BORDER)),
                    ),
                layout[3],
            );
        }
        if self.help {
            self.draw_help(frame, area);
        }
    }

    fn draw_narrow(&self, frame: &mut Frame, area: Rect) {
        let text = vec![
            Line::styled(
                "FIBRE DOCTOR KIT",
                Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
            ),
            Line::raw("Static, read-only snapshot viewer"),
            Line::raw(""),
            Line::raw("Resize terminal to at least 70 x 18."),
            Line::raw("Recommended: 100 x 32 or larger."),
            Line::raw(format!(
                "Host available: {}",
                bytes(&self.snapshot["host"]["memory_available_bytes"])
            )),
            Line::raw(format!(
                "Collection issues: {}",
                string(&self.snapshot["collection_issue_count"])
            )),
            Line::raw("q / Esc: quit"),
        ];
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(panel(" Terminal size ")),
            area,
        );
    }

    fn draw_overview(&mut self, frame: &mut Frame, area: Rect) {
        let bottom_height = if area.height >= 21 { 8 } else { 5 };
        let expanded = area.height >= 26;
        let constraints = if expanded {
            vec![
                Constraint::Length(5),
                Constraint::Length(7),
                Constraint::Min(6),
                Constraint::Length(bottom_height),
            ]
        } else {
            vec![
                Constraint::Length(5),
                Constraint::Min(5),
                Constraint::Length(bottom_height),
            ]
        };
        let sections = Layout::vertical(constraints).split(area);
        let cards = Layout::horizontal([Constraint::Ratio(1, 3); 3]).split(sections[0]);
        let host = &self.snapshot["host"];
        self.card(
            frame,
            cards[0],
            " Host memory ",
            &bytes(&host["memory_available_bytes"]),
            if area.width < 100 {
                "available / shared"
            } else {
                "available to all workloads"
            },
            &format!("{} total", bytes(&host["memory_total_bytes"])),
            TEAL,
        );
        self.card(
            frame,
            cards[1],
            " Root filesystem ",
            &bytes(&host["root_filesystem"]["available_bytes"]),
            if area.width < 100 {
                "available on /"
            } else {
                "available on root filesystem"
            },
            &format!("{} total", bytes(&host["root_filesystem"]["total_bytes"])),
            PURPLE,
        );
        let mut counts = [0usize; 5];
        for finding in &self.findings {
            counts[match finding["status"].as_str() {
                Some("PASS") => 0,
                Some("WARN") => 1,
                Some("FAIL") => 2,
                Some("UNSUPPORTED") => 4,
                _ => 3,
            }] += 1;
        }
        self.card(
            frame,
            cards[2],
            " Evidence checks ",
            &format!("{} findings", self.findings.len()),
            &format!(
                "{} pass / {} warn / {} fail",
                counts[0], counts[1], counts[2]
            ),
            &if area.width < 100 {
                format!("{} inconcl. / {} unsupp.", counts[3], counts[4])
            } else {
                format!("{} inconclusive / {} unsupported", counts[3], counts[4])
            },
            TEXT,
        );
        let services = array(&self.snapshot["services"]);
        let rows: Vec<_> = services
            .iter()
            .map(|service| {
                let active = string(&service["active_state"]);
                Row::new(vec![
                    string(&service["role"]),
                    active,
                    bytes(&service["rss_bytes"]),
                    bytes(&service["memory_anon_bytes"]),
                    bytes(&service["memory_file_bytes"]),
                    bytes(&service["memory_current_bytes"]),
                    string(&service["fd_count"]),
                ])
            })
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(9),
                Constraint::Length(10),
                Constraint::Min(9),
                Constraint::Min(9),
                Constraint::Min(9),
                Constraint::Min(9),
                Constraint::Length(7),
            ],
        )
        .header(
            Row::new([
                "Service",
                "State",
                "RSS",
                "Anon",
                "File cache",
                "Cgroup",
                "FD",
            ])
            .style(Style::default().fg(MUTED))
            .bottom_margin(1),
        )
        .column_spacing(1)
        .row_highlight_style(Style::default().bg(SELECTED))
        .block(panel(" Services / memory scopes overlap; do not sum "));
        frame.render_stateful_widget(
            table,
            sections[1],
            &mut TableState::default()
                .with_selected((!services.is_empty()).then_some(self.service_selected)),
        );
        if services.is_empty() {
            let inner = panel("").inner(sections[1]);
            frame.render_widget(
                Paragraph::new(
                    "Service observations unavailable. See Findings for missing evidence.",
                )
                .style(Style::default().fg(AMBER))
                .wrap(Wrap { trim: false }),
                Rect {
                    y: inner.y.saturating_add(2),
                    height: inner.height.saturating_sub(2),
                    ..inner
                },
            );
        }
        if expanded {
            self.draw_service_detail(frame, sections[2]);
        }
        let bottom = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(sections[if expanded { 3 } else { 2 }]);
        let mut scans = Vec::new();
        for scan in array(&self.snapshot["scans"]).iter().take(2) {
            let incomplete = scan["complete"].as_bool() != Some(true);
            let status = string(&scan["status"]).to_ascii_uppercase();
            scans.push(Line::from(vec![
                Span::styled(
                    format!("{}  ", string(&scan["directory"])),
                    Style::default().fg(TEAL),
                ),
                Span::raw(format!(
                    "{} allocated  ",
                    bytes(&scan["observed_allocated_bytes"])
                )),
                Span::styled(
                    status,
                    Style::default().fg(if incomplete { AMBER } else { MUTED }),
                ),
            ]));
        }
        if scans.is_empty() {
            scans.push(Line::styled(
                "No storage scan evidence",
                Style::default().fg(AMBER),
            ));
        }
        scans.push(Line::styled(
            "Partial bytes are observed only, not a total.",
            Style::default().fg(MUTED),
        ));
        if bottom[0].height >= 7 {
            scans.push(Line::styled(
                "Old staging files do not prove orphaned data.",
                Style::default().fg(MUTED),
            ));
        }
        frame.render_widget(
            Paragraph::new(scans)
                .wrap(Wrap { trim: false })
                .block(panel(" Storage scans ")),
            bottom[0],
        );
        let mut collection = vec![
            Line::raw(format!(
                "Collection issues: {}",
                string(&self.snapshot["collection_issue_count"])
            )),
            Line::raw(format!(
                "Metrics: {}",
                string(&self.snapshot["metrics"]["status"])
            )),
            Line::styled(
                "Missing evidence: see Findings.",
                Style::default().fg(MUTED),
            ),
        ];
        if bottom[1].height >= 7 {
            collection.push(Line::raw(format!(
                "Capture start (Unix s): {}",
                string(&self.snapshot["capture"]["started_epoch_seconds"])
            )));
            collection.push(Line::raw(format!(
                "Receive-buffer model: {}",
                bytes(&self.capacity["receive_buffer_bytes"])
            )));
        }
        frame.render_widget(
            Paragraph::new(collection)
                .wrap(Wrap { trim: false })
                .block(panel(" Collection scope ")),
            bottom[1],
        );
    }

    fn draw_service_detail(&self, frame: &mut Frame, area: Rect) {
        let columns = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(area);
        let service = array(&self.snapshot["services"])
            .get(self.service_selected)
            .unwrap_or(&Value::Null);
        let lines = vec![
            section(&format!(
                "{} / {}",
                string(&service["role"]),
                string(&service["active_state"])
            )),
            Line::raw(format!(
                "Version: {} ({})",
                string(&service["version"]),
                string(&service["version_source"])
            )),
            Line::raw(format!(
                "Capture identity stable: {}",
                string(&service["stable_during_capture"])
            )),
            Line::raw(format!(
                "memory.max: {} / memory.high: {}",
                limit(&service["memory_max"]),
                limit(&service["memory_high"])
            )),
            Line::raw(format!(
                "Ancestor limits complete: {}",
                string(&service["ancestor_limits_complete"])
            )),
            Line::raw(format!(
                "FD: {} / limit {}",
                string(&service["fd_count"]),
                count_limit(&service["fd_soft_limit"])
            )),
            Line::raw(format!(
                "Recorded restarts: {}",
                string(&service["restart_count"])
            )),
            Line::raw(""),
            Line::styled(
                "Current service state does not establish historical uptime.",
                Style::default().fg(MUTED),
            ),
        ];
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(panel(" Selected service / Up Down ")),
            columns[0],
        );
        let mut scenarios = vec![
            section("RECEIVE-BUFFER MODEL"),
            Line::styled(
                "Configuration scenarios, not measured RAM.",
                Style::default().fg(MUTED),
            ),
            Line::raw(""),
        ];
        for scenario in array(&self.capacity["scenarios"]) {
            scenarios.push(Line::from(vec![
                Span::styled(
                    format!("{:>3} connections", string(&scenario["connections"])),
                    Style::default().fg(TEAL),
                ),
                Span::raw(format!(" x {} streams  ", string(&scenario["streams"]))),
                Span::styled(
                    bytes(&scenario["receive_buffer_bytes"]),
                    Style::default().fg(PURPLE),
                ),
            ]));
        }
        if array(&self.capacity["scenarios"]).is_empty() {
            scenarios.push(Line::styled(
                "Insufficient version or configuration evidence.",
                Style::default().fg(AMBER),
            ));
        }
        scenarios.extend([
            Line::raw(""),
            Line::raw(format!("Profile: {}", string(&self.capacity["profile"]))),
            Line::styled(
                format!("Source: {}", string(&self.capacity["profile_source"])),
                Style::default().fg(AMBER),
            ),
            Line::styled(
                "Press 3 for model evidence and limits.",
                Style::default().fg(MUTED),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(scenarios)
                .wrap(Wrap { trim: false })
                .block(panel(" Capacity planning ")),
            columns[1],
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        frame: &mut Frame,
        area: Rect,
        title: &str,
        value: &str,
        label: &str,
        foot: &str,
        color: Color,
    ) {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    value.to_owned(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Line::styled(label.to_owned(), Style::default().fg(MUTED)),
                Line::styled(foot.to_owned(), Style::default().fg(MUTED)),
            ])
            .block(panel(title)),
            area,
        );
    }

    fn draw_findings(&mut self, frame: &mut Frame, area: Rect) {
        let columns = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);
        let items: Vec<_> = self
            .findings
            .iter()
            .map(|finding| {
                let status = string(&finding["status"]);
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("{status} "),
                            Style::default().fg(status_color(&status)),
                        ),
                        Span::styled(string(&finding["title"]), Style::default().fg(TEXT)),
                    ]),
                    Line::styled(string(&finding["id"]), Style::default().fg(MUTED)),
                ])
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(&format!(" Findings / {} ", self.findings.len())))
                .highlight_style(Style::default().bg(SELECTED))
                .highlight_symbol("› "),
            columns[0],
            &mut ListState::default()
                .with_selected((!self.findings.is_empty()).then_some(self.finding_selected)),
        );
        let Some(finding) = self.findings.get(self.finding_selected) else {
            frame.render_widget(
                Paragraph::new("No findings available.").block(panel(" Evidence ")),
                columns[1],
            );
            return;
        };
        let status = string(&finding["status"]);
        let mut lines = vec![
            Line::styled(
                string(&finding["title"]),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                format!("{status}  /  {}", string(&finding["id"])),
                Style::default().fg(status_color(&status)),
            ),
            Line::raw(""),
            section("EVIDENCE"),
        ];
        if array(&finding["evidence"]).is_empty() {
            lines.push(Line::styled(
                "No complete evidence for this check.",
                Style::default().fg(AMBER),
            ));
        }
        for evidence in array(&finding["evidence"]) {
            lines.push(Line::raw(string(evidence)));
        }
        for (heading, key) in [
            ("INTERPRETATION", "impact"),
            ("RECOMMENDED NEXT STEP", "recommendation"),
            ("READ-ONLY VERIFICATION", "verification"),
        ] {
            lines.extend([
                Line::raw(""),
                section(heading),
                Line::raw(string(&finding[key])),
            ]);
        }
        self.detail(
            frame,
            columns[1],
            " Selected finding / PgUp PgDn to scroll ",
            lines,
        );
    }

    fn draw_capacity(&mut self, frame: &mut Frame, area: Rect) {
        let capacity = &self.capacity;
        let status = string(&capacity["status"]);
        let mut lines = vec![
            Line::styled(
                "RECEIVE-BUFFER MODEL",
                Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                "A configuration scenario, not measured RAM or total process memory.",
                Style::default().fg(AMBER),
            ),
            Line::raw(""),
            Line::from(vec![
                Span::styled(
                    format!("{}  ", bytes(&capacity["receive_buffer_bytes"])),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(status.clone(), Style::default().fg(status_color(&status))),
            ]),
            Line::raw(format!(
                "{} connections x {} streams x 132 MiB",
                string(&capacity["connections"]),
                string(&capacity["streams"])
            )),
            Line::raw(format!(
                "Profile: {}  /  {}",
                string(&capacity["profile"]),
                string(&capacity["profile_source"])
            )),
            Line::raw(""),
            section("WHAT-IF SCENARIOS"),
        ];
        if array(&capacity["scenarios"]).is_empty() {
            lines.push(Line::styled(
                "Unavailable: supported version and positive configuration values are required.",
                Style::default().fg(AMBER),
            ));
        }
        for scenario in array(&capacity["scenarios"]) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>3} connections", string(&scenario["connections"])),
                    Style::default().fg(TEAL),
                ),
                Span::raw(format!(
                    "  x {:>3} streams   ",
                    string(&scenario["streams"])
                )),
                Span::styled(
                    format!("{:>10}", bytes(&scenario["receive_buffer_bytes"])),
                    Style::default().fg(PURPLE),
                ),
            ]));
        }
        lines.extend([
            Line::raw(""),
            section("OBSERVED MEMORY / DIFFERENT, OVERLAPPING SCOPES"),
            Line::raw(format!(
                "Host available: {} / {} total",
                bytes(&self.snapshot["host"]["memory_available_bytes"]),
                bytes(&self.snapshot["host"]["memory_total_bytes"])
            )),
        ]);
        for service in array(&self.snapshot["services"]) {
            lines.push(Line::raw(format!(
                "{}: RSS {} | anon {} | file cache {} | cgroup {}",
                string(&service["role"]),
                bytes(&service["rss_bytes"]),
                bytes(&service["memory_anon_bytes"]),
                bytes(&service["memory_file_bytes"]),
                bytes(&service["memory_current_bytes"])
            )));
            lines.push(Line::styled(
                format!(
                    "  memory.max: {} | memory.high: {} | ancestor limits complete: {}",
                    limit(&service["memory_max"]),
                    limit(&service["memory_high"]),
                    string(&service["ancestor_limits_complete"])
                ),
                Style::default().fg(MUTED),
            ));
            lines.push(Line::styled(
                format!(
                    "  FD: {} / soft limit {} | inspection complete: {}",
                    string(&service["fd_count"]),
                    count_limit(&service["fd_soft_limit"]),
                    string(&service["fd_inspection_complete"])
                ),
                Style::default().fg(MUTED),
            ));
        }
        lines.extend([Line::raw(""), section("CONFIGURATION PROVENANCE")]);
        for key in ["root.max_connections", "root.max_concurrent_streams"] {
            let config = self.snapshot["config"]
                .get(key)
                .or_else(|| self.snapshot["config"].get(key.trim_start_matches("root.")))
                .unwrap_or(&Value::Null);
            lines.push(Line::raw(format!(
                "{key}: {} ({})",
                string(&config["value"]),
                string(&config["source"])
            )));
        }
        lines.extend([Line::raw(""), section("MODEL EVIDENCE")]);
        for evidence in array(&capacity["evidence"]) {
            lines.push(Line::raw(string(evidence)));
        }
        lines.extend([Line::raw(""), section("INTERPRETATION")]);
        for limitation in array(&capacity["limitations"]) {
            lines.push(Line::raw(string(limitation)));
        }
        lines.push(Line::raw(
            "Other workloads, ancestor limits and runtime overhead need separate evidence.",
        ));
        self.detail(frame, area, " Capacity / Up Down to scroll ", lines);
    }

    fn draw_compare(&mut self, frame: &mut Frame, area: Rect) {
        let Some(comparison) = &self.comparison else {
            let lines = vec![section("COMPARE TWO SNAPSHOTS"), Line::raw(""),
                Line::raw("This view contains one snapshot. Add an earlier snapshot to compare observations."),
                Line::raw(""), Line::styled("doctor-fibre-kit tui --snapshot before.json --compare after.json --mode temporal", Style::default().fg(TEAL)),
                Line::raw(""), Line::raw("Use --mode temporal for captures from the same host in time order."),
                Line::raw("Use --mode ab for separate environments; temporal rates stay unavailable."),
                Line::raw("The viewer reads files only and does not collect or refresh automatically.")];
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(panel(" Compare ")),
                area,
            );
            return;
        };
        let sections = Layout::vertical([
            Constraint::Length(6),
            Constraint::Min(4),
            Constraint::Length(if area.height >= 24 { 6 } else { 5 }),
        ])
        .split(area);
        let temporal = comparison["mode"] == "temporal";
        let mut summary = vec![Line::from(vec![
            Span::styled(
                if temporal {
                    "TEMPORAL"
                } else {
                    "A/B ENVIRONMENTS"
                },
                Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  |  interval {} s  |  {}",
                seconds(&comparison["elapsed_seconds"]),
                string(&comparison["clock_basis"])
            )),
        ])];
        let identities: Vec<_> = array(&comparison["services"])
            .iter()
            .map(|service| {
                format!(
                    "{}: {}",
                    string(&service["service"]),
                    string(&service["identity_match"])
                )
            })
            .collect();
        summary.push(Line::styled(
            format!(
                "Process identity: {}",
                if identities.is_empty() {
                    "unknown".into()
                } else {
                    identities.join("  |  ")
                }
            ),
            Style::default().fg(MUTED),
        ));
        summary.push(Line::styled(
            if temporal {
                "Before = --snapshot; After = --compare. Unknown continuity withholds rates."
            } else {
                "Before = input A; After = input B. Differences are not changes over time."
            },
            Style::default().fg(MUTED),
        ));
        let legacy_assumed = array(&comparison["services"])
            .iter()
            .any(|s| s["identity_match"] == "HeuristicLegacy");
        if legacy_assumed {
            summary.push(Line::styled(
                "Legacy same-host continuity is assumed, not verified; interval uses wall clock.",
                Style::default().fg(AMBER),
            ));
        } else if temporal && comparison["clock_basis"] != "monotonic" {
            summary.push(Line::styled(
                "Compatible monotonic timing unavailable. Inspect each metric's evidence below.",
                Style::default().fg(AMBER),
            ));
        }
        frame.render_widget(
            Paragraph::new(summary).block(panel(" Comparison scope ")),
            sections[0],
        );
        let value_width = (area.width.saturating_sub(35) / 4).clamp(7, 20);
        let rows: Vec<_> = self
            .comparison_rows
            .iter()
            .map(|entry| {
                let name = string(&entry.metric["metric"]);
                Row::new(vec![
                    entry.scope.clone(),
                    name.clone(),
                    fit_value(&metric_value(&name, &entry.metric["before"]), value_width),
                    fit_value(&metric_value(&name, &entry.metric["after"]), value_width),
                    fit_value(&delta_value(&name, &entry.metric["delta"]), value_width),
                    fit_value(&rate_value(&entry.metric["rate_per_second"]), value_width),
                ])
            })
            .collect();
        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(7),
                    Constraint::Min(21),
                    Constraint::Length(value_width),
                    Constraint::Length(value_width),
                    Constraint::Length(value_width),
                    Constraint::Length(value_width),
                ],
            )
            .header(
                Row::new(["Scope", "Metric", "Before", "After", "Delta", "Rate / s"])
                    .style(Style::default().fg(MUTED))
                    .bottom_margin(1),
            )
            .column_spacing(1)
            .row_highlight_style(Style::default().bg(SELECTED).fg(TEXT))
            .block(panel(
                " Observations / rates use each metric's native unit ",
            )),
            sections[1],
            &mut TableState::default()
                .with_selected((!self.comparison_rows.is_empty()).then_some(self.compare_selected)),
        );
        let detail = self
            .comparison_rows
            .get(self.compare_selected)
            .map(|entry| {
                let code = string(&entry.metric["reason_code"]);
                let explanation = terminal_text(
                    &report::translate(&format!("reason.{code}"), "en").unwrap_or(code),
                );
                format!(
                    "{} / {}: {}\nBefore: {}\nAfter: {}\nDelta: {}\nRate/s: {} (native units; n/a = unavailable)",
                    entry.scope,
                    string(&entry.metric["metric"]),
                    explanation,
                    string(&entry.metric["before"]),
                    string(&entry.metric["after"]),
                    if entry.metric["delta"].is_null() { "n/a".into() } else { string(&entry.metric["delta"]) },
                    if entry.metric["rate_per_second"].is_null() { "n/a".into() } else { string(&entry.metric["rate_per_second"]) }
                )
            })
            .unwrap_or_else(|| "No comparison observations available.".into());
        self.detail(
            frame,
            sections[2],
            " Selected metric / PgUp PgDn for exact values ",
            detail
                .lines()
                .map(|line| Line::styled(line.to_owned(), Style::default().fg(MUTED)))
                .collect(),
        );
    }

    fn detail(&mut self, frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) {
        let block = panel(title);
        let inner = block.inner(area);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.detail_max_scroll = paragraph
            .line_count(inner.width.max(1))
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize) as u16;
        self.detail_scroll = self.detail_scroll.min(self.detail_max_scroll);
        frame.render_widget(paragraph.scroll((self.detail_scroll, 0)).block(block), area);
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let width = area.width.saturating_sub(6).min(84);
        let height = area.height.saturating_sub(4).min(17);
        let modal = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, modal);
        frame.render_widget(
            Paragraph::new(vec![
                section("KEYBOARD"),
                Line::raw("1-4 / Tab / Shift-Tab     Switch screens"),
                Line::raw("Up / Down / j / k        Select a row; scroll Capacity"),
                Line::raw("PgUp / PgDn              Scroll details; page Overview services"),
                Line::raw("Home / End               Start or end of details / rows"),
                Line::raw("? / Esc                  Close this help"),
                Line::raw("q / Ctrl-C               Quit the viewer"),
                Line::raw(""),
                section("EVIDENCE FIRST"),
                Line::raw("This is a static snapshot, not a live monitor. No automatic refresh."),
                Line::raw("Unknown means missing evidence. There is no combined readiness score."),
                Line::raw("Raw host identifiers, paths and errors are omitted from this view."),
            ])
            .wrap(Wrap { trim: false })
            .block(panel(" Help ").border_style(Style::default().fg(TEAL))),
            modal,
        );
    }
}

fn panel(title: &str) -> Block<'static> {
    Block::default()
        .title(Span::styled(title.to_owned(), Style::default().fg(MUTED)))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().fg(TEXT).bg(PANEL))
}

fn section(text: &str) -> Line<'static> {
    Line::styled(
        text.to_owned(),
        Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
    )
}

fn status_color(status: &str) -> Color {
    match status {
        "PASS" => TEAL,
        "WARN" => AMBER,
        "FAIL" => RED,
        "UNSUPPORTED" => PURPLE,
        _ => MUTED,
    }
}

fn shift(selected: &mut usize, length: usize, direction: i16) {
    *selected = selected
        .saturating_add_signed(direction as isize)
        .min(length.saturating_sub(1));
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn string(value: &Value) -> String {
    match value {
        Value::String(value) => terminal_text(value),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => "unknown".into(),
    }
}

fn bytes(value: &Value) -> String {
    value
        .as_str()
        .and_then(|value| value.parse::<u64>().ok())
        .map(human_bytes)
        .unwrap_or_else(|| "unknown".into())
}

fn human_bytes(value: u64) -> String {
    for (unit, divisor) in [
        ("TiB", 1u64 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ] {
        if value >= divisor {
            return format!("{:.2} {unit}", value as f64 / divisor as f64);
        }
    }
    format!("{value} B")
}

fn metric_value(name: &str, value: &Value) -> String {
    if name.ends_with("_bytes") {
        bytes(value)
    } else {
        string(value)
    }
}

fn delta_value(name: &str, value: &Value) -> String {
    let Some(value) = value.as_str().and_then(|v| v.parse::<i128>().ok()) else {
        return "n/a".into();
    };
    if name.ends_with("_bytes") && value.unsigned_abs() <= u64::MAX as u128 {
        format!(
            "{}{}",
            if value < 0 {
                "-"
            } else if value > 0 {
                "+"
            } else {
                ""
            },
            human_bytes(value.unsigned_abs() as u64)
        )
    } else {
        format!("{value:+}")
    }
}

fn rate_value(value: &Value) -> String {
    value
        .as_str()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map(|v| {
            if v.abs() >= 1e6 {
                format!("{v:.3e}")
            } else {
                format!("{v:.3}")
            }
        })
        .unwrap_or_else(|| "n/a".into())
}

fn seconds(value: &Value) -> String {
    value
        .as_str()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map(|v| {
            format!("{v:.6}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned()
        })
        .unwrap_or_else(|| "unknown".into())
}

fn fit_value(value: &str, width: u16) -> String {
    if value.chars().count() <= width as usize {
        value.to_owned()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(width.saturating_sub(1) as usize)
                .collect::<String>()
        )
    }
}

fn limit(value: &Value) -> String {
    match value["kind"].as_str() {
        Some("unlimited") => "unlimited".into(),
        Some("finite") => bytes(&value["value"]),
        _ => "unknown".into(),
    }
}

fn count_limit(value: &Value) -> String {
    match value["kind"].as_str() {
        Some("unlimited") => "unlimited".into(),
        Some("finite") => string(&value["value"]),
        _ => "unknown".into(),
    }
}

fn terminal_text(value: &str) -> String {
    value.chars().map(|c| {
        if c.is_control() || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}') { '?' } else { c }
    }).collect()
}

fn sanitize_value(value: &mut Value) {
    match value {
        Value::String(text) => *text = terminal_text(text),
        Value::Array(values) => values.iter_mut().for_each(sanitize_value),
        Value::Object(values) => values.values_mut().for_each(sanitize_value),
        _ => {}
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
