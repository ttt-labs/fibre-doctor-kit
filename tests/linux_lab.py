"""Integration harness for a disposable Alpine Linux VM ONLY; never a validator.

Creates a fake systemctl in a VM without systemd, real processes/FDs, real
cgroup v2 groups where available, and loopback HTTP fixtures. Python is a lab
dependency only. Run: TTT_DISPOSABLE_LAB=1 python3 linux_lab.py /lab/fibre-doctor-kit
"""
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def serve(home, role):
    held = open(Path(home) / 'staging' / 'held', 'rb') if role == 'fibre' else None

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            if self.path == '/status':
                body = json.dumps({'result': {'node_info': {'network': 'lab-only'},
                    'sync_info': {'latest_block_height': '12345', 'catching_up': False}}}).encode()
            elif self.path == '/oversized':
                body = b'x' * (1024 * 1024 + 1)
            elif role == 'fibre':
                body = b'fibre_server_upload_shard_occupancy_bytes 9007199254740993\nfibre_server_upload_shard_budget_bytes 18446744073709551615\n'
            else:
                body = b'cometbft_privval_signing_latency_median_seconds{chain_id="lab-only"} 0.003\ncometbft_privval_signing_latency_seconds_count{chain_id="lab-only",message_type="vote"} 50\n'
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
    print(server.server_port, flush=True)
    server.serve_forever()
    if held:
        held.close()


def main(binary):
    assert os.environ.get('TTT_DISPOSABLE_LAB') == '1', 'Disposable lab opt-in required'
    assert Path('/etc/alpine-release').is_file() and os.geteuid() == 0
    assert not Path('/usr/bin/systemctl').exists(), 'Refusing to replace a real systemctl'
    binary = str(Path(binary).resolve())
    processes = []
    cgroups = []
    made_ss = False
    measurements = []
    with tempfile.TemporaryDirectory(prefix='ttt-lab-', dir='/lab') as raw:
        root = Path(raw)
        try:
            if not Path('/usr/bin/ss').exists():
                os.symlink('/sbin/ss', '/usr/bin/ss')
                made_ss = True
            if not Path('/etc/machine-id').exists():
                Path('/etc/machine-id').write_text('00000000000000000000000000000001\n')
            cg_root = Path('/sys/fs/cgroup')
            if not (cg_root / 'cgroup.controllers').exists():
                subprocess.run(['mount', '-t', 'cgroup2', 'none', str(cg_root)], check=False)
            controllers = (cg_root / 'cgroup.controllers').read_text().split()
            enabled = [x for x in ('cpu', 'memory') if x in controllers]
            if enabled:
                (cg_root / 'cgroup.subtree_control').write_text(' '.join('+' + x for x in enabled))
            services = {}
            ports = {}
            for role, unit in [('fibre', 'fibre.service'), ('app', 'celestia-appd.service')]:
                home = root / role
                for directory in ('config', 'staging', 'shards', 'data'):
                    (home / directory).mkdir(parents=True)
                for name in ('held', 'closed'):
                    path = home / 'staging' / name
                    path.write_bytes(b'lab staging')
                    os.utime(path, (time.time() - 1800, time.time() - 1800))
                with open(home / 'shards' / 'sparse', 'wb') as f:
                    f.truncate(64 * 1024 * 1024)
                os.link(home / 'shards' / 'sparse', home / 'shards' / 'hardlink')
                os.symlink('/etc', home / 'shards' / 'symlink')
                process = subprocess.Popen([sys.executable, __file__, 'serve', str(home), role],
                    stdout=subprocess.PIPE, text=True)
                processes.append(process)
                port = int(process.stdout.readline())
                ports[role] = port
                if role == 'fibre':
                    (home / 'config' / 'server_config.toml').write_text('max_connections = 16\nmax_concurrent_streams = 13\nunlimited_budget = false\n')
                else:
                    (home / 'config' / 'config.toml').write_text(f'[rpc]\nladdr = "tcp://127.0.0.1:{port}"\n[instrumentation]\nprometheus_listen_addr = "127.0.0.1:{port}"\n')
                cgroup = cg_root / f'ttt-lab-{role}'
                cgroup.mkdir()
                cgroups.append(cgroup)
                (cgroup / 'cgroup.procs').write_text(str(process.pid))
                services[unit] = f'ActiveState=active\nSubState=running\nMainPID={process.pid}\nNRestarts=0\nExecMainStartTimestampMonotonic=1000000\nControlGroup=/ttt-lab-{role}\n'
            definitions = root / 'services.json'
            definitions.write_text(json.dumps(services))
            Path('/usr/bin/systemctl').write_text('#!/usr/bin/python3\nimport json,sys\nprint(json.load(open(' + repr(str(definitions)) + ')).get(sys.argv[-1], ""))\n')
            Path('/usr/bin/systemctl').chmod(0o755)
            args = [binary, 'collect', '--stdout', '--profile', 'v10.4.0-mocha',
                '--app-home', str(root / 'app'), '--fibre-home', str(root / 'fibre')]

            def collect(extra=()):
                started = time.monotonic()
                result = subprocess.run(args + list(extra), capture_output=True, timeout=24)
                assert result.returncode == 0, result.stderr.decode(errors='replace')
                elapsed = time.monotonic() - started
                snapshot = json.loads(result.stdout)
                Path('/lab/latest-collection.json').write_bytes(result.stdout)
                measurements.append({'elapsed_seconds': elapsed, 'parent': snapshot['self_usage'],
                    'workers': snapshot['worker_usage']})
                assert len(result.stdout) < 8 * 1024 * 1024
                return snapshot, elapsed

            first, elapsed = collect(['--fibre-metrics-url', f'http://127.0.0.1:{ports["fibre"]}/metrics'])
            assert elapsed < 21
            assert first['host']['architecture'] == 'x86_64'
            assert first['app_status']['block_height'] == 12345
            assert first['fibre_config']['root.max_connections']['value'] == 16
            assert first['fibre_config']['root.max_connections']['source'] == 'CONFIG_FILE'
            fibre = first['services']['fibre.service']
            assert fibre['identity']['stable_during_capture'] is True
            assert fibre['fd_count'] > 0 and fibre['memory_current_bytes'] is not None
            assert fibre['ancestor_limits_complete'] is True
            scans = {s['directory']: s for s in first['scans']}
            assert scans['shards']['observed_apparent_bytes'] == 64 * 1024 * 1024
            assert scans['shards']['observed_allocated_bytes'] < 64 * 1024 * 1024
            assert scans['shards']['files_counted'] == 1 and scans['shards']['skipped_symlinks'] == 1
            assert scans['staging']['aged_files_count'] == 2
            assert sorted(f['has_open_fd'] for f in scans['staging']['aged_files']) == [False, True]
            occupancy = next(m for m in first['metrics']['samples'] if m['name'].endswith('occupancy_bytes'))
            assert occupancy['exact_integer'] == 9007199254740993
            assert first['metrics']['freshness_verified'] is False
            denied = subprocess.run([binary, '__scan-worker', str(root / 'fibre' / 'staging'), 'staging', '2000', '50000'],
                capture_output=True, timeout=3, check=True, preexec_fn=lambda: os.setuid(65534))
            denied_scan = json.loads(denied.stdout.splitlines()[-1])
            assert denied_scan['status'] == 'error' and denied_scan['observed_apparent_bytes'] is None
            (root / 'first.json').write_text(json.dumps(first))
            second, _ = collect(['--no-fd-inspection'])
            assert all(f['has_open_fd'] is None for s in second['scans'] for f in s['aged_files'])
            (root / 'second.json').write_text(json.dumps(second))
            subprocess.run([binary, 'report', 'compare', str(root / 'first.json'), str(root / 'second.json'),
                '--mode', 'temporal', '--out', str(root / 'report.html')], check=True, timeout=5)
            public = json.loads((root / 'report.json').read_text())
            # Public projection must not expose lab home paths or raw process identity.
            assert str(root) not in json.dumps(public)
            too_large, _ = collect(['--fibre-metrics-url', f'http://127.0.0.1:{ports["fibre"]}/oversized'])
            assert any(i['code'] == 'METRICS_READ_FAILED' for i in too_large['issues'])
            for public_name in ('first.json', 'second.json', 'report.html', 'report.json'):
                (Path('/lab') / ('integration-' + public_name)).write_bytes((root / public_name).read_bytes())
            # A named pipe is not a supported metrics file. Reject it without
            # waiting for a writer or consuming the outer collection deadline.
            fifo = root / 'blocked-metrics'
            os.mkfifo(fifo)
            rejected, duration = collect(['--fibre-metrics-file', str(fifo)])
            assert duration < 5, duration
            assert any(i['code'] == 'METRICS_FILE_UNAVAILABLE' for i in rejected['issues'])
            assert not any(i['code'] == 'COLLECTION_TIMEOUT' for i in rejected['issues'])
            assert rejected['metrics']['samples'] == []
            assert rejected['scans'] and all(u['complete'] for u in rejected['worker_usage'])
            assert all(s['identity']['stable_during_capture'] is True for s in rejected['services'].values())
            result = {'passed': True, 'environment': 'Disposable Alpine/QEMU TCG, mock systemd, real proc/cgroup/FD/filesystem/HTTP',
                'measurements': measurements, 'checks': ['live process and cgroup', 'profile/config provenance',
                    'sparse/hardlink/symlink scan', 'aged FD true/false/unknown', 'large Prometheus integer',
                    'unprivileged permission error', 'HTTP response cap', 'temporal HTML/JSON', 'nonblocking FIFO metrics rejection']}
            Path('/lab/integration-results.json').write_text(json.dumps(result, indent=2))
            print(json.dumps(result, indent=2))
        finally:
            for process in processes:
                process.terminate()
                process.wait(timeout=3)
            Path('/usr/bin/systemctl').unlink(missing_ok=True)
            if made_ss:
                Path('/usr/bin/ss').unlink(missing_ok=True)
            for group in cgroups:
                group.rmdir()


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == 'serve':
        serve(sys.argv[2], sys.argv[3])
    else:
        main(sys.argv[1])
