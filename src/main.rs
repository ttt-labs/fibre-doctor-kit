use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};
use ttt_fibre::{
    analyzer::{self, DoctorOptions},
    collector::{self, CollectOptions},
    model::*,
    parser, report,
};

#[derive(Parser)]
#[command(
    name = "ttt-fibre",
    version,
    about = "Read-only Celestia Fibre diagnostics and offline reports"
)]
struct Cli {
    #[arg(long, global=true, default_value="en", value_parser=["en","zh-CN","ko","ja"])]
    lang: String,
    /// Explicit version assumption; never recorded as observed binary version.
    #[arg(long, global = true)]
    profile: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Collect(CollectArgs),
    Doctor {
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        json: bool,
    },
    Capacity {
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        target_connections: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Report {
        #[command(subcommand)]
        command: ReportCommand,
    },
}
#[derive(Args)]
#[group(skip)]
#[command(group(clap::ArgGroup::new("output").required(true).args(["out", "stdout"])))]
struct CollectArgs {
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    stdout: bool,
    #[arg(long, default_value = "celestia-appd.service")]
    app_unit: String,
    #[arg(long, default_value = "fibre.service")]
    fibre_unit: String,
    #[arg(long)]
    app_home: Option<PathBuf>,
    #[arg(long)]
    fibre_home: Option<PathBuf>,
    /// Fixture root: disables live commands/network and uses mocked Linux observations.
    #[arg(long)]
    sysroot: Option<PathBuf>,
    #[arg(long = "fibre-metrics-file")]
    metrics_file: Option<PathBuf>,
    #[arg(long = "fibre-metrics-url", alias = "fibre-prom-url")]
    metrics_url: Option<String>,
    #[arg(long)]
    otel_config: Option<PathBuf>,
    #[arg(long)]
    no_fd_inspection: bool,
}
#[derive(Subcommand)]
enum ReportCommand {
    Compare {
        snapshot_a: PathBuf,
        snapshot_b: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long,value_enum,default_value_t=CompareMode::Ab)]
        mode: CompareMode,
        /// Explicitly assume legacy snapshots came from the same host (temporal mode only).
        #[arg(long)]
        allow_legacy_identity: bool,
    },
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CompareMode {
    Ab,
    Temporal,
}

fn read_input(path: &str) -> Result<SnapshotV1, String> {
    if path == "-" {
        parser::read_snapshot(io::stdin().lock())
    } else {
        parser::read_snapshot(File::open(path).map_err(|e| format!("cannot open snapshot: {e}"))?)
    }
}
fn json_bytes(value: &impl serde::Serialize) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err("serialized output exceeds 8 MiB".into());
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    // Avoid overwriting input, existing reports, symlinks, or unrelated operator files.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|e| format!("cannot create output (choose an unused path): {e}"))?;
    file.write_all(bytes)
        .map_err(|e| format!("output write failed: {e}"))
}
fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    io::stdout()
        .lock()
        .write_all(&json_bytes(value)?)
        .map_err(|e| e.to_string())
}
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("__scan-worker") => return collector::worker_entry(&args[2..]),
        Some("__collect-worker") => return collector::collection_worker_entry(&args[2..]),
        _ => {}
    }
    let cli = Cli::parse();
    if cli.profile.as_deref().is_some_and(|p| p != "v10.4.0-mocha") {
        return Err("unsupported profile; v0.1 supports v10.4.0-mocha".into());
    }
    match cli.command {
        Command::Collect(a) => {
            if a.out.as_ref().is_some_and(|out| out.exists()) {
                return Err("snapshot output already exists; choose an unused path".into());
            }
            let opts = CollectOptions {
                sysroot: a.sysroot,
                app_unit: a.app_unit,
                fibre_unit: a.fibre_unit,
                app_home: a.app_home,
                fibre_home: a.fibre_home,
                metrics_file: a.metrics_file,
                metrics_url: a.metrics_url,
                otel_config: a.otel_config,
                profile: cli.profile,
                inspect_open_fds: !a.no_fd_inspection,
            };
            let snapshot = collector::collect(&opts)?;
            let bytes = json_bytes(&snapshot)?;
            if let Some(out) = a.out {
                write_new(&out, &bytes)?;
            } else {
                io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .map_err(|e| e.to_string())?;
            }
        }
        Command::Doctor { snapshot, json } => {
            let snapshot = read_input(&snapshot)?;
            let options = DoctorOptions {
                profile: cli.profile.clone(),
                ..DoctorOptions::default()
            };
            let findings = analyzer::doctor(&snapshot, &options);
            if json {
                print_json(&report::public_findings(&findings))?;
            } else {
                print!("{}", report::render_doctor_text(&findings, &cli.lang)?);
            }
        }
        Command::Capacity {
            snapshot,
            target_connections,
            json,
        } => {
            if target_connections == Some(0) {
                return Err("target connections must be positive".into());
            }
            let snapshot = read_input(&snapshot)?;
            let model = analyzer::capacity(&snapshot, cli.profile.as_deref(), target_connections);
            if json {
                print_json(&report::public_capacity(&model))?;
            } else {
                print!("{}", report::render_capacity_text(&model, &cli.lang)?);
            }
        }
        Command::Report {
            command:
                ReportCommand::Compare {
                    snapshot_a,
                    snapshot_b,
                    out,
                    mode,
                    allow_legacy_identity,
                },
        } => {
            if mode == CompareMode::Ab && allow_legacy_identity {
                return Err("--allow-legacy-identity requires --mode temporal".into());
            }
            if out.extension().and_then(|s| s.to_str()) != Some("html") {
                return Err("report --out must end in .html".into());
            }
            let sidecar = out.with_extension("json");
            if out.exists() || sidecar.exists() {
                return Err("report output already exists; choose an unused .html path".into());
            }
            let a = parser::read_snapshot(File::open(snapshot_a).map_err(|e| e.to_string())?)?;
            let b = parser::read_snapshot(File::open(snapshot_b).map_err(|e| e.to_string())?)?;
            let comparison =
                analyzer::compare(&a, &b, mode == CompareMode::Temporal, allow_legacy_identity);
            let options = DoctorOptions {
                profile: cli.profile.clone(),
                ..DoctorOptions::default()
            };
            let fa = analyzer::doctor(&a, &options);
            let fb = analyzer::doctor(&b, &options);
            let ca = analyzer::capacity(&a, cli.profile.as_deref(), None);
            let cb = analyzer::capacity(&b, cli.profile.as_deref(), None);
            let html =
                report::render_comparison(&a, &b, &comparison, &fa, &fb, &ca, &cb, &cli.lang)?;
            let public = report::public_comparison(&a, &b, &comparison, &fa, &fb, &ca, &cb);
            let sidecar_bytes = json_bytes(&public)?;
            write_new(&out, html.as_bytes())?;
            write_new(&sidecar, &sidecar_bytes)?;
            eprintln!("Created offline HTML report and public JSON sidecar.");
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("ttt-fibre: {error}");
        std::process::exit(2);
    }
}
