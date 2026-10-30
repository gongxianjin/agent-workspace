"""Drive bounded-read/storage/auth behavior against an isolated real service, no models."""
import json
from pathlib import Path
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from server import Application
from environment import source_or_bundle
from winjob import JobProcess

output = ROOT / 'dist/p1' / ('transport-browser-' + time.strftime('%H%M%S'))
output.mkdir(parents=True, exist_ok=False)
app = Application(output / 'runtime', port=0, workers=False)
sys.path.insert(0, str(ROOT / 'tests/p1'))
from support import ReadySetupFixture
app.setup = ReadySetupFixture()
app.store.create_project('有界请求测试')
thread = threading.Thread(target=app.serve, daemon=True)
thread.start()
launch = output / 'launch-private.json'
launch.write_text(json.dumps({'urls': [app.origin + '/#bootstrap=' + app.issue_bootstrap() for _ in range(3)]}), encoding='utf-8')
paths = source_or_bundle(ROOT)
try:
    child = JobProcess([str(paths.node), str(ROOT / 'tests/p1/transport-browser.cjs'), str(launch), str(output),
        str(paths.playwright), str(paths.chromium)], ROOT)
    try:
        result = child.wait(70)
    finally:
        child.close()
    (output / 'process.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(result['stdout'])
    print(result['stderr'])
finally:
    app.close()
    thread.join(timeout=3)
    launch.write_text('{"stopped":true}', encoding='utf-8')
print('Evidence: ' + str(output))
raise SystemExit(result['exitCode'] if result['treeStopped'] else 1)
