"""Paid real CLI+reason probes. No scripted model replies and no user project writes."""
import json
from pathlib import Path
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from runtime import REASON, RoleRunner
from reason_mcp import ReasonTransport

run_root = ROOT / 'dist/p1' / ('probes-' + time.strftime('%H%M%S'))
run_root.mkdir(parents=True, exist_ok=False)
transport = ReasonTransport(REASON, run_root / 'reason-home', ROOT)
results = []
try:
    transport.start()
    selected = sys.argv[1:] or ['architect', 'developer', 'tester']
    if not selected or any(role not in ('architect', 'developer', 'tester') for role in selected):
        raise ValueError('Expected role names only')
    for client, role in [('Claude Code', role) for role in selected]:
        attempt_id = uuid.uuid4().hex
        workspace = run_root / 'workspaces' / attempt_id
        (workspace / 'product').mkdir(parents=True)
        (workspace / 'tests').mkdir()
        (workspace / 'product/probe.txt').write_text('scoped read works', encoding='utf-8')
        attempt = {'id': attempt_id, 'projectId': 'new-probe-only', 'runId': None, 'epoch': 1,
            'role': role, 'inputHash': 'nonce-read-probe'}
        runner = RoleRunner(transport, run_root / 'evidence')
        payload = {'request': 'This is only an authorized integration probe. After real receive_task and send_ack, read product/probe.txt, call run_check(inventory), then send_result with {summary:string, artifacts:[]} describing the read content and real check. Do not write product or run extra tools.',
            'resultShape': {'summary': 'string', 'artifacts': []}}
        try:
            evidence = runner.run(attempt, client, workspace, payload, timeout=180)
            results.append({'client': client, 'passed': True, **evidence})
            print(client + ': real ACK/RESULT and clean exit', flush=True)
        except Exception as error:
            import traceback
            (run_root / (attempt_id + '-error.txt')).write_text(traceback.format_exc(), encoding='utf-8')
            results.append({'client': client, 'passed': False, 'role': role, 'attemptId': attempt_id, 'error': str(error)})
            print(client + ': BLOCKED: ' + str(error), flush=True)
        (run_root / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
finally:
    transport.close()
    (run_root / 'transport.json').write_text(json.dumps(transport.audit, ensure_ascii=False, indent=2), encoding='utf-8')
print('Evidence: ' + str(run_root), flush=True)
sys.exit(0 if len(results) == len(selected) and all(r['passed'] for r in results) else 1)
