"""Free Linux browser fixture: real HTTP console + real Chromium, zero model calls.

Runs inside a relay-process-test systemd unit so the owned host context exists;
explicit local Node/Playwright/Chromium paths replace the Windows-only defaults.
Never fabricates model completion.
"""
import json
from pathlib import Path
import subprocess
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from server import Application
from process_control import JobProcess

# 显式本地依赖（Windows development_environment 默认值在 Linux 无效）
NODE = Path('/root/.nvm/versions/node/v25.8.0/bin/node')
PLAYWRIGHT = Path('/root/gwx/sandbox/aios-sandbox-ui/node_modules/playwright/package.json')
CHROMIUM = Path('/root/.cache/ms-playwright/chromium-1217/chrome-linux64/chrome')


def run_inside_unit():
    output = ROOT / 'dist/p1' / ('linux-http-browser-' + time.strftime('%H%M%S'))
    output.mkdir(parents=True, exist_ok=False)
    app = Application(output / 'runtime', port=0, workers=True, preview_port=0)
    sys.path.insert(0, str(ROOT / 'tests/p1'))
    from support import ReadySetupFixture

    class SharedSetupFixture(ReadySetupFixture):
        def __init__(self):
            self.ready = True

        def public(self):
            return {**super().public(), 'canExecute': self.ready}

        def recheck(self):
            # Shared cached gate only: never launches a client, detector, or model.
            self.ready = not self.ready
            return self.public()

    app.setup = SharedSetupFixture()
    # Test-only failed CLI boundary: no model completion or fixture path in production API.
    retry_fixture = app.store.create_project('仅测试：可安全显式重试')
    retry_fixture = app.store.command(retry_fixture['id'], 'fixture-generate', retry_fixture['revision'], 'generate', {})
    retry_attempt = app.store.claim_attempt(retry_fixture['id'])
    app.store.fail_attempt(retry_attempt['id'], 'fixture CLI failure; no model was called', confirmed=True)
    thread = threading.Thread(target=app.serve, daemon=True)
    thread.start()
    launch = output / 'launch-private.json'
    launch.write_text(json.dumps({'url': app.origin + '/#bootstrap=' + app.bootstrap}), encoding='utf-8')
    exit_code = 1
    try:
        child = JobProcess([str(NODE), str(ROOT / 'tests/p1/browser.cjs'), str(launch), str(output),
            str(PLAYWRIGHT), str(CHROMIUM)], ROOT)
        try:
            result = child.wait(180)
        finally:
            child.close()
        print(result['stdout'])
        print(result['stderr'])
        (output / 'process.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
        exit_code = result['exitCode'] if result['treeStopped'] else 1
    finally:
        app.close()
        thread.join(timeout=3)
    print('Evidence: ' + str(output))
    return exit_code


def main():
    unit = 'relay-process-test-' + uuid.uuid4().hex + '.service'
    manager = '--system' if os_getuid() == 0 else '--user'
    started = subprocess.run(['/usr/bin/systemd-run', manager, '--quiet', '--collect',
        '--unit', unit, '--property=Type=exec', '--property=KillMode=control-group',
        '--property=TimeoutStopSec=30s', '--property=UMask=0077',
        sys.executable, '-I', '-B', '-X', 'utf8', str(Path(__file__).resolve()), '--inside'],
        capture_output=True, text=True, timeout=15)
    print('systemd-run:', started.returncode, started.stderr.strip() or 'ok')
    try:
        deadline = time.monotonic() + 300
        while time.monotonic() < deadline:
            result = subprocess.run(['/usr/bin/systemctl', manager, 'show', unit,
                '--property=ActiveState', '--property=ExecMainStatus'], capture_output=True, text=True, timeout=5)
            props = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
            if props.get('ActiveState') in ('failed', 'inactive'):
                print('unit finished:', props)
                return int(props.get('ExecMainStatus') or 1)
            time.sleep(1)
        print('TIMEOUT waiting unit')
        return 1
    finally:
        subprocess.run(['/usr/bin/systemctl', manager, 'stop', unit], capture_output=True, timeout=30)
        subprocess.run(['/usr/bin/systemctl', manager, 'reset-failed', unit], capture_output=True, timeout=10)


def os_getuid():
    import os
    return os.getuid()


if __name__ == '__main__':
    sys.exit(run_inside_unit() if sys.argv[1:2] == ['--inside'] else main())
