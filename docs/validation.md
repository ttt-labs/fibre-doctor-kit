# Validation record

## v0.2.0 terminal UI

Local checks on 2026-10-17:

- Windows: 73 library tests and 7 CLI tests passed; formatting and Clippy passed, and the release binary built.
- Real Windows terminal: ConPTY exercised all four views, arrow/Tab navigation, resizing to 80 × 24, Q/Ctrl+C exit and restoration of the normal screen and cursor. Screenshots show synthetic fixtures only; the capture browser made no network requests.
- New view tests cover unknown-versus-zero evidence, legacy identity assumptions, independent A/B comparisons, control-character sanitization, private-field exclusion, small layouts and scrolling to exact comparison values.
- Linux musl builds and Clippy are checked separately. The CI workflow runs Linux native and musl unit/CLI tests plus `tests/tui_pty.py`, which checks real terminal navigation, resize recovery, terminal mode restoration and Q/Ctrl+C/SIGTERM/SIGHUP exits. Consult the release commit's [CI run](https://github.com/ttt-labs/fibre-doctor-kit/actions/workflows/ci.yml) for the hosted result; cross-compilation alone does not establish Linux runtime behavior.

The new TUI reads one capture and then renders without background collection. Existing collector bounds, snapshot schema and analyzer rules are retained. No production validator was accessed for the TUI implementation or screenshots. The historical collector lab results below apply to their recorded revisions.

## Doctor Fibre Kit English edition

Checked on 2026-10-16 after the package, CLI and repository rename:

- Windows: 58 library tests and 5 CLI tests passed; Clippy passed and the release binary built.
- Linux musl: Clippy passed, the release binary built and Linux test executables compiled. Runtime coverage for this revision is reported by its GitHub Actions run; compilation alone is not a Linux test pass.
- Offline report: English content and branding, finding expansion and a 390px mobile layout passed in Edge/Chromium, with no JavaScript errors or HTTP(S) requests. The generated example and all three published screenshots use bundled synthetic fixtures.
- Snapshot schema and calculation rules are unchanged. `--lang en` remains accepted; retired language choices are rejected. The fixture path and older binary override variable remain compatibility inputs.

Recorded on 2026-10-16 using Windows and a disposable Linux VM, before the Doctor Fibre Kit rename and English-only report update. These results cover that revision's local builds, tests and lab integration; they do not establish production-validator compatibility, performance or validation of later changes.

## Build and tests

| Check | Result |
| --- | --- |
| Windows x86_64 GNU, Rust 1.99.0 | 58 library + 5 CLI tests passed |
| Linux x86_64 musl, Alpine 3.24.2 / kernel 6.18.52 | 78 library + 5 CLI tests passed |
| `cargo fmt --all -- --check` | Passed |
| Clippy, Windows and Linux targets, all targets, warnings denied | Passed |
| Linux ELF | ELF64, x86-64, static PIE; no PT_INTERP and no DT_NEEDED |
| Windows PE imports | Windows system DLLs only; no separate GNU runtime DLL needed |
| Offline HTML, Edge/Chromium via Playwright | The previous four-language viewer passed language switches, finding expansion and 390px layout checks; zero page errors or HTTP(S) requests |

The recorded Linux run includes two legacy-input cases that did not execute assertions. Legacy regressions now use bundled synthetic fixtures and require no external operational data.

Legacy regression checks cover an interval of **83.4120102 seconds**. The adapter preserves missing version and boot identity, marks configuration as `CONFIG_FILE`, and preserves the `app_status` collection error. Temporal comparison requires explicit same-host legacy opt-in and labels identity `HeuristicLegacy`. Capacity under the explicitly selected profile yields **26.8125 / 40.21875 / 53.625 GiB** for 16 / 24 / 32 connections and 13 streams. Those are receive-buffer models, not measured process peaks.

## Linux integration

`tests/linux_lab.py` ran inside an isolated QEMU TCG VM with 2 virtual CPUs and 1536 MiB RAM. Alpine uses OpenRC, so the harness provided mock systemd discovery while using real processes, `/proc`, cgroup v2, descriptors, storage metadata and HTTP sockets. This validates the collector integration without claiming an end-to-end test against a real Ubuntu systemd deployment or Celestia binaries.

Verified cases:

- Service identity before/after capture; real FD counts and cgroup/ancestor reads.
- Flat Fibre-home staging/shards scanning; sparse-file apparent/allocated distinction, hardlink deduplication, symlink rejection, unprivileged permission error.
- Aged staging files both held open and closed; `--no-fd-inspection` retains unknown state.
- Exact Prometheus counter `9007199254740993`, loopback RPC, bounded oversized HTTP rejection.
- Temporal public HTML/JSON generation with operational paths removed.
- A deliberately blocked FIFO read in the collection worker: outer supervisor returned the last complete checkpoint in **20.024 seconds**. Process stability, RPC completeness and FD conclusions were withheld after timeout.
- Unit tests cover incomplete checkpoint JSON, scan/entry deadlines, disappearing files, corrupt Go build-info lengths, counter resets, incompatible identities, HTML/script payloads, integer precision, deterministic fixture clocks and simulated non-exiting children after kill.

The non-exiting-child test uses an injected nonblocking poll result. No kernel D-state incident was induced. A genuine D-state worker can outlive SIGKILL; `cleanup_pending` remains necessary.

## Observed collection cost

These are **VM lab observations**, not validator performance promises. Three normal/oversized-response captures took **0.679–0.845 seconds** wall time. In the first capture, the supervisor reported 69.401 ms CPU and 17,473,536 bytes peak RSS; the collection worker separately reported 145.997 ms CPU and 2,191,360 bytes peak RSS. Scanner usage is reported separately. These peaks are not added into a process-tree peak.

Resource measurements finish before final JSON serialization and SSH transfer. TCG emulation, launch method, file cache, dataset size and hardware affect these observations. Legacy snapshots without collector-cost evidence leave those fields unavailable in the report.

## Reproduction

Use the build scripts in the README. Copy the resulting Linux executable and compiled Linux test executables to a disposable Linux lab; set `DOCTOR_FIBRE_KIT_BIN` to the Linux executable when running cross-compiled CLI tests. The older `TTT_FIBRE_BIN` name remains a compatibility fallback. Run the Alpine harness only with `TTT_DISPOSABLE_LAB=1` in a disposable root VM where `/usr/bin/systemctl` does not exist. It creates and removes its own mock discovery and test processes.

The optional browser verification needs Python/Playwright on a development machine and an installed Chromium browser. Pass a generated comparison HTML, `--browser` executable path and `--out` screenshot directory to `scripts/verify-report.py`. Neither dependency is required on validators.

## Remaining operational boundaries

- Production-validator validation is pending. Check server `uname -m`, checksum and permissions before an initial read-only rollout.
- Live signing freshness stays unverified unless independent activity evidence is provided. A recent scrape alone is insufficient.
- Live OTel autodiscovery has parser/ownership validation and lab coverage of explicit endpoints; production OTel deployments have not been exercised end-to-end. Unsupported or ambiguous configurations remain unavailable.
- No stress benchmark, restart/recovery, prune mutation or automatic tuning is included.
- GitHub Actions passed for source commit `7964e6d6a4f44aca2acb328859f77e8b93d68e62` on 2026-10-16: Windows and Ubuntu formatting, tests, Clippy and release builds, plus Linux musl tests and release build. See the [hosted validation run](https://github.com/ttt-labs/doctor-fibre-kit/actions/runs/38860685067). This supplements the local evidence above; hosted CI does not exercise a production validator.
