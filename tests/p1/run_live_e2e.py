"""Explicitly paid real E2E; isolated managed projects, never automated acceptance."""
import json
from pathlib import Path
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from server import Application
from runtime import NODE
from winjob import JobProcess

output = ROOT / 'dist/p1' / ('live-e2e-' + time.strftime('%Y%m%d-%H%M%S'))
output.mkdir(parents=True, exist_ok=False)
app = Application(output / 'runtime', port=0, workers=True)
thread = threading.Thread(target=app.serve, daemon=True)
thread.start()
launch = output / 'launch-private.json'
launch.write_text(json.dumps({'url': app.origin + '/#bootstrap=' + app.bootstrap}), encoding='utf-8')
finished = threading.Event()


def observe_stop_process():
    request = output / 'wait-stop-process.json'
    while not finished.wait(.1):
        if not request.is_file():
            continue
        try:
            project_id = json.loads(request.read_text(encoding='utf-8'))['projectId']
            with app.engine.lock:
                runner = app.engine.runner
                if app.engine.active_project != project_id or not runner:
                    continue
                with runner.lock:
                    child = runner.active
                    if not child or child.poll() is not None:
                        continue
                    record = {'projectId': project_id, 'pid': child.pid, 'activeProcesses': child.active_processes(), 'time': time.time()}
            (output / 'stop-process-ready.json').write_text(json.dumps(record), encoding='utf-8')
            return
        except (OSError, ValueError, KeyError):
            continue


observer = threading.Thread(target=observe_stop_process, daemon=True)
observer.start()
result = None
print('Running paid real E2E; evidence: ' + str(output), flush=True)
try:
    child = JobProcess([str(NODE), str(ROOT / 'tests/p1/live-e2e.cjs'), str(launch), str(output)], ROOT)
    try:
        result = child.wait(7200)
    finally:
        child.close()
    print(result['stdout'], flush=True)
    print(result['stderr'], flush=True)
    (output / 'process.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
finally:
    finished.set()
    observer.join(timeout=3)
    try:
        app.close()
        thread.join(timeout=3)
        (output / 'service-cleanup.json').write_text(json.dumps({'closed': app.closed, 'engineThreadAlive': app.engine.thread.is_alive(), 'httpThreadAlive': thread.is_alive(), 'previewThreadAlive': app.preview_thread.is_alive(), 'reasonJobClosed': not app.engine.transport or app.engine.transport.daemon is None}, indent=2), encoding='utf-8')
    finally:
        launch.write_text('{"stopped":true}', encoding='utf-8')
        (output / 'states-after-shutdown.json').write_text(json.dumps(app.store.list_projects(), ensure_ascii=False, indent=2), encoding='utf-8')
print('Evidence: ' + str(output), flush=True)
sys.exit(result['exitCode'] if result and result['treeStopped'] and not result['timedOut'] and not result['outputLimited'] else 1)
