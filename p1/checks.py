"""Trusted fixed browser checks. Model text cannot mint evidence or execute host JS."""
import copy
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import threading
import uuid

from runtime import BASE, PATHS, tree_manifest
from reason_mcp import safe_path
from process_control import JobProcess

ASSERTIONS = {'text', 'count', 'visible', 'noOverflow', 'noErrors', 'value'}
FIELDS = {
    'fill': {'op', 'selector', 'value'}, 'click': {'op', 'selector'},
    'text': {'op', 'selector', 'value'}, 'value': {'op', 'selector', 'value'},
    'count': {'op', 'selector', 'value'}, 'visible': {'op', 'selector'},
    'reload': {'op'}, 'noOverflow': {'op'}, 'noErrors': {'op'},
    'viewport': {'op', 'width', 'height'}, 'storageFailure': {'op'},
}


def validate_spec(spec):
    if not isinstance(spec, dict) or set(spec) != {'version', 'cases'} or spec['version'] != 1:
        raise ValueError('Expected browser specification version 1')
    cases = spec['cases']
    if not isinstance(cases, list) or not 1 <= len(cases) <= 20:
        raise ValueError('Nonempty browser cases required, maximum 20')
    for case in cases:
        if not isinstance(case, dict) or set(case) != {'name', 'steps'} or not isinstance(case['name'], str) or not 1 <= len(case['name']) <= 200:
            raise ValueError('Invalid browser case')
        steps = case['steps']
        if not isinstance(steps, list) or not 1 <= len(steps) <= 60:
            raise ValueError('Nonempty case steps required')
        if not any(isinstance(s, dict) and s.get('op') in ASSERTIONS for s in steps):
            raise ValueError('Every case requires an actual assertion')
        for step in steps:
            op = step.get('op') if isinstance(step, dict) else None
            if op not in FIELDS or set(step) != FIELDS[op]:
                raise ValueError('Unknown action/fields; no skip, executable JS or host commands allowed')
            if 'selector' in step and (not isinstance(step['selector'], str) or not 1 <= len(step['selector']) <= 300):
                raise ValueError('Invalid selector')
            if op in ('fill', 'text', 'value') and (not isinstance(step['value'], str) or len(step['value']) > 2000):
                raise ValueError('Invalid text value')
            if op == 'count' and (type(step['value']) is not int or not 0 <= step['value'] <= 500):
                raise ValueError('Invalid count')
            if op == 'viewport' and not (type(step['width']) is int and type(step['height']) is int and 320 <= step['width'] <= 1920 and 480 <= step['height'] <= 1440):
                raise ValueError('Invalid viewport')
    return spec


def artifact_name(name):
    return name in ('report.json', 'evidence.json', 'process.json') or re.fullmatch(r'case-(?:[1-9]|1[0-9]|20)\.(png|zip)', name) is not None


def resolve_artifact(root, evidence_id, name):
    if not re.fullmatch(r'[a-f0-9]{32}', evidence_id) or not artifact_name(name):
        raise ValueError('Unknown artifact')
    root = Path(root)
    record = json.loads(safe_path(root, 'check-index/' + evidence_id + '.json').read_text(encoding='utf-8'))
    relative = record['directory']
    if not relative.startswith('evidence/') or name not in record['artifacts']:
        raise ValueError('Unknown artifact')
    target = safe_path(root, relative + '/' + name)
    if target.stat().st_size > 32 * 1024 * 1024 or hashlib.sha256(target.read_bytes()).hexdigest() != record['artifacts'][name]:
        raise ValueError('Artifact bytes changed')
    return target


class CheckRunner:
    def __init__(self, index_root=None, profile='generic', paths=None):
        self.paths = paths or PATHS
        if profile not in ('generic', 'liuwa-v1'):
            raise ValueError('Unknown fixed acceptance profile')
        self.profile = profile
        self.index_root = Path(index_root) if index_root else None
        self.issued = {}
        self.active = None
        self.lock = threading.RLock()
        self.cancelled = False
        self.failure = None

    def stop(self):
        with self.lock:
            self.cancelled = True
            return not self.active or self.active.stop()

    def run(self, workspace, evidence_dir, timeout=180):
        self.failure = None
        try:
            return self._run(workspace, evidence_dir, timeout)
        except Exception as error:
            if isinstance(error, OSError) and getattr(error, 'winerror', None) == 206:
                error = ValueError('path_too_long：浏览器检查路径超过Windows支持的长度；请正常退出后将整个工作台移到更短的本地可写目录，再明确重试。')
            self.failure = (Path(workspace).resolve(), str(error)[:2000])
            raise error

    def _run(self, workspace, evidence_dir, timeout):
        if self.cancelled:
            raise ValueError('Check cancelled before start')
        workspace = Path(workspace)
        source = safe_path(workspace, 'tests/browser.json')
        if source.stat().st_size > 256 * 1024:
            raise ValueError('Browser specification exceeds limit')
        validate_spec(json.loads(source.read_text(encoding='utf-8')))
        product, tests = tree_manifest(workspace / 'product'), tree_manifest(workspace / 'tests')
        if not any(f['path'] == 'index.html' for f in product['files']):
            raise ValueError('Product index.html required')
        check_id = uuid.uuid4().hex
        directory = (self.index_root.parent / 'evidence' / check_id if self.index_root else
            Path(evidence_dir) / ('check-' + check_id))
        snapshot = directory / 'snapshot'
        targets = [directory / name for name in ('report.json', 'evidence.json', 'process.json', 'case-20.png', 'case-20.zip')]
        targets += [snapshot / area / row['path'] for area, manifest in [('product', product), ('tests', tests)] for row in manifest['files']]
        if self.index_root:
            targets.append(self.index_root / (check_id + '.json'))
        if os.name == 'nt' and any(len(str(target.resolve()).encode('utf-16-le')) // 2 >= 248 for target in targets):
            raise ValueError('path_too_long：浏览器检查路径超过Windows支持的长度；尚未创建测试快照或启动浏览器。请正常退出后将整个工作台移到更短的本地可写目录，再明确重试。')
        directory.mkdir(parents=True, exist_ok=False)
        for area, manifest in [('product', product), ('tests', tests)]:
            for row in manifest['files']:
                target = snapshot / area / row['path']
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(safe_path(workspace / area, row['path']), target)
            if tree_manifest(snapshot / area) != manifest:
                raise ValueError('Snapshot changed while freezing')
        argv = [str(self.paths.node), str(BASE / 'p1/checks.cjs'), str(snapshot / 'product'), str(snapshot / 'tests/browser.json'),
            str(directory), self.profile, str(self.paths.playwright), str(self.paths.chromium)]
        with self.lock:
            if self.active:
                raise ValueError('Another check is running')
            if self.cancelled:
                raise ValueError('Check cancelled before process start')
            self.active = JobProcess(argv, BASE, output_limit=4 * 1024 * 1024)
        try:
            process = self.active.wait(timeout)
        finally:
            with self.lock:
                self.active.close()
                self.active = None
        (directory / 'process.json').write_text(json.dumps({'argv': argv, **process}, ensure_ascii=False, indent=2), encoding='utf-8')
        if self.cancelled or not process['treeStopped'] or process['timedOut'] or process['outputLimited']:
            raise ValueError('Browser check interrupted or process tree unconfirmed')
        report = json.loads((directory / 'report.json').read_text(encoding='utf-8'))
        if type(report.get('assertions')) is not int or report['assertions'] < 1:
            raise ValueError('Actual nonempty assertions required')
        if tree_manifest(workspace / 'product') != product or tree_manifest(workspace / 'tests') != tests:
            raise ValueError('Inputs changed during browser check')
        evidence = {'id': check_id, 'exitCode': process['exitCode'], 'assertions': report['assertions'],
            'productHash': product['sha256'], 'specHash': tests['sha256'], 'directory': str(directory),
            'cases': report['cases']}
        (directory / 'evidence.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2), encoding='utf-8')
        if self.index_root:
            relative = directory.resolve().relative_to(self.index_root.parent.resolve()).as_posix()
            if not relative.startswith('evidence/'):
                raise ValueError('Evidence must remain in the owned evidence tree')
            artifacts = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in directory.iterdir()
                if p.is_file() and artifact_name(p.name)}
            self.index_root.mkdir(parents=True, exist_ok=True)
            (self.index_root / (check_id + '.json')).write_text(json.dumps({'directory': relative, 'artifacts': artifacts}), encoding='utf-8')
        self.issued[check_id] = copy.deepcopy(evidence)
        return evidence

    def verify(self, evidence_id, workspace):
        evidence = self.issued.get(evidence_id)
        if not evidence and self.failure and self.failure[0] == Path(workspace).resolve():
            raise ValueError('浏览器检查未生成有效证据；原始原因：' + self.failure[1])
        if not evidence or evidence['productHash'] != tree_manifest(Path(workspace) / 'product')['sha256'] or evidence['specHash'] != tree_manifest(Path(workspace) / 'tests')['sha256']:
            raise ValueError('Unknown evidence or changed product/test bytes')
        return copy.deepcopy(evidence)
