"""Drive production onboarding and fixed non-model probes in a disposable source root."""
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

# Genuine package terms must remain the user's decision; this source-only check cannot accept them.
if (ROOT / 'portable.json').exists() or (ROOT / 'vendor').exists():
    raise ValueError('This check is source-only, not a package-consent bypass')
paths = source_or_bundle(ROOT)
output = ROOT / 'dist/p1' / ('real-onboarding-' + time.strftime('%H%M%S'))
output.mkdir(parents=True, exist_ok=False)
app = Application(output / 'runtime', port=0, workers=False)
thread = threading.Thread(target=app.serve, daemon=True)
thread.start()
launch = output / 'launch-private.json'
launch.write_text(json.dumps({'url': app.origin + '/#bootstrap=' + app.bootstrap}), encoding='utf-8')
try:
    child = JobProcess([str(paths.node), str(ROOT / 'tests/p1/onboarding-browser.cjs'), str(launch), str(output),
        str(paths.playwright), str(paths.chromium)], ROOT)
    try:
        result = child.wait(150)
    finally:
        child.close()
    print(result['stdout'])
    print(result['stderr'])
    (output / 'process.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
finally:
    app.close()
    thread.join(timeout=5)
    launch.write_text('{"stopped":true}', encoding='utf-8')
print('Evidence: ' + str(output))
sys.exit(0 if result['exitCode'] == 0 and result['treeStopped'] and not result['timedOut'] else 1)
