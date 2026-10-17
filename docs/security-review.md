# Security review — v0.2.1

Review date: 2026-10-17. Scope: application source, Linux collection and worker supervision, offline parsing and analysis, terminal/HTML output, packaging scripts, CI references and locked dependencies. The baseline was v0.2.0 (`33bd0a3b7a7a7c4c502fb4125b5c3cafbbfa1bd9`); this document accompanies its fixes and the consistent `fibre-doctor-kit` naming.

This is a source review and regression test record, not an independent audit certification. Tests use the PC, synthetic inputs and a disposable Linux lab. No validator connection or production service modification is part of this review.

## Findings addressed

| Finding | Preconditions and impact | Change |
| --- | --- | --- |
| OTel YAML alias amplification | A local OTel configuration selected by discovery can be small on disk but expand to a much larger in-memory tree. The old byte/time limits did not cap this allocation. | Replaced the deprecated YAML parser with `serde-saphyr`, with includes/interpolation features disabled. Reject anchors, aliases, merges, unsupported tags, duplicates and multiple documents; cap input at 64 KiB, nesting at 32, nodes at 4,096, events at 8,192 and scalar data at 128 KiB. Configuration errors never expose snippets. |
| Terminal escape in malformed snapshot errors | Opening a crafted snapshot could echo a malicious enum value, including an OSC terminal sequence, through a Serde error. | Parser errors no longer quote input values. Application stderr strips controls and bidi formatting characters and limits message length. Regression supplies an OSC52 sequence and private marker, captured without executing it in a terminal. |
| Offline snapshot amplification | Many small service records could expand into disproportionately large findings/output despite fitting within the 8 MiB byte cap. | Reject excessive services, scans, metrics, issue counts and tree structure before typed conversion/analysis; both v1 and legacy adapters are covered. This does not claim a hard allocator limit. |
| FIFO blocking before file-type checks | A local snapshot/fixture/metrics path could be a named pipe with no writer. Opening it blocked before the regular-file check; offline paths had no collection supervisor. | Unix file inputs use nonblocking open and inspect the opened descriptor. Explicit stdin remains a streaming interface. Tests exercise FIFOs and bounded failure. |
| False socket ownership | An untrusted local process name containing `pid=<target>,` could satisfy a substring search in `ss` output. This could misattribute local metrics/RPC evidence. | Parse owner records outside quoted process names, reject ambiguous/mixed owners and malformed records, and check address family. Unsupported `*:port` output fails closed. This is evidence integrity, not demonstrated remote code execution. |
| SSH redirection permissions | The old local shell redirection example could create raw snapshots with a permissive umask. | Document private directories, `umask 077`, no-clobber creation and analysis only after successful transfer. Windows ACLs remain operator-managed. |
| Mutable CI action references | A movable third-party action tag could change future workflow execution. | Pin action commits, disable persisted checkout credentials and retain read-only repository permissions. Add a RustSec dependency audit job. |

## Dependency review

The initial OSV scan found `lru` 0.12.5 through Ratatui, plus maintenance advisories for `paste` and `unsafe-libyaml`. The LRU advisories concern iterator aliasing and panic safety; this review did not demonstrate exploitation through the tool's concrete cache key types. See [RUSTSEC-2026-0002](https://rustsec.org/advisories/RUSTSEC-2026-0002.html) and [RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253.html).

Ratatui was upgraded to 0.30.2, selecting `lru` 0.18.5; the old `paste` dependency was removed. Replacing `serde_yaml` removed `unsafe-libyaml`. The maintenance advisories are [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) and [RUSTSEC-2024-0449](https://rustsec.org/advisories/RUSTSEC-2024-0449.html). The YAML replacement raises the minimum Rust version to 1.89.

The subsequent OSV query of all **134 registry packages** in the updated lockfile returned **no matching advisories** on the review date. OSV sees only public package names/versions, not snapshots. A local Windows `cargo-audit` 0.22.2 build failed in its own AWS-LC/MinGW linker dependency (`nanosleep64`); this is not counted as a passed audit. The Linux CI job runs `cargo audit --deny warnings` against the current RustSec database. Advisory databases describe known reports and cannot establish the absence of unknown defects.

## Controls checked

- Live network requests are numeric loopback GETs only. Redirects, non-loopback addresses, hostname/proxy resolution, URL credentials and query strings are rejected. There is no runtime telemetry uploader or web listener.
- Collection uses fixed helper executable paths and argument arrays, clears child environments and never executes the inspected validator binary. No node restart, config write, transaction submission, pruning or file-content scan was found in runtime paths.
- Storage uses an anchored directory descriptor, `fstatat` without following child symlinks, device checks and hardlink deduplication. Checkpoint sizes and reads are bounded; worker reaping uses nonblocking polls after kill.
- The public projection removes process/host identity, paths, labels and free-form source text. HTML escapes embedded JSON, renders through text APIs and blocks network connections with its CSP. TUI text is also control-character filtered.
- Output creation is exclusive and uses mode `0600` on Unix. Existing and dangling output symlinks are tested without modifying their targets.
- Existing tests cover sparse files, disappearing entries, permission errors, incomplete checkpoints, counter resets, identity mismatches, missing evidence, large integers, script payloads and terminal restoration.

## Residual risks and deployment boundary

Validation passed with 81 library/8 CLI tests on Windows and 105 library/10 CLI tests in the disposable Linux lab. The Linux integration harness, real PTY/ConPTY interaction checks and offline browser checks passed. A traced lab integration run observed only local fixture connections and requested report writes; see the [validation record](validation.md#v021-security-review-and-consistent-naming) for scope and exact limits. A bounded pattern scan of tracked files found no private-key blocks, GitHub tokens, AWS access-key IDs, configured production address or authorized-key entries; this is not an exhaustive secret detector or a historical repository audit.

Read-only collection is an application behavior, **not OS-enforced confinement**. Root execution still grants root authority; use the least required privileges and protect executable/configuration parent directories from replacement. Fixture canonicalization is not a descriptor-based sandbox against concurrent hostile changes. Explicit metrics URLs trust the operator to select a harmless local GET endpoint.

Raw snapshots are private and unencrypted. Hashes are correlatable. Public reports still reveal permitted measurements, versions and timing; review them before distribution. Configuration/arguments/environment can temporarily exist in memory, so crash dumps or privileged memory inspection are separate privacy concerns.

Bounded sampling consumes resources and does not prove zero impact. Deadlines cannot interrupt every kernel wait; a D-state worker may remain after kill, and pending cleanup must be respected. Offline regular-file reads on broken/network filesystems and slow stdout consumers can still wait. Snapshot content and local service responses are not cryptographically authenticated.

Release checksums verify matching bytes, not independent publisher identity. Reproducible builds, signed provenance, a hostile-root threat model, formal verification and an independent external audit are not established here. Production-validator validation remains pending; use [Security and privacy](../SECURITY.md) and [Operations](operations.md) when planning a rollout.
