# CLI and interpretation reference

[Back to the quick start](../README.md)

Examples on this page use `fibre-doctor-kit` for an executable on your PATH. Otherwise use `./fibre-doctor-kit-linux-x86_64-musl` on Linux or `.\fibre-doctor-kit-windows-x86_64.exe` in PowerShell.

## Supported collection environment

Live collection targets systemd services and cgroup v2. Default services are `celestia-appd.service` and `fibre.service`. The initial Fibre capacity/storage profile is `v10.4.0-mocha`; unknown versions are not silently treated as supported. Unreadable sections remain missing or inconclusive.

The Linux artifact is a static binary. Live reads use systemd and `ss` where available; missing commands or permissions reduce collection coverage. The kit does not restart services, inject traffic, upload blobs, edit configuration or delete storage. Load benchmarks, recovery tests, automatic tuning and S3/R2 integration are outside v0.1.

## CLI reference

| Command | Purpose |
| --- | --- |
| `collect --out snapshot.json` | Write one private operational snapshot; Linux live mode by default |
| `collect --stdout` | Emit the same snapshot to stdout; choose exactly one output destination |
| `tui` | Collect once on Linux and open the interactive terminal viewer; saves no files |
| `tui --snapshot snapshot.json` | View an existing snapshot offline on Linux or Windows |
| `tui --snapshot A.json --compare B.json --mode temporal` | View the newer capture B and its changes from A on the same host |
| `doctor --snapshot snapshot.json` | Show findings, evidence, impact and verification steps |
| `doctor --snapshot - --json` | Read one snapshot from stdin and return public findings as JSON |
| `capacity --snapshot snapshot.json --target-connections 24` | Calculate a configuration scenario without applying it |
| `report compare A.json B.json --out report.html` | Write offline HTML and a public `report.json` sidecar |

The CLI and HTML report use English. JSON keys, metric names, flags and check IDs are stable identifiers. `doctor` and `capacity` support `--json`.

TUI is available starting in v0.2.0 and uses the same analyzer and public data projections as reports. It does not poll or refresh in the background. Use Tab/Shift+Tab or 1–4 for views, arrows/j/k for navigation, PgUp/PgDn for details, Home/End for the start/end of details or rows, and Q/Esc/Ctrl+C to exit. A small terminal asks you to enlarge it. Existing snapshots are read once; reopen the viewer to load updated data. With `--snapshot`, live collection flags are rejected rather than silently ignored. Snapshot input must be a file; stdin remains available through `doctor --snapshot -`.

When using `tui` over SSH, allocate a terminal (for example `ssh -t` through your normal interactive access). A collection-only forced-command key cannot start TUI. For scripts or output redirection use `doctor`, `capacity` and `report compare`. TUI restores the terminal on normal exit and handled errors/signals; a forced kill such as SIGKILL cannot run cleanup.

`--profile v10.4.0-mocha` is an explicit model assumption, not an observed binary version. Use it only when that assumption is justified. Results go to stdout and diagnostic messages to stderr. Exit code `0` means the command completed, **not** that every check passed; command/input errors return `2`. Automation should inspect finding statuses and collection issues.

## Collection and metrics

Override service names and homes when they differ from the defaults:

```sh
fibre-doctor-kit collect --out snapshot-custom.json --app-unit celestia-appd.service --fibre-unit fibre.service --app-home /srv/celestia --fibre-home /srv/fibre
fibre-doctor-kit collect --out snapshot-file.json --fibre-metrics-file metrics.prom
fibre-doctor-kit collect --out snapshot-http.json --fibre-metrics-url http://127.0.0.1:8889/metrics
fibre-doctor-kit collect --out snapshot-otel.json --otel-config /etc/otelcol/config.yaml
```

Paths and port `8889` above are examples, not discovered defaults. Supply your existing configuration. Explicit metrics URLs must use HTTP with a numeric loopback address and port; credentials, redirects and remote addresses are rejected. `--fibre-prom-url` is an alias for `--fibre-metrics-url`.

Metrics-file input takes precedence over URL/discovery and does not verify target identity or freshness. OTel discovery requires an identifiable local Fibre OTLP receiver, collector process/configuration and matching enabled Prometheus exporter pipeline. `--otel-config` helps validate that relationship; it does not turn any YAML file into a trusted endpoint. Ports are not guessed, and OTLP ingestion endpoints are not scraped as Prometheus endpoints.

`--no-fd-inspection` disables the additional inode-based matching between aged staging files and open descriptors; FD counts are still collected. `--sysroot PATH` instead selects offline fixtures and disables live commands/network. See [schema conventions](schema.md) for field meanings.

## Capacity and comparison

For the supported profile:

```text
receive_buffer_bytes = max_connections × max_concurrent_streams × 132 MiB
```

With 13 concurrent streams, the model yields:

| Connections | Receive-buffer model |
| --- | --- |
| 16 | 26.8125 GiB |
| 24 | 40.21875 GiB |
| 32 | 53.625 GiB |

This is a concurrency model, not measured RAM, total process memory or a safe hardware recommendation. Increasing connections increases modeled buffers. RSS, anonymous memory, file cache and cgroup memory overlap; do not add them together. Account for other services, parent cgroup limits and FD headroom. `CONFIG_FILE` values are not proof that the running process loaded them; the other sources are `OBSERVED_RUNTIME`, `DEFAULT_ASSUMED` and `UNKNOWN`.

The default `--mode ab` compares independent environments side by side and never produces temporal counter rates. Use temporal mode for successive captures from the same host, with the older capture first. Inputs must be collection snapshots, not generated public reports or `doctor --json` output. Both output paths must be unused, and their parent directory must already exist.

Add `--allow-legacy-identity` only for older snapshots when you know both came from the same host. It records an assumption; it does not verify identity or bypass detected restarts/counter resets:

```sh
fibre-doctor-kit report compare before.json after.json --mode temporal --out temporal.html
fibre-doctor-kit report compare legacy-a.json legacy-b.json --mode temporal --allow-legacy-identity --out legacy.html
```

`Verified` requires complete process/host/boot identity evidence. `HeuristicLegacy` is an explicit same-host assumption for older snapshots, not verified continuity. `Mismatch` and `Unknown` withhold affected temporal calculations. Rates also require valid timing and appropriate process/cgroup/host scope; counter resets make rates unavailable. A short memory delta does not establish a leak or historical uptime.

## Reading findings and collection limits

| Status | Meaning |
| --- | --- |
| `PASS` | The individual check is satisfied by the available evidence |
| `WARN` | An observation needs investigation |
| `FAIL` | Evidence violates that check's condition |
| `INCONCLUSIVE` | Evidence is missing, incomplete, stale or insufficient |
| `UNSUPPORTED` | The version or observation cannot be evaluated by the supported model |

There is no overall readiness score. A collection error is not automatically a node failure. Signing requires the appropriate version, scoped samples and verified activity freshness; warm-up or a recent HTTP scrape alone does not establish healthy signing. Live v0.1 collection conservatively leaves freshness unverified.

| Collection bound | Limit |
| --- | --- |
| Overall collection work budget | 20 seconds, plus bounded cleanup/serialization overhead |
| Flat scan per `staging/` or `shards/` | 2 seconds / 50,000 entries |
| Kill/reap polling grace | 100 ms, nonblocking |
| Config read / metrics response / snapshot | 64 KiB / 1 MiB / 8 MiB |
| Aged staging threshold | More than 900 seconds |

Scans reject the final directory symlink and do not follow child symlinks; parent path components may resolve through symlinks. Hardlinks are deduplicated. A partial result is **bytes observed**, not an instantaneous total or reliable lower bound during concurrent writes/pruning. `has_open_fd` is true, false or unknown; false requires complete scoped inspection and stable identity. Neither age nor a closed FD proves that a file is an orphan or safe to delete. A kernel-blocked worker can outlive a kill request; `cleanup_pending` exposes this.

Collector costs separate parent, collection worker and scanner CPU/RSS. Do not sum peak RSS values into a process-tree peak. Measurements end before final JSON serialization and SSH transfer. Reads still consume resources; review a first capture before repeated use.

## Privacy and SSH operation

Keep raw snapshots private. Public reports use an allowlisted projection that omits host/boot hashes, operational paths, PIDs, raw errors and arbitrary metric labels. This reduces disclosure but is not a blanket anonymity guarantee; review reports before publishing. Large integer measurements are decimal strings in public JSON to preserve browser precision.

Install through an administrative channel, then use a dedicated restricted collection key. An `authorized_keys` **template** is:

```text
restrict,expiry-time="YYYYMMDDHHMMSSZ",command="/usr/local/bin/fibre-doctor-kit collect --stdout" ssh-ed25519 PUBLIC_KEY collector-only
```

Replace the date and public key, verify the host fingerprint, and protect binary ownership. The client command below assumes that the forced command is already configured; the word `snapshot` is not a remote subcommand installed by the kit.

```sh
ssh -n -T -o BatchMode=yes -o StrictHostKeyChecking=yes -o IdentitiesOnly=yes -o ForwardAgent=no -o ClearAllForwardings=yes -i /path/to/dedicated_key operator@host snapshot | fibre-doctor-kit doctor --snapshot -
```

No private key is required in the repository. See [operations](operations.md) for account permissions and fixed-command deployment.

## Troubleshooting

| Symptom | Action |
| --- | --- |
| Live collection fails on Windows | Analyze a Linux snapshot or use explicit `--sysroot` fixtures |
| Output already exists | Choose a new filename; reports also reserve the `.json` sidecar |
| Unknown version / unavailable capacity | Verify the binary version and config; only explicitly assume the supported profile when justified |
| Empty metrics / inconclusive signing | Check the configured exporter, ownership, sample scope and freshness; do not guess ports |
| Partial scan / `cleanup_pending` | Inspect the stop reason and host I/O condition before retrying; do not delete data |
| Invalid stdin / expired probe output | Provide one supported JSON snapshot; an SSH or probe error is not a snapshot |
| Comparison input is rejected | Use raw collection snapshots, not `report.json` or `doctor --json` output |
| HTML was created but no browser opened | Download the HTML to your PC and open it; see [viewing the report](../README.md#open-the-html-report) |
