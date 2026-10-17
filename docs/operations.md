# Read-only operation and restricted SSH

## Before first collection

1. Validate the release checksum locally and run the Linux lab tests documented in `validation.md`.
2. On the intended server, inspect `uname -m`; select the matching Linux artifact. Do not infer architecture from CPU count.
3. Install the binary using an existing administrative channel. Keep its directory and any wrapper/configuration owned by the administrator and not writable by the collection account.
4. Run one on-demand collection first. Review measured duration, parent/worker resource usage, errors, partial scans and `cleanup_pending` before scheduling anything.

The collector does not install services or modify node configuration. Do not add restarts, `sudo` grants, load tests or prune operations to its SSH wrapper.

## Fixed SSH command

Example **template**, not a command to paste unchanged. Replace the public key and expiry with operator-approved values; retain existing access and a private backup. Never paste a private key into this tool.

```text
restrict,expiry-time="YYYYMMDDHHMMSSZ",command="/usr/local/bin/fibre-doctor-kit collect --stdout" ssh-ed25519 PUBLIC_KEY collector-only
```

Use a dedicated account with only the read permissions required by the deployment where feasible. Missing permissions produce missing/unknown evidence rather than a reason to grant arbitrary command access. If existing service homes require root, the fixed collector still needs full input validation and must not accept `SSH_ORIGINAL_COMMAND` as executable instructions.

`restrict` disables port/agent/X11 forwarding, PTY allocation and user rc execution. The fixed command ignores client-supplied shell commands. Check sshd configuration and binary ownership for the actual deployment. Expiry disables authentication; it does not remove the public-key line or backup.

Client example for a POSIX shell, using a new output filename:

```sh
(
  umask 077
  set -C
  ssh -n -T -o BatchMode=yes -o StrictHostKeyChecking=yes \
    -o IdentitiesOnly=yes -o ForwardAgent=no -o ClearAllForwardings=yes \
    -i /path/to/dedicated_key operator@host snapshot > private-snapshot.json &&
    fibre-doctor-kit doctor --snapshot private-snapshot.json
)
```

`umask 077` removes group/other permissions when the client creates the file; `set -C` refuses to overwrite an existing regular file. Use an administrator-controlled directory without untrusted symlinks or special files. The subshell leaves the caller's shell settings unchanged. SSH redirection creates the file on the client: it does not inherit the collector's server-side output permissions. A failed transfer may leave a partial private file; inspect it and choose a new filename before retrying. On Windows, use a private directory whose ACL permits only the intended account; POSIX mode bits do not apply.

Verify the server host fingerprint through an existing trusted channel. Raw snapshots contain operational identifiers and configuration evidence; keep them private. Review the public HTML and JSON emitted by `report compare` before sharing them; public reports still disclose capacity and diagnostic results.

A forced-command key can run only its configured command. Install and update the binary through a separate administrative channel; keep collection access restricted.

## Interpretation

- A scan timeout keeps its last valid checkpoint. A process waiting in uninterruptible kernel I/O may survive the kill request; `cleanup_pending` exposes this. Stop repeated collection attempts until the underlying I/O issue is understood.
- `has_open_fd=false` means no matching FD was observed during a complete scoped inspection. It does not prove a file is abandoned or safe to delete.
- Different filesystem identifiers do not prove different physical devices. Root filesystem free space is not a measurement of another data mount.
- `NRestarts=0`, current active state and zero retained OOM counters do not establish complete historical uptime.
- Self resource measurements end before JSON serialization/SSH transfer. They do not measure unrelated services or the whole node.

Sources: [OpenSSH](https://man.openbsd.org/sshd.8#AUTHORIZED_KEYS_FILE_FORMAT), [Rust process handling](https://doc.rust-lang.org/std/process/struct.Child.html), [Linux getrusage](https://man7.org/linux/man-pages/man2/getrusage.2.html).
