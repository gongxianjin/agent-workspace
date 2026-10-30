"""Run isolated actual UI with no model, downloads or existing service changes."""
import json
from pathlib import Path
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
HELPERS = ROOT / 'dist/p1'
sys.path.insert(0, str(ROOT / 'p1'))
from server import Application
from environment import source_or_bundle
from winjob import JobProcess

output = HELPERS / ('agent-controls-' + time.strftime('%H%M%S'))
output.mkdir(exist_ok=False)
app = Application(output / 'runtime', port=0, workers=False)
launch = output / 'launch-private.json'
thread = threading.Thread(target=app.serve, daemon=True)
try:
    app.store.create_project('三角色交互测试夹具（不调用模型）')
    thread.start()
    launch.write_text(json.dumps({'urls': [app.origin + '/#bootstrap=' + app.issue_bootstrap()]}), encoding='utf-8')
    paths = source_or_bundle(ROOT)
    child = JobProcess([str(paths.node), str(ROOT / 'tests/p1/agent-controls-browser.cjs'), str(launch), str(output), str(paths.playwright), str(paths.chromium), str(ROOT)], ROOT)
    try:
        result = child.wait(70)
    finally:
        child.close()
    (output / 'process.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(result['stdout']); print(result['stderr'])
finally:
    app.close(); thread.join(timeout=3)
    if launch.exists():
        launch.write_text('{"stopped":true}', encoding='utf-8')
print('Evidence: ' + str(output))
raise SystemExit(result['exitCode'] if result['treeStopped'] else 1)
