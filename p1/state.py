"""Authoritative project state. Model output is data, never an approval."""
import copy
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import threading
import time
import uuid

from guidance import Guidance, SCHEMA as GUIDANCE_SCHEMA


class Rejected(ValueError):
    pass


def require(condition, message='操作不符合当前状态或版本。'):
    if not condition:
        raise Rejected(message)


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


def digest(value):
    return hashlib.sha256(encoded(value).encode('utf-8')).hexdigest()


def text(value, limit, label):
    require(isinstance(value, str) and bool(value.strip()) and len(value) <= limit, f'{label}为空或超过限制。')
    require(not any(ord(c) < 32 and c not in '\n\r\t' for c in value) and '\x7f' not in value, f'{label}含非文本字符。')
    try:
        value.encode('utf-8', errors='strict')
    except UnicodeError as error:
        raise Rejected(f'{label}不是有效UTF-8。') from error
    return value


def fields(value, expected):
    require(isinstance(value, dict) and set(value) == set(expected), '字段不符合约定。')


def validate_task(task):
    fields(task, ['id', 'owner', 'title', 'detail', 'acceptance', 'dependsOn'])
    require(isinstance(task['id'], str) and re.fullmatch(r'[A-Za-z][A-Za-z0-9_-]{0,31}', task['id']), '任务ID无效。')
    require(task['owner'] in ('developer', 'tester'), '任务岗位无效。')
    text(task['title'], 200, '任务标题')
    text(task['detail'], 8000, '任务说明')
    require(isinstance(task['acceptance'], list) and 1 <= len(task['acceptance']) <= 30, '任务必须有验收点。')
    for point in task['acceptance']:
        text(point, 2000, '验收点')
    deps = task['dependsOn']
    require(isinstance(deps, list) and all(isinstance(d, str) for d in deps) and len(set(deps)) == len(deps), '依赖无效。')
    return copy.deepcopy(task)


def validate_tasks(tasks):
    require(isinstance(tasks, list) and 2 <= len(tasks) <= 30, '清单须有开发与测试任务，最多30项。')
    checked = [validate_task(t) for t in tasks]
    by_id = {t['id']: t for t in checked}
    require(len(by_id) == len(checked), '任务ID重复。')
    require({t['owner'] for t in checked} == {'developer', 'tester'}, '清单缺开发或测试岗位。')
    visiting, visited = set(), set()

    def visit(task_id):
        require(task_id in by_id and task_id not in visiting, '依赖缺失或形成环。')
        if task_id in visited:
            return
        visiting.add(task_id)
        for dep in by_id[task_id]['dependsOn']:
            visit(dep)
        visiting.remove(task_id)
        visited.add(task_id)
    for task_id in by_id:
        visit(task_id)
    # A developer must never depend on a tester: testing is of a completed snapshot.
    require(all(by_id[d]['owner'] == 'developer' for t in checked if t['owner'] == 'developer' for d in t['dependsOn']), '开发任务不能依赖测试任务。')
    developers = {t['id'] for t in checked if t['owner'] == 'developer'}
    for item in checked:
        if item['owner'] == 'tester':
            reachable = set()
            def gather(task_id):
                for dep in by_id[task_id]['dependsOn']:
                    if dep not in reachable:
                        reachable.add(dep)
                        gather(dep)
            gather(item['id'])
            require(developers <= reachable, '测试任务须依赖全部开发任务，确保冻结交付后测试。')
    return checked


def approved(state):
    doc, tasks = state['uploadedDocument'], state['tasks']
    if not doc or not tasks or doc['contextRevision'] != state['contextRevision']:
        return False
    by_id = {t['id']: t for t in tasks}
    if len(by_id) != len(tasks):
        return False
    for task in tasks:
        a = task['approval']
        binding = {'documentVersion': doc['version'], 'documentHash': doc['sha256'],
            'contextRevision': state['contextRevision'], 'taskVersion': task['version'],
            'dependencies': {d: by_id[d]['version'] for d in task['dependsOn'] if d in by_id}}
        if task['status'] != 'approved' or a != binding or len(binding['dependencies']) != len(task['dependsOn']):
            return False
    return True


class Store:
    def __init__(self, root, recover=True):
        self.root = Path(root).resolve()
        self.root.mkdir(parents=True, exist_ok=True)
        self.db = self.root / 'workbench.sqlite3'
        self.lock = threading.RLock()
        self.guidance = Guidance(require, text, fields, digest, encoded)
        with self._connect() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY, state TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS commands(project TEXT, id TEXT, fingerprint TEXT, response TEXT, PRIMARY KEY(project,id));
                CREATE TABLE IF NOT EXISTS attempts(id TEXT PRIMARY KEY, project TEXT, epoch INTEGER, status TEXT, data TEXT);
                CREATE TABLE IF NOT EXISTS project_creations(id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, project TEXT NOT NULL, response TEXT NOT NULL, created_at REAL NOT NULL);
            ''' + GUIDANCE_SCHEMA)
            if recover:
                db.execute('BEGIN IMMEDIATE')
                db.execute("UPDATE agent_messages SET status='expired',nonce=NULL,updated_at=? WHERE status IN ('queued','received','needs_confirmation')", (time.time(),))
                for row in db.execute('SELECT state FROM projects').fetchall():
                    state = json.loads(row[0])
                    suspended = state.get('agentControl', {}).get('resumable', False)
                    if suspended:
                        state['agentControl'].update(status='expired', resumable=False)
                        db.execute('DELETE FROM agent_controls WHERE project=?', (state['id'],))
                    if state['activeAttempt'] or state['phase'] in ('paused', 'stopping') or suspended:
                        state['epoch'] += 1
                        state['phase'] = 'blocked'
                        state['leaseUnknown'] = True
                        state['recoveryRequired'] = True
                        if state['activeAttempt']:
                            db.execute('UPDATE attempts SET status=? WHERE id=?', ('interrupted', state['activeAttempt']['id']))
                        state['activeAttempt'] = None
                        state['revision'] += 1
                        self._event(state, '服务重启：旧运行已中断，确认旧进程树停止前禁止重新开工。')
                        self._save(db, state)
                    elif 'agentInbox' in state:
                        self._save(db, state)

    @contextmanager
    def _connect(self):
        db = sqlite3.connect(self.db, timeout=10)
        try:
            db.execute('PRAGMA busy_timeout=10000')
            with db:
                yield db
        finally:
            db.close()

    def _get(self, db, project_id):
        row = db.execute('SELECT state FROM projects WHERE id=?', (project_id,)).fetchone()
        require(row is not None, '项目不存在。')
        return json.loads(row[0])

    def _save(self, db, state):
        self.guidance.refresh(db, state)
        state['canStart'] = state['phase'] == 'approved' and not state['activeAttempt'] and not state['leaseUnknown'] and approved(state)
        db.execute('INSERT OR REPLACE INTO projects VALUES(?,?)', (state['id'], encoded(state)))
        return copy.deepcopy(state)

    def get(self, project_id):
        with self.lock, self._connect() as db:
            return self._get(db, project_id)

    def attempt_info(self, project_id, attempt_id):
        with self.lock, self._connect() as db:
            row = db.execute('SELECT data,status FROM attempts WHERE id=? AND project=?', (attempt_id, project_id)).fetchone()
            require(row is not None, '阶段不存在或不属于此项目。')
            attempt = json.loads(row[0])
            return {**{name: attempt[name] for name in ('id', 'projectId', 'epoch', 'runId', 'inputHash', 'role', 'kind', 'createdAt')}, 'status': row[1]}

    def list_projects(self):
        with self.lock, self._connect() as db:
            return [json.loads(row[0]) for row in db.execute('SELECT state FROM projects ORDER BY rowid')]

    @staticmethod
    def _event(state, message, channel='workflow', **details):
        state['events'].append({'number': len(state['events']) + 1, 'time': time.time(),
            'channel': channel, 'message': message, 'simulated': False, **details})

    def create_project(self, name, command_id=None):
        text(name, 100, '项目名')
        if command_id is not None:
            require(isinstance(command_id, str) and 1 <= len(command_id) <= 100, 'commandId无效。')
        fingerprint = digest({'name': name})
        created = []
        try:
            with self.lock, self._connect() as db:
                db.execute('BEGIN IMMEDIATE')
                if command_id is not None:
                    previous = db.execute('SELECT fingerprint,response FROM project_creations WHERE id=?', (command_id,)).fetchone()
                    if previous:
                        require(previous[0] == fingerprint, '同commandId不能用于不同载荷。')
                        return json.loads(previous[1])
                state = {'id': uuid.uuid4().hex, 'name': name, 'epoch': 1, 'revision': 1, 'contextRevision': 1,
                    'phase': 'draft', 'requirement': '做一个本地上海遛娃工具，搜索、分类、收藏、手机可用，所有地点待核实，不登录、不部署。',
                    'roles': dict.fromkeys(('architect', 'developer', 'tester'), 'Claude Code'),
                    'generatedDocument': None, 'uploadedDocument': None, 'documentCounter': 0, 'generationCounter': 0,
                    'taskCounter': 0, 'tasks': [], 'events': [], 'activeAttempt': None, 'run': None,
                    'leaseUnknown': False, 'recoveryRequired': False, 'pauseRequested': False, 'lastError': '', 'lastAttempt': None}
                parent = self.root / 'projects'
                parent.mkdir(exist_ok=True)
                directory = parent / state['id']
                for target in (directory, directory / 'product', directory / 'tests'):
                    target.mkdir()
                    created.append(target)
                response = self._save(db, state)
                if command_id is not None:
                    db.execute('INSERT INTO project_creations VALUES(?,?,?,?,?)',
                        (command_id, fingerprint, state['id'], encoded(response), time.time()))
                return response
        except BaseException:
            for target in reversed(created):
                try:
                    target.rmdir()
                except OSError:
                    pass  # Never remove unknown files or a nonempty project directory.
            raise

    @staticmethod
    def _receipt(scope, command_id, project_id, response):
        result = {'found': response is not None, 'scope': scope, 'commandId': command_id, 'projectId': project_id}
        if response is not None:
            state = json.loads(response)
            attempt = state.get('activeAttempt')
            result.update(acceptedRevision=state['revision'], epoch=state['epoch'],
                attemptId=attempt['id'] if attempt else None)
        return result

    def command_receipt(self, project_id, command_id):
        require(isinstance(command_id, str) and 1 <= len(command_id) <= 100, 'commandId无效。')
        with self.lock, self._connect() as db:
            self._get(db, project_id)
            row = db.execute('SELECT response FROM commands WHERE project=? AND id=?', (project_id, command_id)).fetchone()
            return self._receipt('project', command_id, project_id, row[0] if row else None)

    def creation_receipt(self, command_id):
        require(isinstance(command_id, str) and 1 <= len(command_id) <= 100, 'commandId无效。')
        with self.lock, self._connect() as db:
            row = db.execute('SELECT project,response FROM project_creations WHERE id=?', (command_id,)).fetchone()
            return self._receipt('create', command_id, row[0] if row else None, row[1] if row else None)

    def _queue(self, db, state, kind, role, task_id=None, retry_count=0, extra=None):
        require(not state['activeAttempt'] and not state['leaseUnknown'], '旧进程或租约未释放。')
        item = next((t for t in state['tasks'] if t['id'] == task_id), None)
        context = {'requirement': state['requirement'], 'roles': state['roles'],
            'document': state['uploadedDocument'], 'tasks': state['tasks'], 'target': item,
            'lastEvidence': state['run']['evidence'][-1] if state['run'] and state['run']['evidence'] else None}
        if extra:
            context.update(extra)
        attempt = {'id': uuid.uuid4().hex, 'projectId': state['id'], 'epoch': state['epoch'],
            'runId': state['run']['id'] if state['run'] else None, 'kind': kind, 'role': role,
            'taskId': task_id, 'status': 'queued', 'inputHash': digest(context), 'input': context, 'createdAt': time.time(), 'retryCount': retry_count}
        state['activeAttempt'] = attempt
        state['phase'] = {'generate': 'designing', 'split': 'splitting', 'regenerate': 'regenerating',
            'develop': 'developing', 'test': 'testing', 'repair': 'fixing', 'consult': 'consulting'}[kind]
        db.execute('INSERT INTO attempts VALUES(?,?,?,?,?)', (attempt['id'], state['id'], state['epoch'], 'queued', encoded(attempt)))
        state['activeAttempt'] = self._public_attempt(attempt)
        self._event(state, f'{role}：{kind} 已排队。', attemptId=attempt['id'])

    @staticmethod
    def _public_attempt(attempt):
        result = copy.deepcopy(attempt)
        context = result.get('input', {})
        if 'guidance' in context:
            context['guidance'] = [{'id': item['id']} for item in context['guidance']]
        if 'consultation' in context:
            consultation = context['consultation']
            consultation['messageIds'] = [item['id'] for item in consultation.pop('messages', [])]
        return result

    @staticmethod
    def _history(task, kind, message):
        task['history'].append({'kind': kind, 'taskVersion': task['version'], 'documentVersion': task['documentVersion'], 'message': message})

    def _invalidate(self, state, task_id):
        affected = {task_id}
        while True:
            more = {t['id'] for t in state['tasks'] if set(t['dependsOn']) & affected} - affected
            if not more:
                break
            affected |= more
        for task in state['tasks']:
            if task['id'] != task_id and task['id'] in affected and task['status'] in ('approved', 'needs-review'):
                task['status'], task['approval'] = 'needs-review', None
                task['reviewReason'] = f'上游{task_id}已变化，依赖与验收范围需要重新审核。'
                self._history(task, 'needs-review', task['reviewReason'])

    def _next(self, db, state):
        run = state['run']
        require(approved(state), '批准绑定已失效。')
        if state['pauseRequested']:
            state['phase'] = 'paused'
            return
        pending = [t for t in state['tasks'] if t['id'] not in run['done']]
        if not pending:
            state['phase'] = 'ready'
            self._event(state, '实际测试证据通过，等待人工验收。')
            return
        require(time.time() - run['startedAt'] < 7200, '完整运行超过120分钟。')
        eligible = [t for t in pending if all(d in run['done'] for d in t['dependsOn'])]
        require(bool(eligible), '任务依赖无法调度。')
        task = eligible[0]
        self._queue(db, state, 'develop' if task['owner'] == 'developer' else 'test', task['owner'], task['id'])

    def command(self, project_id, command_id, expected_revision, kind, payload):
        require(isinstance(command_id, str) and 1 <= len(command_id) <= 100, 'commandId无效。')
        require(isinstance(payload, dict), '命令载荷无效。')
        try:
            fingerprint = digest({'revision': expected_revision, 'kind': kind, 'payload': payload})
        except UnicodeError as error:
            raise Rejected('命令不是有效UTF-8。') from error
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            previous = db.execute('SELECT fingerprint,response FROM commands WHERE project=? AND id=?', (project_id, command_id)).fetchone()
            if previous:
                require(previous[0] == fingerprint, '同commandId不能用于不同载荷。')
                return json.loads(previous[1])
            state = self._get(db, project_id)
            require(type(expected_revision) is int and expected_revision == state['revision'], '版本冲突，请读取最新状态。')
            require(not state['leaseUnknown'] or kind in ('stop', 'agent_message', 'agent_resolve'), '旧进程状态未知，禁止派工。')
            busy = state['activeAttempt'] is not None or state['phase'] in ('paused', 'stopping')
            if kind in ('agent_message', 'agent_resolve'):
                self.guidance.refresh(db, state)
                if kind == 'agent_message':
                    self.guidance.save(db, state, payload)
                else:
                    self.guidance.resolve(db, state, payload)
                self._event(state, '角色消息已保存。' if kind == 'agent_message' else '角色消息处理决定已保存。')
            elif kind == 'agent_handoff':
                fields(payload, [])
                attempt = state['activeAttempt']
                require(attempt and attempt['status'] in ('queued', 'running') and attempt['kind'] != 'consult', '没有可交接的工作阶段。')
                state['agentControl'] = {'status': 'handoff_requested', 'attemptId': attempt['id'],
                    'role': attempt['role'], 'resumable': False, 'reason': '等待当前工具完成及公开交接，尚未停稳。'}
                self._event(state, '已请求安全交接；当前阶段尚未完成或停稳。')
            elif kind in ('agent_consult', 'agent_resume'):
                self._agent_command(db, state, kind, payload)
            elif kind == 'stop':
                fields(payload, [])
                state['epoch'] += 1
                state['pauseRequested'] = False
                db.execute('DELETE FROM agent_controls WHERE project=?', (state['id'],))
                if state.get('agentControl'):
                    state['agentControl'].update(status='cancelled', resumable=False)
                for task in state['tasks']:
                    task['approval'], task['status'] = None, 'pending'
                    self._history(task, 'revoked', '终止撤销批准。')
                if state['activeAttempt']:
                    attempt = state['activeAttempt']
                    db.execute('UPDATE attempts SET status=? WHERE id=?', ('cancelled', attempt['id']))
                    if attempt['status'] == 'queued' and not state['leaseUnknown']:
                        state.update(activeAttempt=None, phase='stopped')
                    else:
                        state['phase'], state['leaseUnknown'] = 'stopping', True
                else:
                    state['phase'] = 'blocked' if state['leaseUnknown'] else 'stopped'
                self._event(state, '请求终止：撤销批准，正在确认进程树停止。')
            elif kind == 'pause':
                fields(payload, [])
                require(state['run'] is not None and state['phase'] in ('developing', 'testing', 'fixing'), '当前不在执行。')
                state['pauseRequested'] = True
                self._event(state, '已请求暂停调度：当前阶段仍运行，结束后不派下一项。')
            elif kind == 'resume':
                fields(payload, [])
                require(state['phase'] == 'paused' and approved(state) and not state.get('agentControl', {}).get('resumable'), '受控暂停须使用明确恢复并重新核对上下文。')
                state['pauseRequested'] = False
                self._next(db, state)
            else:
                require(not busy, '正在运行，请先终止并确认进程退出。')
                if kind == 'retry':
                    fields(payload, [])
                    previous_attempt = state['lastAttempt']
                    require(state['phase'] == 'blocked' and not state['recoveryRequired'] and previous_attempt, '没有可安全重试的失败阶段。')
                    row = db.execute('SELECT status FROM attempts WHERE id=?', (previous_attempt['id'],)).fetchone()
                    require(row and row[0] == 'failed' and previous_attempt['epoch'] == state['epoch'], '失败阶段已失效。')
                    require(previous_attempt.get('retryCount', 0) < 2, '已达该阶段两次显式重试上限。')
                    kind_before = previous_attempt['kind']
                    if kind_before in ('develop', 'test', 'repair'):
                        require(state['run'] and state['run']['id'] == previous_attempt['runId'] and approved(state), '运行或批准已变化。')
                        require(time.time() - state['run']['startedAt'] < 7200, '完整运行超过120分钟。')
                    context = {'requirement': state['requirement'], 'roles': state['roles'],
                        'document': state['uploadedDocument'], 'tasks': state['tasks'],
                        'target': next((t for t in state['tasks'] if t['id'] == previous_attempt['taskId']), None),
                        'lastEvidence': state['run']['evidence'][-1] if state['run'] and state['run']['evidence'] else None}
                    require(digest(context) == previous_attempt['inputHash'], '失败后输入已变化，不能重用原批准重试。')
                    self._queue(db, state, kind_before, previous_attempt['role'], previous_attempt['taskId'], previous_attempt.get('retryCount', 0) + 1)
                elif kind == 'edit':
                    require(bool(payload) and set(payload) <= {'requirement', 'roles'}, '编辑字段无效。')
                    if 'requirement' in payload:
                        text(payload['requirement'], 4000, '需求')
                        state['requirement'] = payload['requirement']
                    if 'roles' in payload:
                        fields(payload['roles'], ['architect', 'developer', 'tester'])
                        require(set(payload['roles'].values()) == {'Claude Code'}, '真实模式目前仅支持三个独立 Claude Code 岗位；其他客户端尚未接入。')
                        state['roles'] = payload['roles']
                    state['contextRevision'] += 1
                    state.update(generatedDocument=None, uploadedDocument=None, tasks=[], run=None, phase='draft')
                elif kind == 'upload':
                    fields(payload, ['name', 'text'])
                    require(isinstance(payload['name'], str) and len(payload['name']) <= 180 and re.fullmatch(r'[^/\\\x00]+\.(md|txt)', payload['name'], re.I), '请选择.md/.txt文档。')
                    source = text(payload['text'], 262144, '上传文档')
                    require(len(source.encode('utf-8')) <= 256 * 1024, '文档超过256KiB。')
                    source = source.removeprefix('﻿')
                    require(bool(source.strip()), '文档不能为空。')
                    state['documentCounter'] += 1
                    state['uploadedDocument'] = {'version': state['documentCounter'], 'contextRevision': state['contextRevision'],
                        'name': payload['name'], 'text': source, 'sha256': hashlib.sha256(source.encode('utf-8')).hexdigest()}
                    state.update(tasks=[], run=None, phase='uploaded')
                elif kind == 'generate':
                    fields(payload, [])
                    state.update(generatedDocument=None, uploadedDocument=None, tasks=[], run=None)
                    self._queue(db, state, 'generate', 'architect')
                elif kind == 'split':
                    require(set(payload) <= {'confirmed'}, '拆分载荷无效。')
                    require(bool(state['uploadedDocument']), '请先上传人工确认后的设计。')
                    require(not state['tasks'] or payload.get('confirmed') is True, '重拆需要确认撤销原批准。')
                    self._queue(db, state, 'split', 'architect')
                elif kind in ('approve', 'reject', 'regenerate'):
                    expected = ['taskId', 'taskVersion', 'documentVersion'] + (['reason'] if kind == 'reject' else [])
                    fields(payload, expected)
                    require(state['phase'] in ('review', 'approved'), '当前不能审核。')
                    task = next((t for t in state['tasks'] if t['id'] == payload['taskId']), None)
                    require(task and task['version'] == payload['taskVersion'] and task['documentVersion'] == payload['documentVersion'], '任务或文档版本已变化。')
                    if kind == 'approve':
                        require(task['status'] in ('pending', 'needs-review'), '驳回项须重做后再审。')
                        by_id = {t['id']: t for t in state['tasks']}
                        require(all(d in by_id and by_id[d]['status'] not in ('rejected', 'needs-review') for d in task['dependsOn']), '上游需要重审。')
                        task['approval'] = {'documentVersion': task['documentVersion'], 'documentHash': state['uploadedDocument']['sha256'],
                            'contextRevision': state['contextRevision'], 'taskVersion': task['version'],
                            'dependencies': {d: by_id[d]['version'] for d in task['dependsOn']}}
                        task['status'], task['reviewReason'] = 'approved', ''
                        self._history(task, 'approved', '人工通过当前文档、任务和依赖版本。')
                    elif kind == 'reject':
                        require(task['status'] != 'rejected', '任务已经驳回。')
                        reason = text(payload['reason'], 2000, '驳回原因').strip()
                        task.update(status='rejected', approval=None, rejectionReason=reason)
                        self._history(task, 'rejected', reason)
                        self._invalidate(state, task['id'])
                    else:
                        require(task['status'] == 'rejected', '只能重做被驳回项。')
                        self._queue(db, state, 'regenerate', 'architect', task['id'])
                    if kind != 'regenerate':
                        state['phase'] = 'approved' if approved(state) else 'review'
                elif kind == 'start':
                    fields(payload, [])
                    require(state['canStart'] and approved(state), '全部当前任务通过后才能开始。')
                    state['run'] = {'id': uuid.uuid4().hex, 'startedAt': time.time(), 'done': [], 'evidence': [], 'repairRounds': 0}
                    state['pauseRequested'] = False
                    self._next(db, state)
                elif kind == 'retest':
                    fields(payload, [])
                    require(state['phase'] == 'ready' and approved(state), '仅待人工验收且批准有效时可重新测试。')
                    state['run']['done'] = [t['id'] for t in state['tasks'] if t['owner'] == 'developer']
                    self._next(db, state)
                elif kind == 'repair':
                    fields(payload, [])
                    require(state['phase'] == 'failed' and approved(state), '仅测试失败且批准有效时可返修。')
                    require(state['run']['repairRounds'] < 2, '已达两轮返修上限。')
                    state['run']['repairRounds'] += 1
                    state['run']['done'] = [t['id'] for t in state['tasks'] if t['owner'] == 'developer']
                    self._queue(db, state, 'repair', 'developer')
                elif kind == 'accept':
                    fields(payload, [])
                    require(state['phase'] == 'ready' and approved(state), '实际测试通过后才能人工验收。')
                    state['phase'] = 'accepted'
                else:
                    raise Rejected('未知或仅内部允许的操作。')
                state['lastError'] = ''
                self._event(state, f'人工操作：{kind}。')
            state['revision'] += 1
            response = self._save(db, state)
            db.execute('INSERT INTO commands VALUES(?,?,?,?)', (project_id, command_id, fingerprint, encoded(response)))
            return response

    def _suspended(self, db, state):
        from runtime import tree_manifest
        row = db.execute('SELECT data FROM agent_controls WHERE project=?', (state['id'],)).fetchone()
        require(row and state.get('agentControl', {}).get('resumable'), '没有已确认停稳的原工作上下文。')
        context = json.loads(row[0])
        old = context['attempt']
        require(context['binding'] == self.guidance.binding(state) and old['epoch'] == state['epoch'], '原工作上下文已变化。')
        if old['kind'] in ('develop', 'test', 'repair'):
            require(approved(state) and state['run'] and state['run']['id'] == old['runId'], '运行或批准已变化。')
            require(time.time() - state['run']['startedAt'] < 7200, '完整运行超过120分钟。')
        workspace = self.root / 'projects' / state['id']
        require(context['productManifest'] == tree_manifest(workspace / 'product') and
            context['testManifest'] == tree_manifest(workspace / 'tests'), '暂停后的产品或测试快照已变化。')
        freeze = self.root / 'frozen-tests' / (old['runId'] or old['id'])
        require(not freeze.is_file() or freeze.read_text(encoding='utf-8') == context['testManifest']['sha256'], '冻结测试规格已变化。')
        return context

    def _agent_command(self, db, state, kind, payload):
        from runtime import tree_manifest
        fields(payload, ['role', 'messageIds'] if kind == 'agent_consult' else ['messageIds'])
        require(not state['activeAttempt'] and not state['leaseUnknown'] and not state['recoveryRequired'] and state['phase'] != 'stopping', '必须先确认所有执行已停稳。')
        for row in db.execute('SELECT state FROM projects'):
            other = json.loads(row[0])
            require(not other['activeAttempt'] and not other['leaseUnknown'] and not other['recoveryRequired'], '其他项目仍有排队、运行或未知进程，禁止交叉启动。')
        suspended = db.execute('SELECT data FROM agent_controls WHERE project=?', (state['id'],)).fetchone()
        context = self._suspended(db, state) if suspended else None
        if kind == 'agent_consult':
            role = payload['role']
            require(role in ('architect', 'developer', 'tester'), '咨询岗位无效。')
            selected = self.guidance.select(db, state, role, payload['messageIds'])
            require(bool(selected), '请明确选择要咨询的问题。')
            workspace = self.root / 'projects' / state['id']
            consultation = {'messages': selected, 'returnPhase': 'paused' if context else state['phase'],
                'binding': self.guidance.binding(state), 'productManifest': tree_manifest(workspace / 'product'),
                'testManifest': tree_manifest(workspace / 'tests')}
            self._queue(db, state, 'consult', role, extra={'consultation': consultation})
            for item in selected:
                db.execute('UPDATE agent_messages SET target_attempt=?,nonce=NULL,delivery_attempt=NULL WHERE id=?', (state['activeAttempt']['id'], item['id']))
            self._event(state, '只读咨询已明确排队；不会完成或自动恢复原任务。')
        else:
            require(context is not None, '没有可恢复的原工作。')
            old = context['attempt']
            selected = self.guidance.select(db, state, old['role'], payload['messageIds'])
            conflict = db.execute("SELECT 1 FROM agent_messages WHERE project=? AND binding=? AND status='needs_confirmation' LIMIT 1",
                (state['id'], self.guidance.binding(state))).fetchone()
            require(not conflict, '范围冲突仍待明确处理，不能恢复原批准工作。')
            self._queue(db, state, old['kind'], old['role'], old['taskId'], old.get('retryCount', 0),
                extra={'guidance': selected, 'continuation': {'attemptId': old['id'], 'terminal': context['terminal']}})
            fresh = state['activeAttempt']
            db.execute("UPDATE agent_messages SET status='expired',nonce=NULL,updated_at=? WHERE project=? AND role=? AND status IN ('queued','received','needs_confirmation')",
                (time.time(), state['id'], old['role']))
            for item in selected:
                db.execute("UPDATE agent_messages SET status='queued',target_attempt=?,nonce=NULL,delivery_attempt=NULL,updated_at=? WHERE id=?",
                    (fresh['id'], time.time(), item['id']))
            db.execute('DELETE FROM agent_controls WHERE project=?', (state['id'],))
            state['agentControl'].update(status='resumed', resumable=False, resumedAttemptId=fresh['id'])
            state['pauseRequested'] = False
            state['lastError'] = ''
            self._event(state, '已明确恢复为新阶段；原阶段没有记作完成。', attemptId=fresh['id'])

    def agent_thread(self, project_id, role, after=0, limit=50):
        with self.lock, self._connect() as db:
            self._get(db, project_id)
            return self.guidance.thread(db, project_id, role, after, limit)

    def receive_guidance(self, attempt_id):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            state, attempt = self.guidance.live(db, attempt_id, self._get)
            return self.guidance.receive(db, state, attempt)

    def acknowledge_guidance(self, attempt_id, message_id, nonce, status, summary):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            state, attempt = self.guidance.live(db, attempt_id, self._get)
            receipt = self.guidance.acknowledge(db, state, attempt, message_id, nonce, status, summary)
            state['revision'] += 1
            self._save(db, state)
            return receipt

    def guidance_control(self, attempt_id):
        with self.lock, self._connect() as db:
            state, attempt = self.guidance.live(db, attempt_id, self._get)
            return self.guidance.control(db, state, attempt)

    def claim_attempt(self, project_id):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            state = self._get(db, project_id)
            attempt = state['activeAttempt']
            require(attempt and attempt['status'] == 'queued' and not state['leaseUnknown'], '没有可认领的阶段。')
            attempt['status'] = 'running'
            db.execute('UPDATE attempts SET status=? WHERE id=?', ('running', attempt['id']))
            state['revision'] += 1
            self._save(db, state)
            row = db.execute('SELECT data FROM attempts WHERE id=?', (attempt['id'],)).fetchone()
            return {**json.loads(row[0]), 'status': 'running'}

    @staticmethod
    def _process_proof(attempt_id, proof, message_key):
        require(isinstance(proof, dict) and proof.get('attemptId') == attempt_id and
            type(proof.get('pid')) is int and proof['pid'] > 0 and
            type(proof.get('exitCode')) is int and proof['exitCode'] == 0 and
            all(proof.get(key) is True for key in ('treeStopped', 'handlersDrained', 'checksStopped')) and
            all(isinstance(proof.get(key), str) and bool(proof[key]) for key in
                ('taskMessageId', 'ackMessageId', message_key)), '缺少当前阶段进程树、处理器与检查器停稳证明。')

    def _suspend_attempt(self, db, state, attempt, terminal, proof):
        stored = json.loads(db.execute('SELECT data FROM attempts WHERE id=?', (attempt['id'],)).fetchone()[0])
        context = {'attempt': {**stored, 'status': attempt['status']}, 'phase': state['phase'],
            'binding': self.guidance.binding(state), 'terminal': terminal,
            'productManifest': proof.get('productManifest'), 'testManifest': proof.get('testManifest')}
        db.execute('INSERT OR REPLACE INTO agent_controls VALUES(?,?,?)',
            (state['id'], attempt['id'], encoded(context)))
        state['agentControl'] = {'status': terminal['kind'], 'attemptId': attempt['id'],
            'role': attempt['role'], 'kind': attempt['kind'], 'taskId': attempt['taskId'],
            'reason': terminal['reason'], 'resumable': True}
        state['lastAttempt'] = copy.deepcopy(attempt)
        state.update(activeAttempt=None, phase='paused' if terminal['kind'] == 'handoff' else 'blocked',
            leaseUnknown=False, lastError=terminal['reason'])
        db.execute('UPDATE attempts SET status=? WHERE id=?', ('controlled', attempt['id']))
        self._event(state, '阶段已停稳，等待说明与明确恢复；没有记作任务完成。', attemptId=attempt['id'])
        state['revision'] += 1
        return self._save(db, state)

    def settle_controlled_attempt(self, attempt_id, terminal, process_proof):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            state, attempt = self.guidance.live(db, attempt_id, self._get)
            fields(terminal, ['kind', 'reason'])
            require(terminal['kind'] in ('blocked', 'handoff') and attempt['kind'] != 'consult', '非正常终态类型无效。')
            if terminal['kind'] == 'handoff':
                require(self.guidance.control(db, state, attempt)['handoffRequested'], '没有此阶段的明确交接请求。')
            self.guidance.bounded_text(terminal['reason'], 2000, '公开阻塞原因')
            self._process_proof(attempt_id, process_proof, 'terminalMessageId')
            return self._suspend_attempt(db, state, attempt, terminal, process_proof)

    def complete_consultation(self, attempt_id, reply, process_proof):
        from runtime import tree_manifest
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            state, attempt = self.guidance.live(db, attempt_id, self._get)
            require(attempt['kind'] == 'consult', '非咨询阶段不能提交咨询回复。')
            fields(reply, ['summary'])
            summary = self.guidance.bounded_text(reply['summary'], 2000, '咨询回复')
            self._process_proof(attempt_id, process_proof, 'consultationMessageId')
            stored = json.loads(db.execute('SELECT data FROM attempts WHERE id=?', (attempt_id,)).fetchone()[0])
            context = stored['input']['consultation']
            workspace = self.root / 'projects' / state['id']
            require(context['binding'] == self.guidance.binding(state) and
                context['productManifest'] == tree_manifest(workspace / 'product') and
                context['testManifest'] == tree_manifest(workspace / 'tests'), '只读咨询期间上下文或快照变化。')
            for item in context['messages']:
                rows = self.guidance.rows(db, 'WHERE id=? AND project=?', (item['id'], state['id']))
                require(rows and rows[0]['status'] in ('queued', 'received') and rows[0]['target_attempt'] == attempt_id and
                    rows[0]['text'] == item['text'] and rows[0]['intent'] == 'clarification', '咨询问题已撤回或变化，不能将旧回复记作已回答。')
                db.execute("UPDATE agent_messages SET status='answered',summary=?,nonce=NULL,updated_at=? WHERE id=? AND target_attempt=? AND status IN ('queued','received')",
                    (summary, time.time(), item['id'], attempt_id))
            db.execute('INSERT INTO agent_messages(id,project,role,binding,target_attempt,status,intent,text,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)',
                (uuid.uuid4().hex, state['id'], attempt['role'], self.guidance.binding(state), attempt_id, 'answered', 'reply', summary, time.time(), time.time()))
            db.execute('UPDATE attempts SET status=? WHERE id=?', ('consulted', attempt_id))
            state.update(activeAttempt=None, phase=context['returnPhase'], lastAttempt=copy.deepcopy(attempt))
            self._event(state, '只读咨询已结束；原工作没有完成，也没有自动恢复。', attemptId=attempt_id)
            state['revision'] += 1
            return self._save(db, state)

    def complete_attempt(self, attempt_id, result, evidence=None, process_proof=None):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT project,epoch,status,data FROM attempts WHERE id=?', (attempt_id,)).fetchone()
            require(row and row[2] == 'running', '阶段未运行、已取消或已完成。')
            state = self._get(db, row[0])
            attempt = state['activeAttempt']
            require(attempt and attempt['id'] == attempt_id and row[1] == state['epoch'] and not state['leaseUnknown'], '迟到阶段结果。')
            require(attempt['kind'] != 'consult', '咨询不能提交正常任务结果。')
            if process_proof is not None:
                self._process_proof(attempt_id, process_proof, 'resultMessageId')
            control = self.guidance.control(db, state, attempt)
            if control['pending'] or control['handoffRequested']:
                self._process_proof(attempt_id, process_proof, 'resultMessageId')
                terminal = {'kind': 'handoff' if control['handoffRequested'] else 'blocked',
                    'reason': '阶段结束前收到交接请求或未处理说明；结果未提交，等待明确恢复。'}
                return self._suspend_attempt(db, state, attempt, terminal, process_proof)
            kind = attempt['kind']
            if kind == 'generate':
                fields(result, ['markdown', 'criteria'])
                text(result['markdown'], 200000, '设计文档')
                require(isinstance(result['criteria'], list) and 1 <= len(result['criteria']) <= 40, '设计缺验收点。')
                for point in result['criteria']:
                    text(point, 2000, '设计验收点')
                state['generationCounter'] += 1
                state['generatedDocument'] = {**result, 'version': state['generationCounter'], 'contextRevision': state['contextRevision']}
                state['phase'] = 'document'
            elif kind == 'split':
                fields(result, ['tasks'])
                tasks = validate_tasks(result['tasks'])
                state['taskCounter'] += 1
                state['tasks'] = [{**t, 'version': state['taskCounter'], 'documentVersion': state['uploadedDocument']['version'],
                    'status': 'pending', 'approval': None, 'rejectionReason': '', 'reviewReason': '',
                    'history': [{'kind': 'created', 'taskVersion': state['taskCounter'], 'documentVersion': state['uploadedDocument']['version'], 'message': '真实模型拆分，等待人工逐项审核。'}]} for t in tasks]
                state['phase'] = 'review'
            elif kind == 'regenerate':
                fields(result, ['task'])
                replacement = validate_task(result['task'])
                target = next(t for t in state['tasks'] if t['id'] == attempt['taskId'])
                require(replacement['id'] == target['id'] and replacement['owner'] == target['owner'], '局部重做不能改ID或岗位。')
                combined = [{k: (replacement if t['id'] == target['id'] else t)[k] for k in TASK_FIELDS} for t in state['tasks']]
                validate_tasks(combined)
                state['taskCounter'] += 1
                target.update(replacement, version=state['taskCounter'], status='pending', approval=None, reviewReason='局部重做的新版本，必须人工重审。')
                self._history(target, 'regenerated', '根据驳回原因真实重做，等待重审。')
                self._invalidate(state, target['id'])
                state['phase'] = 'review'
            elif kind in ('develop', 'repair'):
                fields(result, ['summary', 'artifacts'])
                text(result['summary'], 8000, '执行摘要')
                require(isinstance(result['artifacts'], list) and all(isinstance(a, str) and len(a) <= 240 for a in result['artifacts']), '产物清单无效。')
            elif kind == 'test':
                fields(result, ['summary', 'evidenceId'])
                text(result['summary'], 8000, '测试摘要')
                require(evidence and evidence['id'] == result['evidenceId'] and type(evidence.get('exitCode')) is int
                    and evidence.get('assertions', 0) > 0 and evidence.get('productHash') and evidence.get('specHash'), '缺少执行器签发的实际测试证据。')
                state['run']['evidence'].append(evidence)
            state['lastAttempt'] = {k: attempt[k] for k in ('id', 'role', 'kind', 'taskId', 'inputHash')}
            state['activeAttempt'] = None
            db.execute('UPDATE attempts SET status=? WHERE id=?', ('complete', attempt_id))
            self._event(state, f'{attempt["role"]}：{kind} 实际阶段完成。', attemptId=attempt_id)
            if kind in ('develop', 'test', 'repair'):
                require(approved(state), '执行批准已经失效。')
                if kind == 'test' and evidence['exitCode'] != 0:
                    state['phase'] = 'failed'
                else:
                    if kind != 'repair':
                        state['run']['done'].append(attempt['taskId'])
                    self._next(db, state)
            state['revision'] += 1
            return self._save(db, state)

    def record_execution(self, attempt_id, evidence):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT project,status FROM attempts WHERE id=?', (attempt_id,)).fetchone()
            require(row and row[1] == 'running', '执行证据对应阶段已失效。')
            state = self._get(db, row[0])
            require(state['activeAttempt'] and state['activeAttempt']['id'] == attempt_id and state['activeAttempt']['epoch'] == state['epoch'], '执行证据版本已失效。')
            require(evidence.get('attemptId') == attempt_id and evidence.get('exitCode') == 0 and evidence.get('treeStopped') is True
                and all(evidence.get(k) for k in ('taskMessageId', 'ackMessageId', 'resultMessageId')), 'CLI/reason证据不完整。')
            summary = {k: copy.deepcopy(v) for k, v in evidence.items() if k != 'result'}
            state.setdefault('executions', []).append(summary)
            self._event(state, f"{state['activeAttempt']['role']}：真实TASK→ACK→RESULT，PID {evidence['pid']}，exit 0，进程树已清空。", channel='reason', attemptId=attempt_id,
                taskMessageId=evidence['taskMessageId'], ackMessageId=evidence['ackMessageId'], resultMessageId=evidence['resultMessageId'])
            state['revision'] += 1
            return self._save(db, state)

    def fail_attempt(self, attempt_id, message, confirmed=True):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT project,status FROM attempts WHERE id=?', (attempt_id,)).fetchone()
            require(row is not None, '阶段不存在。')
            state = self._get(db, row[0])
            require(state['activeAttempt'] and state['activeAttempt']['id'] == attempt_id, '阶段已失效。')
            state['lastAttempt'] = copy.deepcopy(state['activeAttempt'])
            state.update(activeAttempt=None, phase='blocked', leaseUnknown=not confirmed, lastError=str(message)[:4000])
            state['revision'] += 1
            db.execute('UPDATE attempts SET status=? WHERE id=?', ('failed', attempt_id))
            self._event(state, f'阶段阻塞：{state["lastError"]}')
            return self._save(db, state)

    def confirm_stopped(self, project_id, confirmed):
        with self.lock, self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            state = self._get(db, project_id)
            require(state['phase'] in ('stopping', 'blocked'), '未请求停止。')
            state.update(phase='stopped' if confirmed else 'blocked', leaseUnknown=not confirmed)
            if confirmed:
                state['activeAttempt'] = None
                state['recoveryRequired'] = False
            state['revision'] += 1
            self._event(state, '已确认本轮进程树停止。' if confirmed else '进程树状态未知，保持阻塞。')
            return self._save(db, state)


TASK_FIELDS = ('id', 'owner', 'title', 'detail', 'acceptance', 'dependsOn')
