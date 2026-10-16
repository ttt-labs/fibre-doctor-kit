# v0.1 validation record

Verified on 2026-10-16 using Windows and a disposable Linux VM. These results cover local builds, tests and lab integration; they do not establish production-validator compatibility or performance.

## Build and tests

| Check | Result |
| --- | --- |
| Windows x86_64 GNU, Rust 1.99.0 | 58 library + 5 CLI tests passed |
| Linux x86_64 musl, Alpine 3.24.2 / kernel 6.18.52 | 78 library + 5 CLI tests passed |
| `cargo fmt --all -- --check` | Passed |
| Clippy, Windows and Linux targets, all targets, warnings denied | Passed |
| Linux ELF | ELF64, x86-64, static PIE; no PT_INTERP and no DT_NEEDED |
| Windows PE imports | Windows system DLLs only; no separate GNU runtime DLL needed |
| Offline HTML, Edge/Chromium via Playwright | Four language switches, finding expansion, 390px layout passed; zero page errors or HTTP(S) requests |

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

Use the build scripts in the README. Copy the resulting Linux executable and compiled Linux test executables to a disposable Linux lab; set `TTT_FIBRE_BIN` to the Linux executable when running cross-compiled CLI tests. Run the Alpine harness only with `TTT_DISPOSABLE_LAB=1` in a disposable root VM where `/usr/bin/systemctl` does not exist. It creates and removes its own mock discovery and test processes.

The optional browser verification needs Python/Playwright on a development machine and an installed Chromium browser. Pass a generated comparison HTML, `--browser` executable path and `--out` screenshot directory to `scripts/verify-report.py`. Neither dependency is required on validators.

## Remaining operational boundaries

- Production-validator validation is pending. Check server `uname -m`, checksum and permissions before an initial read-only rollout.
- Live signing freshness stays unverified unless independent activity evidence is provided. A recent scrape alone is insufficient.
- Live OTel autodiscovery has parser/ownership validation and lab coverage of explicit endpoints; production OTel deployments have not been exercised end-to-end. Unsupported or ambiguous configurations remain unavailable.
- No stress benchmark, restart/recovery, prune mutation or automatic tuning is included.
- The CI workflow is supplied but has not been published or run on GitHub. Local commands above are the validation evidence.
