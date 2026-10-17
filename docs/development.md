# Development and packaging

[Back to the quick start](../README.md)

Run the commands below from the repository root. Operators can use the prebuilt release without a Rust toolchain.

## Try it without a server

From the source checkout, generate a report from the bundled **synthetic** snapshots. These fixtures contain no validator data.

```sh
cargo run --locked -- doctor --snapshot tests/fixtures/legacy/snapshot-a.json
cargo run --locked -- capacity --snapshot tests/fixtures/legacy/snapshot-a.json --profile v10.4.0-mocha --target-connections 24
cargo run --locked -- report compare tests/fixtures/legacy/snapshot-a.json tests/fixtures/legacy/snapshot-b.json --mode temporal --allow-legacy-identity --profile v10.4.0-mocha --out example.html
```

Open `example.html` in a browser. `example.json` is the accompanying public report. No web server or internet connection is needed. Choose a new filename on subsequent runs: output files are not overwritten.

## Build, tests and packaging

The manifest declares Rust 1.89; recorded builds used Rust 1.99.0. Use `Cargo.lock`. Windows MSVC builds require the C++ build tools and Windows SDK; Windows GNU builds need compatible MinGW tools. Building a static Linux target also needs that Rust target and a compatible linker.

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
rustup target add x86_64-unknown-linux-musl
cargo build --locked --release --target x86_64-unknown-linux-musl
cp target/x86_64-unknown-linux-musl/release/fibre-doctor-kit ./fibre-doctor-kit-linux-x86_64-musl
./fibre-doctor-kit-linux-x86_64-musl --version
```

Cargo names the Linux executable `fibre-doctor-kit`; packaging gives it the filename `fibre-doctor-kit-linux-x86_64-musl` used in the installation examples. The `cp` command above creates that filename in your checkout. Run these Linux build commands on a development or lab machine, then copy the finished binary to the validator. Windows users can use the cross-compilation helper below and run the resulting Linux binary on Linux.

Windows helpers support native builds and musl cross-compilation without WSL:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1 -Release
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-linux-from-windows.ps1 -Tests
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-source.ps1
```

The Linux helper compiles test executables; run them in a Linux lab to validate runtime behavior. Binary packaging requires both release targets and an unused output directory. Source packaging excludes local snapshots, SSH artifacts, build outputs and Git history. The build helpers do not install a missing Rust toolchain or compiler automatically.

When running cross-compiled CLI tests, set `FIBRE_DOCTOR_KIT_BIN` to the executable under test. `DOCTOR_FIBRE_KIT_BIN` and `TTT_FIBRE_BIN` remain compatibility aliases, in that fallback order.

Synthetic legacy fixtures are committed under `tests/fixtures/legacy/`. `--sysroot` fixtures use mirrored Linux files plus `run/ttt-fibre/<unit>.show`, `clock.json`, `staging.scan.json` and `shards.scan.json`. The `run/ttt-fibre/` fixture directory is retained for compatibility; no live Unix metadata is inferred on Windows. `tests/linux_lab.py` is for a **disposable Alpine VM only**, never an operational node. `scripts/verify-report.py` uses Playwright to verify offline report interactions; Python is a development dependency only.

See the [validation record](validation.md) for actual test counts and limitations. Lab checks do not establish production-validator compatibility. The supplied GitHub Actions workflow runs formatting, tests and lint/build checks when enabled in a repository; a workflow file is not evidence of a completed hosted run.

## Project layout

| Path | Contents |
| --- | --- |
| `src/model.rs`, `src/parser.rs` | Snapshot types and legacy conversion |
| `src/analyzer.rs` | Doctor, capacity and comparison logic |
| `src/collector/` | Bounded Linux collection and fixture readers |
| `src/report.rs`, `assets/` | Public projection and offline English report |
| `src/tui/` | Terminal lifecycle, keyboard navigation and snapshot views |
| `tests/`, `scripts/` | Regression fixtures, lab/UI verification and packaging |
| `docs/` | Operations, schema and validation notes |

Contributions should include a reproducible case with synthetic or sanitized data and relevant tests. Do not submit raw operational snapshots, credentials or private keys. Licensed under [Apache-2.0](../LICENSE).

## Terminal UI verification

The TUI uses [Ratatui](https://docs.rs/ratatui/0.29.0/ratatui/) with Crossterm. Views render only the existing public evidence projection. `cargo test --locked` includes small-terminal layouts, keyboard navigation, missing evidence, exact comparison values, control-character sanitization and private-field exclusion.

On Linux, exercise an actual PTY after building:

```sh
python3 tests/tui_pty.py target/release/fibre-doctor-kit
```

This opens only bundled synthetic snapshots. It checks screen navigation, resizing, Q/Ctrl+C/SIGTERM/SIGHUP exits and restoration of terminal modes, alternate screen and cursor. CI runs it for the Linux native and musl builds.

To reproduce the README screenshots on a Windows development machine, install Python with Playwright and use an installed Chromium browser. Download and extract the pinned [`@xterm/xterm` 5.5.0 package](https://registry.npmjs.org/@xterm/xterm/-/xterm-5.5.0.tgz); put its `lib/xterm.js` and `css/xterm.css` together in a local directory such as `.lab/xterm-5.5.0`. Then run from the repository root:

```powershell
python scripts/verify-tui.py --binary target/release/fibre-doctor-kit.exe --browser 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe' --xterm-dir .lab/xterm-5.5.0 --out reports/tui-preview
```

The script starts the real Windows binary in ConPTY, sends navigation keys and renders its emitted terminal bytes with local xterm.js in an offline browser. Screenshots capture that terminal output; the application layout is not recreated in HTML. The JSON verification record checks all four screens, 80-column resizing, Q/Ctrl+C exit, cursor/alternate-screen restoration and absence of browser network requests. The published `tui-overview.png` and `tui-compare.png` show a 140 × 42 terminal with the synthetic legacy fixtures and an explicitly assumed capacity profile. They contain no production node measurements. Python, Playwright and xterm.js are development tools, not TUI runtime dependencies.
