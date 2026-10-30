"""Capture the installed reason text contract in a new isolated HOME, no AI."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('wj', ROOT / 'p1/winjob.py')
wj = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wj)
exe = ROOT.parent / 'reason-network-v0.10.8-portable/extracted/reason-network.exe'
home = ROOT / 'dist/p1/reason-sample-home'
home.mkdir(parents=True, exist_ok=False)
env = dict(os.environ, REASON_NETWORK_HOME=str(home), REASON_NETWORK_NO_SERVICE='1', REASON_NETWORK_NO_UPDATE='1')
records = []
daemon = wj.JobProcess([str(exe), 'daemon'], ROOT, env=env)
try:
    time.sleep(1)
    def call(args, source=b''):
        p = wj.JobProcess([str(exe), *args], ROOT, env=env, input_data=source)
        try:
            result = p.wait(15)
            records.append({'args': args, **result})
            if result['exitCode'] != 0:
                raise RuntimeError(result['stderr'] or result['stdout'])
            return result['stdout']
        finally:
            p.close()
    call(['listen', '--as', 'sample-control', '--wait', '0'])
    call(['listen', '--as', 'sample-worker', '--wait', '0'])
    directory = json.loads(call(['who', '--all', '--json']))
    target = next(s['address'] for s in directory['sessions'] if s['name'] == 'sample-worker')
    call(['send', target, '--from', 'sample-control', '-'], json.dumps({'kind': 'SAMPLE', 'nonce': 'format-contract-only', 'body': 'hello\nworld'}).encode())
    call(['listen', '--as', 'sample-worker', '--wait', '1'])
finally:
    daemon.stop()
    result = daemon.wait(5)
    records.append({'args': ['daemon'], **result})
    daemon.close()
    (ROOT / 'dist/p1/reason-contract-sample.json').write_text(json.dumps(records, ensure_ascii=False, indent=2), encoding='utf-8')
print(json.dumps(records, ensure_ascii=False, indent=2))
