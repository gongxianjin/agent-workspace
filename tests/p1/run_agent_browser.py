"""Real isolated progress UI with explicit state fixtures; no model or user data."""
import copy
import json
from pathlib import Path
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from server import Application
from environment import source_or_bundle
from winjob import JobProcess

output = ROOT / 'dist/p1' / ('agent-browser-' + time.strftime('%H%M%S'))
output.mkdir(parents=True, exist_ok=False)
app = Application(output / 'runtime', port=0, workers=False)
thread = None
launch = output / 'launch-private.json'
try:
    project = app.store.create_project('Agent进度夹具（非模型执行）')
    def command(kind, payload=None):
        current = app.store.get(project['id'])
        return app.store.command(project['id'], uuid.uuid4().hex, current['revision'], kind, payload or {})
    tasks = [dict(id='D1', title='页面结构', owner='developer', detail='夹具：页面结构', acceptance=['夹具断言'], dependsOn=[]),
        dict(id='D2', title='交互与存储', owner='developer', detail='夹具：交互与存储', acceptance=['夹具断言'], dependsOn=['D1']),
        dict(id='T1', title='独立验收', owner='tester', detail='夹具：独立验收', acceptance=['夹具断言'], dependsOn=['D1', 'D2'])]
    command('upload', {'name': 'fixture.md', 'text': '# 明示测试夹具，不是真实模型设计'})
    command('split')
    attempt = app.store.claim_attempt(project['id'])
    app.store.complete_attempt(attempt['id'], {'tasks': copy.deepcopy(tasks)})
    for task in app.store.get(project['id'])['tasks']:
        command('approve', {'taskId': task['id'], 'taskVersion': task['version'], 'documentVersion': task['documentVersion']})
    command('start')
    attempt = app.store.claim_attempt(project['id'])
    app.store.complete_attempt(attempt['id'], {'summary': '明示测试夹具，不是真实开发完成', 'artifacts': []})
    attempt = app.store.claim_attempt(project['id'])
    scope = dict(projectId=project['id'], attemptId=attempt['id'], epoch=attempt['epoch'], runId=attempt['runId'], inputHash=attempt['inputHash'])
    app.telemetry.begin(scope)
    app.telemetry.append(scope, {'kind': 'tool', 'tool': 'write_project', 'phase': 'finished', 'outcome': 'ok'})
    thread = threading.Thread(target=app.serve, daemon=True)
    thread.start()
    launch.write_text(json.dumps({'urls': [app.origin + '/#bootstrap=' + app.issue_bootstrap()]}), encoding='utf-8')
    paths = source_or_bundle(ROOT)
    child = JobProcess([str(paths.node), str(ROOT / 'tests/p1/agent-browser.cjs'), str(launch), str(output),
        str(paths.playwright), str(paths.chromium)], ROOT)
    try:
        result = child.wait(65)
    finally:
        child.close()
    (output / 'process.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(result['stdout'])
    print(result['stderr'])
finally:
    app.close()
    if thread:
        thread.join(timeout=3)
    if launch.exists():
        launch.write_text('{"stopped":true}', encoding='utf-8')
print('Evidence: ' + str(output))
raise SystemExit(result['exitCode'] if result['treeStopped'] else 1)
