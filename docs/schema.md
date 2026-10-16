# Snapshot and public report conventions

The authoritative Rust types are in `src/model.rs`. Snapshots have `schema_version: 1`; legacy probe input is normalized without inventing missing identity or application version. Unsupported schema versions fail parsing. Input and serialized snapshot output are limited to 8 MiB.

| Field group | Meaning |
| --- | --- |
| `capture` | Start/end wall time, monotonic observation, hashed host/boot identity, input provenance |
| `services` | Role, process identity, cgroup/FD/memory measurements, version evidence |
| `fibre_config` | Allowlisted values and `OBSERVED_RUNTIME`, `CONFIG_FILE`, `DEFAULT_ASSUMED` or `UNKNOWN` provenance |
| `scans` | Observed bytes/counts, completion/stop reason, aged FD tri-state and scanner cost |
| `metrics` | Allowlisted samples, endpoint identity/freshness qualification, collection status |
| `app_status` | Scoped synchronization evidence, independent of process active state |
| `issues` | Bounded collection issues; these are not automatically node failures |
| `self_usage`, `worker_usage` | Separate parent and collection-worker CPU/RSS accounting |

Memory is bytes. Native counters remain unsigned integers; finite limits use `{ "kind": "finite", "bytes": N }`, while unlimited is `{ "kind": "unlimited" }`. Missing values are `null`, not zero or unlimited. Collection status is separate from a doctor's `PASS/WARN/FAIL/INCONCLUSIVE/UNSUPPORTED` verdict.

Prometheus samples retain an `exact_integer` when the value token is an unsigned decimal integer; `value` is the floating-point interpretation for other measurements. An exported floating-point sample cannot recover precision already lost by its producer. Analyzer integer conversions reject unsafe or overflowing values.

`has_open_fd` is `true`, `false` or `null`. A complete descriptor inspection plus stable capture identity is required for `false`; incomplete/truncated/failed inspection is unknown. File age and descriptor state do not prove orphan status.

Public reports use `report_schema_version: 1` and a curated projection, not a raw embedded snapshot. Operational paths, PIDs, host/boot hashes, raw errors, arbitrary config, metric labels and free-form text are omitted. Integer measurements are decimal **strings** in public JSON so offline browser code can preserve values larger than JavaScript's safe integer range. Consumers should use integer/decimal parsing rather than floats for counters. Reports use English; metric names, flags and check IDs are stable identifiers.

Comparison defaults to independent A/B observations. Temporal rates require eligible identity continuity for each process/cgroup/host metric, valid time, and monotonic counters. Partial observations remain visible even when rates are unavailable. There is no aggregate readiness score.
