use doctor_fibre_kit::model::*;
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};
fn tool() -> Command {
    Command::new(
        std::env::var_os("DOCTOR_FIBRE_KIT_BIN")
            .or_else(|| std::env::var_os("TTT_FIBRE_BIN"))
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_doctor-fibre-kit").into()),
    )
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
    assert!(text.contains("DOCTOR FIBRE KIT"));
    assert!(text.contains("lang=\"en\""));
    assert!(!text.contains("data-lang"));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(html.with_extension("json")).unwrap()).unwrap();
    assert_eq!(report["report_schema_version"], 1);
    assert!(!tool().args(args).output().unwrap().status.success());
    assert_eq!(fs::read_to_string(&html).unwrap(), text);
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
