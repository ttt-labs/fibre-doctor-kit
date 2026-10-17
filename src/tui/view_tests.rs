use super::*;
use crate::{model::*, parser};
use ratatui::{backend::TestBackend, Terminal};
use serde_json::json;

fn legacy_pair() -> (SnapshotV1, SnapshotV1) {
    (
        parser::parse_snapshot(include_bytes!(
            "../../tests/fixtures/legacy/snapshot-a.json"
        ))
        .unwrap(),
        parser::parse_snapshot(include_bytes!(
            "../../tests/fixtures/legacy/snapshot-b.json"
        ))
        .unwrap(),
    )
}

fn view() -> ViewState {
    let (before, after) = legacy_pair();
    ViewState::new(
        &after,
        Some(&before),
        Some(analyzer::CAPACITY_PROFILE),
        true,
        true,
    )
}

fn press(app: &mut ViewState, code: KeyCode) -> bool {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn draw(app: &mut ViewState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .chunks(width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn metric<'a>(app: &'a ViewState, scope: &str, name: &str) -> &'a Value {
    &app.comparison_rows
        .iter()
        .find(|row| row.scope == scope && row.metric["metric"] == name)
        .unwrap_or_else(|| panic!("missing metric {scope}/{name}"))
        .metric
}

#[test]
fn every_screen_renders_at_large_and_standard_terminal_sizes() {
    let mut app = view();
    for (width, height) in [(140, 42), (80, 24)] {
        for (tab, expected) in [
            (0, "Host memory"),
            (1, "EVIDENCE"),
            (2, "RECEIVE-BUFFER MODEL"),
            (3, "TEMPORAL"),
        ] {
            app.set_tab(tab);
            let text = draw(&mut app, width, height);
            assert!(
                text.contains("FIBRE DOCTOR KIT"),
                "{width}x{height} tab {tab}:\n{text}"
            );
            assert!(
                text.contains(expected),
                "{width}x{height} tab {tab}:\n{text}"
            );
            assert!(text.contains("quit"));
        }
    }
}

#[test]
fn terminal_resize_recovers_the_selected_screen_and_row() {
    let mut app = view();
    app.set_tab(1);
    press(&mut app, KeyCode::Down);
    let selected = app.finding_selected;
    let mut terminal = Terminal::new(TestBackend::new(140, 42)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal.backend_mut().resize(50, 12);
    terminal.resize(Rect::new(0, 0, 50, 12)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("Terminal size"));
    assert!(text.contains("70 x 18"));
    assert_eq!(app.tab, 1);
    assert_eq!(app.finding_selected, selected);
    terminal.backend_mut().resize(140, 42);
    terminal.resize(Rect::new(0, 0, 140, 42)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("EVIDENCE"));
    assert!(!text.contains("Terminal size"));
    assert_eq!(app.finding_selected, selected);
    // Even terminals smaller than the fallback's text must not panic.
    draw(&mut app, 1, 1);
}

#[test]
fn keyboard_navigation_clamps_rows_and_resets_detail_scroll_between_screens() {
    let mut app = view();
    assert!(!press(&mut app, KeyCode::Up));
    assert_eq!(app.service_selected, 0);
    press(&mut app, KeyCode::End);
    assert_eq!(
        app.service_selected,
        array(&app.snapshot["services"]).len() - 1
    );
    press(&mut app, KeyCode::Down);
    assert_eq!(app.service_selected, 1);
    press(&mut app, KeyCode::Home);
    assert_eq!(app.service_selected, 0);

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.tab, 1);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.finding_selected, 1);
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.finding_selected, 0);
    draw(&mut app, 80, 24);
    press(&mut app, KeyCode::End);
    assert!(app.detail_scroll > 0);
    assert_eq!(app.detail_scroll, app.detail_max_scroll);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.detail_scroll, 0);
    assert_eq!(app.finding_selected, 1);

    press(&mut app, KeyCode::Char('3'));
    draw(&mut app, 80, 24);
    press(&mut app, KeyCode::PageDown);
    assert!(app.detail_scroll > 0);
    press(&mut app, KeyCode::Home);
    assert_eq!(app.detail_scroll, 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.detail_scroll, 1);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.tab, 3);
    assert_eq!(app.detail_scroll, 0);
    press(&mut app, KeyCode::End);
    assert_eq!(app.compare_selected, app.comparison_rows.len() - 1);
    press(&mut app, KeyCode::Home);
    assert_eq!(app.compare_selected, 0);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.tab, 0);
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.tab, 3);
}

#[test]
fn help_blocks_navigation_and_exit_keys_restore_control() {
    let mut app = view();
    assert!(!press(&mut app, KeyCode::Char('?')));
    assert!(app.help);
    let text = draw(&mut app, 140, 42);
    assert!(text.contains("KEYBOARD"));
    assert!(text.contains("No automatic refresh"));
    assert!(!press(&mut app, KeyCode::Tab));
    assert_eq!(app.tab, 0);
    assert!(!press(&mut app, KeyCode::Esc));
    assert!(!app.help);
    assert!(press(&mut app, KeyCode::Esc));
    assert!(press(&mut app, KeyCode::Char('q')));
    assert!(press(&mut app, KeyCode::Char('Q')));
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    assert!(!app.handle_key(KeyEvent::new_with_kind(
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    )));
}

#[test]
fn missing_memory_and_metrics_remain_unknown_and_inconclusive() {
    let mut app = ViewState::new(&SnapshotV1::default(), None, None, false, false);
    assert!(app.snapshot["host"]["memory_available_bytes"].is_null());
    let host = app
        .findings
        .iter()
        .find(|finding| finding["id"] == "host_memory")
        .unwrap();
    assert_eq!(host["status"], "INCONCLUSIVE");
    assert_eq!(app.capacity["status"], "INCONCLUSIVE");
    assert!(app.capacity["receive_buffer_bytes"].is_null());
    let overview = draw(&mut app, 140, 42);
    assert!(overview.contains("unknown"));
    assert!(!overview.contains("0 B"));
    app.set_tab(2);
    let capacity = draw(&mut app, 140, 42);
    assert!(capacity.contains("INCONCLUSIVE"));
    assert!(capacity.contains("Unavailable:"));
    assert!(!capacity.contains("0 B"));
    assert_eq!(bytes(&Value::Null), "unknown");
    assert_eq!(bytes(&json!("0")), "0 B");
    assert_eq!(limit(&json!({"kind":"unlimited"})), "unlimited");
    assert_eq!(limit(&json!({"kind":"missing"})), "unknown");
}

#[test]
fn terminal_projection_omits_private_fields_and_arbitrary_labels() {
    let (mut before, _) = legacy_pair();
    let marker = "PRIVATE_HOST_TOKEN_PATH_MARKER";
    before.capture.host_hash = Some(marker.into());
    before.capture.boot_hash = Some(marker.into());
    before.capture.supplied_provenance = Some(marker.into());
    before.issues.push(Issue {
        section: marker.into(),
        code: marker.into(),
        detail: marker.into(),
    });
    before
        .services
        .get_mut("fibre.service")
        .unwrap()
        .identity
        .cgroup_path = Some(marker.into());
    before
        .fibre_config
        .get_mut("root.max_connections")
        .unwrap()
        .evidence = marker.into();
    before.metrics.source = marker.into();
    before.metrics.samples.push(MetricSample {
        name: "fibre_store_occupancy_bytes".into(),
        labels: [("secret".into(), marker.into())].into_iter().collect(),
        value: 42.0,
        ..MetricSample::default()
    });
    let mut app = ViewState::new(
        &before,
        Some(&before),
        Some(analyzer::CAPACITY_PROFILE),
        false,
        false,
    );
    let projected = format!(
        "{:?}{:?}{:?}{:?}",
        app.snapshot, app.findings, app.capacity, app.comparison
    );
    assert!(!projected.contains(marker));
    for tab in 0..4 {
        app.set_tab(tab);
        assert!(!draw(&mut app, 140, 42).contains(marker));
    }
}

#[test]
fn terminal_text_cannot_inject_controls_or_hidden_direction_changes() {
    let hostile = "safe\u{1b}[2J\r\n\t\u{7}\u{85}\u{202e}\u{2066}\u{200b}\u{feff}end";
    let expected = "safe?[2J?????????end";
    assert_eq!(terminal_text(hostile), expected);
    let mut nested = json!({"values": [hostile, {"text": hostile}], "number": 42});
    sanitize_value(&mut nested);
    assert_eq!(nested["values"][0], expected);
    assert_eq!(nested["values"][1]["text"], expected);
    assert_eq!(nested["number"], 42);
    assert_eq!(
        terminal_text("Fibre / 한국어 / 日本語"),
        "Fibre / 한국어 / 日本語"
    );
}

#[test]
fn legacy_temporal_rates_require_explicit_assumption_and_preserve_interval() {
    let (before, after) = legacy_pair();
    let strict = ViewState::new(&after, Some(&before), None, true, false);
    assert!(metric(&strict, "app", "cpu_usage_ns")["delta"].is_null());
    assert!(metric(&strict, "app", "cpu_usage_ns")["rate_per_second"].is_null());
    let mut accepted = ViewState::new(&after, Some(&before), None, true, true);
    let comparison = accepted.comparison.as_ref().unwrap();
    let interval: f64 = comparison["elapsed_seconds"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((interval - 83.4120102).abs() < 0.000001);
    assert_eq!(comparison["clock_basis"], "estimated_wall_clock");
    for service in array(&comparison["services"]) {
        assert_eq!(service["identity_match"], "HeuristicLegacy");
        assert!(array(&service["reason_codes"]).contains(&json!("legacy_accepted")));
    }
    assert_eq!(
        metric(&accepted, "app", "cpu_usage_ns")["delta"],
        "8000000000"
    );
    assert!(metric(&accepted, "app", "cpu_usage_ns")["rate_per_second"].is_string());
    accepted.set_tab(3);
    let text = draw(&mut accepted, 140, 42);
    assert!(text.contains("HeuristicLegacy"));
    assert!(text.contains("estimated_wall_clock"));
    assert!(text.contains("83.412"));
}

#[test]
fn ab_mode_keeps_gauge_differences_but_never_temporal_counter_rates() {
    let (before, after) = legacy_pair();
    let mut app = ViewState::new(&after, Some(&before), None, false, false);
    assert_eq!(app.comparison.as_ref().unwrap()["mode"], "side_by_side");
    assert!(app.comparison.as_ref().unwrap()["elapsed_seconds"].is_null());
    assert_eq!(
        metric(&app, "app", "memory_current_bytes")["delta"],
        "67108864"
    );
    assert!(metric(&app, "app", "cpu_usage_ns")["delta"].is_null());
    assert!(app
        .comparison_rows
        .iter()
        .all(|row| row.metric["rate_per_second"].is_null()));
    app.set_tab(3);
    let text = draw(&mut app, 140, 42);
    assert!(text.contains("A/B ENVIRONMENTS"));
    assert!(text.contains("not changes over time"));
}

#[test]
fn comparison_without_second_input_gives_real_cli_syntax() {
    let (_, snapshot) = legacy_pair();
    let mut app = ViewState::new(&snapshot, None, None, false, false);
    app.set_tab(3);
    let text = draw(&mut app, 140, 42);
    assert!(text.contains("COMPARE TWO SNAPSHOTS"));
    assert!(text.contains("--snapshot before.json --compare after.json --mode temporal"));
    assert!(!text.contains("--before"));
}

#[test]
fn capacity_scenarios_stay_separate_from_measured_memory() {
    let mut app = view();
    app.set_tab(2);
    let text = draw(&mut app, 140, 42);
    assert!(text.contains("not measured RAM or total process memory"));
    assert!(text.contains("26.81 GiB"));
    assert!(text.contains("40.22 GiB"));
    assert!(text.contains("53.62 GiB"));
    assert!(text.contains("DEFAULT_ASSUMED"));
    assert!(text.contains("memory.max: unlimited"));
    assert_eq!(
        delta_value("memory_current_bytes", &json!("-4096")),
        "-4.00 KiB"
    );
    assert_eq!(delta_value("memory_current_bytes", &Value::Null), "n/a");
    assert_eq!(rate_value(&Value::Null), "n/a");
}

#[test]
fn capacity_displays_fd_soft_limit_as_a_count_instead_of_memory_bytes() {
    let (_, mut snapshot) = legacy_pair();
    let fibre = snapshot.services.get_mut("fibre.service").unwrap();
    fibre.fd_count = Some(128);
    fibre.fd_soft_limit = Some(MemoryLimit::Finite(1024));
    let mut app = ViewState::new(
        &snapshot,
        None,
        Some(analyzer::CAPACITY_PROFILE),
        false,
        false,
    );
    app.set_tab(2);
    let text = draw(&mut app, 140, 42);
    assert!(text.contains("FD: 128 / soft limit 1024"));
    assert!(!text.contains("soft limit 1.00 KiB"));
    assert_eq!(count_limit(&json!({"kind": "unlimited"})), "unlimited");
    assert_eq!(count_limit(&Value::Null), "unknown");
}

#[test]
fn absent_storage_scans_show_actual_status_instead_of_partial_measurements() {
    let snapshot = SnapshotV1 {
        scans: vec![
            ScanResult {
                directory: "staging".into(),
                status: CollectionStatus::Missing,
                ..ScanResult::default()
            },
            ScanResult {
                directory: "shards".into(),
                status: CollectionStatus::Unsupported,
                ..ScanResult::default()
            },
        ],
        ..SnapshotV1::default()
    };
    let mut app = ViewState::new(&snapshot, None, None, false, false);
    let text = draw(&mut app, 140, 42);
    assert!(text.contains("staging  unknown allocated  MISSING"));
    assert!(text.contains("shards  unknown allocated  UNSUPPORTED"));
    assert!(!text.contains("PARTIAL"));
    assert!(!text.contains("0 B allocated"));
}

#[test]
fn end_scroll_reaches_final_verification_after_word_wrapping_long_evidence() {
    let mut app = view();
    app.set_tab(1);
    // Each token fits in the detail column, but two cannot fit on one row.
    // Counting character length / width undercounts these word-wrapped rows.
    app.findings[0]["evidence"] = json!(["long_observation_token_for_test ".repeat(45)]);
    app.findings[0]["verification"] = json!("VERIFICATION_REACHED_END");
    let initial = draw(&mut app, 80, 24);
    assert!(!initial.contains("VERIFICATION_REACHED_END"));
    press(&mut app, KeyCode::End);
    let scrolled = draw(&mut app, 80, 24);
    assert!(scrolled.contains("VERIFICATION_REACHED_END"), "{scrolled}");
    assert_eq!(app.detail_scroll, app.detail_max_scroll);
    press(&mut app, KeyCode::Home);
    assert!(!draw(&mut app, 80, 24).contains("VERIFICATION_REACHED_END"));
}

#[test]
fn compact_comparison_details_scroll_to_exact_values() {
    let mut app = view();
    app.set_tab(3);
    app.compare_selected = app
        .comparison_rows
        .iter()
        .position(|row| row.scope == "app" && row.metric["metric"] == "cpu_usage_ns")
        .unwrap();
    let initial = draw(&mut app, 80, 24);
    assert!(!initial.contains("Rate/s:"));
    for _ in 0..10 {
        press(&mut app, KeyCode::PageDown);
    }
    let scrolled = draw(&mut app, 80, 24);
    assert!(scrolled.contains("Rate/s:"), "{scrolled}");
    assert!(scrolled.contains("Delta: 8000000000"), "{scrolled}");
    press(&mut app, KeyCode::Down);
    assert_eq!(app.detail_scroll, 0);
}
