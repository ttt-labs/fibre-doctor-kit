# Security and privacy

Fibre Doctor Kit is an on-demand diagnostic tool. Its collection code reads evidence; it does not restart services, submit transactions, tune configuration, prune data or read shard contents. Open source makes these behaviors inspectable. It does not make a binary trustworthy by itself, and this project does not promise absolute safety or anonymity.

## Data and privileges

| Surface | Behavior |
| --- | --- |
| Live Linux collection | Reads selected systemd properties, `/proc`, cgroup counters, filesystem statistics and bounded configuration files. Enumerates storage metadata and FD target metadata. |
| Process arguments/environment | Reads bounded process arguments and environment to locate configuration. Only selected values are used; complete arguments, environment and config are not serialized. Sensitive values can exist transiently in process memory. |
| Network | Performs HTTP GETs to literal loopback IP addresses only. No DNS lookup, proxy support, redirects, authentication forwarding, telemetry upload or listening web server. Explicit metrics URLs are operator-selected local services; only provide endpoints whose GET is safe. Automatic discovery requires local process/config/pipeline ownership evidence. |
| Offline commands | Read supplied files and render results. They do not contact the validator or any remote service. TUI navigation does not collect in the background. |
| Files written | `collect --out` writes a new raw snapshot. `report compare` writes a new HTML and JSON pair. Existing output files and final symlinks are refused. Unix-created output files request mode `0600`; Windows uses the destination ACL. Shell redirection has its own permissions. |
| Child processes | Executes fixed local helper programs and its own workers, without a shell or inherited helper environment. Timeout cleanup signals only its own children/process group. Inspected validator binaries are read for build metadata, never executed. |

Run with the least privileges that can read the required evidence. Missing permissions produce incomplete observations; they are not a reason to make secrets world-readable. Full access to another user's `/proc` environment, FD targets or home directory may require elevated permissions. When run as root, this is **not an OS sandbox**: the executable has root's authority even though its intended operations are read-only.

Keep the executable and its parent directories administrator-owned and not writable by the SSH collection account. Do not grant setuid, blanket sudo, Docker access or service-management permissions to make collection work. Review the [restricted SSH example](docs/operations.md) before enabling remote collection. A forced command restricts the SSH entry point; it does not sandbox a compromised binary. Fixed command paths, expiry and `restrict` must be configured by the administrator. The tool neither installs SSH keys nor changes SSH configuration.

## Raw snapshots and shareable reports

**Raw snapshots are private operational data.** They can contain service names, PIDs, cgroup paths, process timing, software versions, metrics labels and hashed host/boot/instance identities. Hashing is not encryption or anonymity: stable hashes allow correlation and low-entropy values may be guessed. Keep raw snapshots out of public repositories, support tickets and screenshots. Legacy input is not automatically sanitized in place.

Doctor output, TUI, HTML and the report JSON sidecar use a separate allowlisted projection. They exclude host/boot hashes, PIDs, cgroup paths, arbitrary service names, free-form issue details, chain IDs and metric labels. HTML embeds no raw snapshot and has no external scripts, fonts, images or data requests. Values are inserted as text; embedded JSON is escaped. Malformed snapshot errors do not echo supplied values, and application errors strip terminal control characters.

Reports still disclose permitted operational measurements: memory, storage, versions, capture times, block height and findings. **Review them before sharing.** The word “public” describes this restricted schema, not a guarantee that every organization considers these fields public. Terminal scrollback, screen recordings, backups and browser extensions are outside the tool's control.

Use a private local directory. Prefer `collect --out` to shell redirection; see the operations guide for `umask 077` and no-clobber SSH capture. A failed write may leave an incomplete file. Snapshot files are not encrypted at rest.

## Bounds and limitations

- Live collection has a 20-second supervision deadline. Storage scanners have a 2-second/50,000-entry budget per directory. Incomplete scans retain the last valid checkpoint.
- Configuration, HTTP and snapshot inputs are size-bounded. OTel YAML also has depth, node and expanded-scalar budgets. Snapshot collection counts are checked before analysis.
- FIFO inputs are rejected without waiting for a writer on Unix. This does not guarantee that a regular file on a stalled filesystem returns immediately. Explicit stdin is a stream and may wait for its producer.
- Storage scanning is flat, uses an anchored directory descriptor, refuses the final directory symlink and does not follow child symlinks. Parent paths and process namespace paths can legitimately traverse symlinks. Configuration and fixture directories must be trusted and not concurrently replaced by another user; canonical path checks are not a race-proof sandbox.
- No tool can promise zero CPU, memory or I/O impact. The budgets are not a hard process-tree memory limit. A kernel D-state child may survive a kill request; cleanup is reported as pending without a blocking wait. Do not run repeated concurrent collections as a benchmark on a validator.
- Evidence can change between reads. Ownership checks and before/after process identity reduce mistakes; they do not authenticate a hostile kernel, root account, service or forged snapshot. A report is not an attestation.

## Distribution and maintenance

Download from the [official repository's Releases](https://github.com/ttt-labs/fibre-doctor-kit/releases), verify the provided SHA256, and confirm the architecture before execution. A checksum detects mismatched downloads; a checksum served beside a binary is not an independent publisher signature. Releases do not currently provide an independent reproducible-build or signing guarantee. Builds use `Cargo.lock`; CI action references are pinned to commits and checkout credentials are not retained.

See the [security review record](docs/security-review.md) for the review scope, fixes, dependency results and remaining validation limits. This is a maintainer review with regression testing, not an independent penetration test or formal proof.

## Reporting a concern

Do not post secrets, live raw snapshots or exploit details against a running validator in public issues. Use GitHub's private vulnerability reporting option if it is enabled; otherwise ask a TTT Labs maintainer for a private reporting channel before sharing sensitive details. For a non-sensitive reproducible bug, provide the version, platform and a minimal synthetic fixture.
