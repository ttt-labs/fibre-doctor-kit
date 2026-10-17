use fibre_doctor_kit::model::*;
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};
fn tool() -> Command {
    Command::new(
        std::env::var_os("FIBRE_DOCTOR_KIT_BIN")
            .or_else(|| std::env::var_os("DOCTOR_FIBRE_KIT_BIN"))
            .or_else(|| std::env::var_os("TTT_FIBRE_BIN"))
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_fibre-doctor-kit").into()),
    )
}

#[test]
fn malformed_snapshot_errors_do_not_echo_secrets_or_terminal_sequences() {
    for field in ["status", "schema_version"] {
        let mut snapshot = serde_json::json!({"schema_version":1,"services":{}});
        let payload = "PRIVATE_SENTINEL\u{1b}]52;c;U0VDUkVU\u{7}\u{202e}";
        if field == "status" {
            snapshot["services"]["fibre.service"] = serde_json::json!({"status":payload});
        } else {
            snapshot["schema_version"] = payload.into();
        }
        let mut child = tool()
            .args(["doctor", "--snapshot", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&snapshot).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(!error.contains("PRIVATE_SENTINEL"));
        assert!(!error.contains(['\u{1b}', '\u{7}', '\u{202e}']));
    }
}

#[test]
fn stdin_legacy_and_json_output_are_machine_readable() {
    let mut child = tool()
        .args(["doctor", "--snapshot", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"timestamp_epoch":100,"services":{},"app_status":{"error":"LookupError"}}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let findings: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(findings
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["status"] == "INCONCLUSIVE"));
}

#[test]
fn expired_stream_is_an_error_and_does_not_print_a_healthy_report() {
    let mut child = tool()
        .args(["doctor", "--snapshot", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"error":"Expired"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn offline_report_pair_is_private_by_projection_and_does_not_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    let html = dir.path().join("report.html");
    let mut snapshot = SnapshotV1::default();
    snapshot.capture.host_hash = Some("SECRET_HOST_MARKER".into());
    snapshot.capture.started_epoch_seconds = Some(100.0);
    snapshot.issues.push(Issue {
        section: "private".into(),
        code: "private".into(),
        detail: "TOKEN_SECRET_MARKER".into(),
    });
    fs::write(&a, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    snapshot.capture.started_epoch_seconds = Some(200.0);
    fs::write(&b, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let args = [
        "report",
        "compare",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--out",
        html.to_str().unwrap(),
        "--lang",
        "en",
    ];
    let out = tool().args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(&html).unwrap();
    assert!(!text.contains("SECRET_HOST_MARKER"));
    assert!(!text.contains("TOKEN_SECRET_MARKER"));
    assert!(text.contains("FIBRE DOCTOR KIT"));
    assert!(text.contains("lang=\"en\""));
    assert!(!text.contains("data-lang"));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(html.with_extension("json")).unwrap()).unwrap();
    assert_eq!(report["report_schema_version"], 1);
    assert!(!tool().args(args).output().unwrap().status.success());
    assert_eq!(fs::read_to_string(&html).unwrap(), text);
}

#[cfg(unix)]
fn bounded_output(mut command: Command) -> std::process::Output {
    use std::time::{Duration, Instant};
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            // Do not turn a failed timeout assertion into another blocking wait.
            let grace = Instant::now() + Duration::from_millis(100);
            while Instant::now() < grace {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("offline command exceeded its test deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn snapshot_file_inputs_reject_fifos_without_waiting_for_a_writer() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("snapshot.fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let regular = dir.path().join("snapshot.json");
    fs::write(
        &regular,
        serde_json::to_vec(&SnapshotV1::default()).unwrap(),
    )
    .unwrap();
    for command_name in ["doctor", "capacity"] {
        let mut command = tool();
        command.arg(command_name).arg("--snapshot").arg(&fifo);
        let out = bounded_output(command);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("regular file"));
    }
    for (first, second) in [(&fifo, &regular), (&regular, &fifo)] {
        let mut command = tool();
        command
            .args(["report", "compare"])
            .arg(first)
            .arg(second)
            .arg("--out")
            .arg(dir.path().join("report.html"));
        let out = bounded_output(command);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("regular file"));
        assert!(!dir.path().join("report.html").exists());
    }
}

#[cfg(unix)]
#[test]
fn report_outputs_never_follow_existing_or_dangling_symlinks() {
    use std::os::unix::fs::symlink;
    for extension in ["html", "json"] {
        for target_exists in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let snapshot = dir.path().join("snapshot.json");
            fs::write(
                &snapshot,
                serde_json::to_vec(&SnapshotV1::default()).unwrap(),
            )
            .unwrap();
            let html = dir.path().join("report.html");
            let output_link = html.with_extension(extension);
            let target = dir.path().join("private-operator-file");
            if target_exists {
                fs::write(&target, "DO_NOT_CHANGE").unwrap();
            }
            symlink(&target, &output_link).unwrap();
            let mut command = tool();
            command
                .args(["report", "compare"])
                .arg(&snapshot)
                .arg(&snapshot)
                .arg("--out")
                .arg(&html);
            let out = bounded_output(command);
            assert!(!out.status.success());
            assert!(out.stdout.is_empty());
            assert!(fs::symlink_metadata(&output_link)
                .unwrap()
                .file_type()
                .is_symlink());
            if target_exists {
                assert_eq!(fs::read_to_string(&target).unwrap(), "DO_NOT_CHANGE");
            } else {
                assert!(!target.exists());
            }
        }
    }
}

#[test]
fn invalid_capacity_and_legacy_mode_arguments_fail_early() {
    for lang in ["zh-CN", "ko", "ja"] {
        let output = tool()
            .args(["doctor", "--snapshot", "missing", "--lang", lang])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
    }
    assert!(!tool()
        .args([
            "capacity",
            "--snapshot",
            "missing",
            "--target-connections",
            "0"
        ])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!tool()
        .args([
            "report",
            "compare",
            "a",
            "b",
            "--out",
            "x.html",
            "--allow-legacy-identity"
        ])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!tool()
        .args(["doctor", "--snapshot", "missing", "--profile", "v999"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn collect_output_selection_allows_other_options_but_requires_one_destination() {
    let root = tempfile::tempdir().unwrap();
    let output = tool()
        .args([
            "collect",
            "--stdout",
            "--sysroot",
            root.path().to_str().unwrap(),
            "--profile",
            "v10.4.0-mocha",
            "--app-home",
            "/lab/app",
            "--fibre-home",
            "/lab/fibre",
            "--fibre-metrics-url",
            "http://127.0.0.1:1/metrics",
            "--no-fd-inspection",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshot["capture"]["input_format"], "fixture_v1");
    assert!(!tool()
        .args(["collect", "--sysroot", root.path().to_str().unwrap()])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!tool()
        .args(["collect", "--stdout", "--out", "unused.json"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn tui_requires_a_terminal_before_reading_or_collecting() {
    for args in [
        vec!["tui"],
        vec!["tui", "--snapshot", "not-a-real-file.json"],
    ] {
        let out = tool().args(args).stdin(Stdio::null()).output().unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(
            error.contains("requires an interactive terminal"),
            "{error}"
        );
        assert!(!error.contains("Collecting"));
        assert!(!error.contains("cannot open snapshot"));
        assert!(!out.stderr.contains(&0x1b));
    }
}

#[test]
fn tui_rejects_ambiguous_sources_and_comparison_options() {
    for args in [
        vec!["tui", "--compare", "after.json"],
        vec![
            "tui",
            "--snapshot",
            "before.json",
            "--fibre-home",
            "/srv/fibre",
        ],
        vec![
            "tui",
            "--snapshot",
            "before.json",
            "--app-unit",
            "other.service",
        ],
        vec!["tui", "--snapshot", "before.json", "--mode", "temporal"],
        vec![
            "tui",
            "--snapshot",
            "before.json",
            "--compare",
            "after.json",
            "--allow-legacy-identity",
        ],
    ] {
        let out = tool().args(args).stdin(Stdio::null()).output().unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("requires an interactive terminal"));
    }
}
