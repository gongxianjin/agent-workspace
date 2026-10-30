"""Start/stop an owned HTTP fixture and real browser tests without any model calls."""
import json
from pathlib import Path
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from server import Application
from environment import source_or_bundle

PATHS = source_or_bundle(ROOT)
from winjob import JobProcess

output = ROOT / 'dist/p1' / ('http-browser-' + time.strftime('%H%M%S'))
output.mkdir(parents=True, exist_ok=False)
app = Application(output / 'runtime', port=0, workers=False)
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
try:
    child = JobProcess([str(PATHS.node), str(ROOT / 'tests/p1/browser.cjs'), str(launch), str(output),
        str(PATHS.playwright), str(PATHS.chromium)], ROOT)
    try:
        result = child.wait(120)
    finally:
        child.close()
    print(result['stdout'])
    print(result['stderr'])
    (output / 'process.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
finally:
    app.close()
    thread.join(timeout=3)
    launch.write_text('{"stopped":true}', encoding='utf-8')
print('Evidence: ' + str(output))
sys.exit(result['exitCode'] if result['treeStopped'] else 1)
