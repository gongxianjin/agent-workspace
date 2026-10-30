"""Serial role scheduling; only trusted execution paths can complete attempts."""
import json
from pathlib import Path
import threading
import time

from checks import CheckRunner, FIELDS
from runtime import REASON, RoleRunner, tree_manifest
from reason_mcp import ReasonTransport
from state import Rejected, TASK_FIELDS
from telemetry import TelemetryStore

TASK_SCHEMA = {'id': 'D1', 'owner': 'developer or tester', 'title': 'string', 'detail': 'string', 'acceptance': ['nonempty string'], 'dependsOn': ['existing task ID']}


def payload_for(attempt, frozen=False):
    kind, context = attempt['kind'], attempt['input']
    common = ('Respond in Chinese. Only implement the approved document and task scope. This platform currently supports dependency-free static HTML/CSS/JS only. '
        'No login, external requests, deployment, installs or build toolchain. Use small UTF-8 files. Locations must be explicitly unverified examples. ')
    shapes = {
        'generate': {'markdown': 'Markdown design string', 'criteria': ['acceptance string']},
        'split': {'tasks': [TASK_SCHEMA]}, 'regenerate': {'task': TASK_SCHEMA},
        'develop': {'summary': 'string', 'artifacts': ['product/index.html']},
        'repair': {'summary': 'string', 'artifacts': ['product/index.html']},
        'test': {'summary': 'string', 'evidenceId': 'actual run_check(browser) id'},
    }
    requests = {
        'generate': 'Generate a concise implementable design document from requirement. Include goals, non-goals, static files, data, stable DOM selectors and executable acceptance criteria. Do not create tasks or code yet.',
        'split': 'Split ONLY the uploaded document into 2-6 small tasks, preferably two independent developer tasks D1/D2 and one tester T1 depending on every developer task. No developer depends on tester. Tester must transitively depend on ALL developers. Unique IDs, no cycles. Return only task schema fields. Never provide approvals or versions. Each developer task must define file ownership and meaningful acceptance. Avoid changing unrelated task scopes.',
        'regenerate': 'Replace ONLY input.target according to rejectionReason. Preserve its id and owner, do not change any other task. Include concrete acceptance resolving the rejection. Return one task, not the entire list.',
        'develop': 'Read existing product files as needed; implement ONLY input.target and approved document using write_project. Integrate with prior files without breaking approved scope. Call run_check(inventory) after writing. Do not modify tests/. Deliver usable static product/index.html; do not just describe code.',
        'repair': 'Read the actual lastEvidence and relevant product files. Fix failing assertions ONLY within approved developer scope. Never modify tests or expectation/specification. Use write_project then inventory. The independent tester will rerun the same frozen specification.',
        'test': 'Independently inspect approved document, all tasks and product using read_project. ' + (
            'Existing tests/browser.json is FROZEN; do not edit any tests. Run the same specification again. ' if frozen else
            'Write tests/browser.json with the strict declaration schema described below. Include real search/category/empty-result/favorite+reload/storageFailure/narrow-screen checks as required by the document, using actual product selectors and data. Each case starts with fresh browser storage. '
        ) + 'The first completed run_check(browser) freezes ALL tests/ files, including any acceptance report; finish file writing BEFORE that call. After it, put findings/blockers in send_result.summary, not files. Call run_check(browser), inspect actual exitCode/cases, then send_result even if tests fail. Reference the actual evidence id; never self-report pass. No host JS, eval, skip, or commands in test specification.',
    }
    payload = {'request': common + requests[kind], 'input': context, 'resultShape': shapes[kind]}
    if kind == 'test':
        payload['browserSpecification'] = {'version': 1, 'cases': [{'name': 'case name', 'steps': [{'op': 'text', 'selector': '#actual-element', 'value': 'expected substring'}, {'op': 'noOverflow'}, {'op': 'noErrors'}]}]}
        payload['allowedSteps'] = {op: sorted(keys) for op, keys in FIELDS.items()}
        payload['specRules'] = 'Only these fields per step. count.value integer; fill/text/value.value string. viewport width 320..1920 height 480..1440. No optional fields. storageFailure causes reload with setItem throwing; then interact and verify no page errors. Every case must contain actual assertions. Maximum 20 cases, 60 steps each.'
    return payload


class Engine:
    def __init__(self, store, setup=None, telemetry=None):
        self.store = store
        self.setup = setup
        self.telemetry = telemetry or TelemetryStore(store.root)
        self.transport = None
        self.runner = None
        self.checker = CheckRunner(index_root=self.store.root / 'check-index')
        self.active_project = None
        self.wake = threading.Event()
        self.closed = threading.Event()
        self.lock = threading.RLock()
        self.idle = threading.Condition(self.lock)
        self.stop_proofs = {}
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.thread.start()

    def notify(self):
        self.wake.set()

    def stop(self, project_id):
        deadline = time.monotonic() + 12
        with self.idle:
            if self.active_project == project_id:
                # Cancel process capabilities first, then wait for the Python host stage too.
                try:
                    if self.runner:
                        self.runner.stop()
                    self.checker.stop()
                except Exception:
                    return False
                while self.active_project == project_id:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        return False
                    self.idle.wait(remaining)
            state = self.store.get(project_id)
            if state['recoveryRequired']:
                return False
            if state['leaseUnknown']:
                proof = self.stop_proofs.get(project_id)
                last = state['activeAttempt'] or state['lastAttempt']
                return bool(proof and proof['confirmed'] and last and proof['attemptId'] == last['id'])
            return True

    def _loop(self):
        while not self.closed.is_set():
            self.wake.wait(.5)
            self.wake.clear()
            states = self.store.list_projects()
            if any(state['leaseUnknown'] for state in states):
                continue
            for state in states:
                if self.closed.is_set():
                    return
                attempt = state['activeAttempt']
                if attempt and attempt['status'] == 'queued' and not state['leaseUnknown']:
                    self._execute(state['id'])
                    break

    def _execute(self, project_id):
        attempt = scope = None
        def emit(event):
            if scope:
                try:
                    self.telemetry.append(scope, event)
                except Exception:
                    pass
        with self.lock:
            self.active_project = project_id
        try:
            executable = self.setup.snapshot() if self.setup else None
            attempt = self.store.claim_attempt(project_id)
            scope = {'projectId': project_id, 'attemptId': attempt['id'], 'epoch': attempt['epoch'],
                'runId': attempt['runId'], 'inputHash': attempt['inputHash']}
            try:
                self.telemetry.begin(scope)
            except Exception:
                scope = None
            emit({'kind': 'stage', 'stage': 'preparing'})
            with self.lock:
                self.checker = CheckRunner(index_root=self.store.root / 'check-index')
                self.runner = RoleRunner(self.transport, self.store.root / 'evidence')
                self.runner.telemetry = emit
                if executable:
                    self.runner.executable = executable
            state = self.store.get(project_id)
            if state['uploadedDocument'] and 'relay-acceptance: liuwa-v1' in state['uploadedDocument']['text']:
                self.checker.profile = 'liuwa-v1'
            if state['epoch'] != attempt['epoch'] or state['phase'] == 'stopping':
                raise ValueError('Attempt cancelled before dispatch')
            if set(state['roles'].values()) != {'Claude Code'}:
                raise ValueError('本轮真实模式只支持三个独立Claude Code岗位。')
            if not self.transport:
                self.transport = ReasonTransport(REASON, self.store.root / ('reason-' + str(time.time_ns())), self.store.root)
                self.transport.start()
            self.runner.transport = self.transport
            workspace = self.store.root / 'projects' / project_id
            freeze = self.store.root / 'frozen-tests' / (attempt['runId'] or attempt['id'])
            frozen = freeze.is_file()
            expected_spec = freeze.read_text(encoding='utf-8') if frozen else None
            if frozen and tree_manifest(workspace / 'tests')['sha256'] != expected_spec:
                raise ValueError('冻结测试规格已变化，禁止降低期望。')
            before_product = tree_manifest(workspace / 'product')
            before_tests = tree_manifest(workspace / 'tests')
            evidence = self.runner.run(attempt, 'Claude Code', workspace, payload_for(attempt, frozen),
                checker=self.checker.run if attempt['role'] == 'tester' else None,
                allow_test_writes=not frozen)
            emit({'kind': 'stage', 'stage': 'host_validation'})
            if attempt['role'] != 'developer' and tree_manifest(workspace / 'product') != before_product:
                raise ValueError('非开发岗位改变了产品快照。')
            if attempt['role'] != 'tester' and tree_manifest(workspace / 'tests') != before_tests:
                raise ValueError('非测试岗位改变了测试规格。')
            checked = None
            if attempt['kind'] == 'test':
                checked = self.checker.verify(evidence['result'].get('evidenceId'), workspace)
                if not any(c['id'] == checked['id'] for c in evidence['checks']):
                    raise ValueError('该证据不属于当前测试attempt。')
                spec_hash = tree_manifest(workspace / 'tests')['sha256']
                if frozen and spec_hash != expected_spec:
                    raise ValueError('复测修改了冻结规格。')
                freeze.parent.mkdir(parents=True, exist_ok=True)
                if not frozen:
                    freeze.write_text(spec_hash, encoding='utf-8')
                # An independent trusted rerun, not a second model self-report.
                emit({'kind': 'stage', 'stage': 'trusted_rerun'})
                repeat = self.checker.run(workspace, self.store.root / 'evidence' / attempt['id'])
                if checked['exitCode'] != repeat['exitCode'] or checked['productHash'] != repeat['productHash'] or checked['specHash'] != repeat['specHash']:
                    raise ValueError('冻结规格复跑结果不一致。')
                checked['rerunId'] = repeat['id']
            self.store.record_execution(attempt['id'], evidence)
            self.store.complete_attempt(attempt['id'], evidence['result'], evidence=checked)
        except Exception as error:
            if attempt:
                try:
                    current = self.store.get(project_id)
                    if current['phase'] != 'stopping':
                        self.store.fail_attempt(attempt['id'], str(error), confirmed=self._quiesce())
                except Rejected:
                    pass
        finally:
            confirmed = self._quiesce()
            if scope:
                try:
                    self.telemetry.seal(scope)
                except Exception:
                    pass
            with self.idle:
                if attempt:
                    self.stop_proofs[project_id] = {'attemptId': attempt['id'], 'confirmed': confirmed}
                self.active_project = None
                self.runner = None
                self.idle.notify_all()
            if attempt:
                self.wake.set()

    def _quiesce(self):
        try:
            role_stopped = self.runner.stop() if self.runner else True
            check_stopped = self.checker.stop()
            return role_stopped and check_stopped
        except Exception:
            return False

    def close(self):
        self.closed.set()
        with self.lock:
            if self.runner:
                self.runner.stop()
            self.checker.stop()
        self.wake.set()
        self.thread.join(timeout=15)
        if self.thread.is_alive():
            raise RuntimeError('执行线程未停止，不能确认租约释放。')
        if self.transport:
            self.transport.close()
