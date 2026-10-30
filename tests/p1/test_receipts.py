"""Real SQLite receipt contracts; no model execution or synthetic completion."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import http.client
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import threading
import unittest
import uuid
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from state import Store, Rejected


class ReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-receipts-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.store = Store(self.root)

    def test_create_replay_is_one_project_and_changed_name_rejects(self):
        first = self.store.create_project('同名项目', command_id='create-1')
        self.assertEqual(self.store.create_project('同名项目', command_id='create-1'), first)
        with self.assertRaises(Rejected):
            self.store.create_project('另一名称', command_id='create-1')
        second = self.store.create_project('同名项目', command_id='create-2')
        legacy = self.store.create_project('旧格式')
        self.assertEqual(len({first['id'], second['id'], legacy['id']}), 3)
        self.assertEqual(len(self.store.list_projects()), 3)
        self.assertEqual(len(list((self.root / 'projects').iterdir())), 3)
        self.assertEqual(self.store.creation_receipt('create-1'), {
            'found': True, 'scope': 'create', 'commandId': 'create-1', 'projectId': first['id'],
            'acceptedRevision': 1, 'epoch': 1, 'attemptId': None})

    def test_concurrent_same_creation_id_uses_one_transaction_and_directory(self):
        other = Store(self.root, recover=False)
        gate = threading.Barrier(2)
        def create(store):
            gate.wait(timeout=3)
            return store.create_project('并发创建', command_id='one-creation')
        with ThreadPoolExecutor(max_workers=2) as pool:
            one, two = [future.result(timeout=8) for future in [pool.submit(create, self.store), pool.submit(create, other)]]
        self.assertEqual(one, two)
        self.assertEqual(len(self.store.list_projects()), 1)
        folders = list((self.root / 'projects').iterdir())
        self.assertEqual([folder.name for folder in folders], [one['id']])
        self.assertEqual({item.name for item in folders[0].iterdir()}, {'product', 'tests'})

    def test_invalid_ids_never_create_projects_or_receipts(self):
        for value in ('', 'x' * 101, 12, False, []):
            with self.subTest(value=repr(value)):
                with self.assertRaises(Rejected):
                    self.store.create_project('无效ID', command_id=value)
                with self.assertRaises(Rejected):
                    self.store.creation_receipt(value)
        self.assertEqual(self.store.list_projects(), [])
        self.assertFalse((self.root / 'projects').exists())

    def test_receipt_is_read_only_and_excludes_payload(self):
        project = self.store.create_project('私密项目', command_id='private-creation')
        saved = self.store.command(project['id'], 'save-private', 1, 'edit', {'requirement': 'private-payload-marker'})
        self.store.command(project['id'], 'queued-model', saved['revision'], 'generate', {})
        before = self.store.get(project['id'])
        with closing(sqlite3.connect(self.store.db)) as db, db:
            counts = [db.execute('SELECT COUNT(*) FROM ' + table).fetchone()[0]
                for table in ('projects', 'commands', 'attempts', 'project_creations')]
        self.assertEqual(self.store.command_receipt(project['id'], 'save-private'), {
            'found': True, 'scope': 'project', 'commandId': 'save-private', 'projectId': project['id'],
            'acceptedRevision': 2, 'epoch': 1, 'attemptId': None})
        accepted = self.store.command_receipt(project['id'], 'queued-model')
        self.assertEqual(accepted, {'found': True, 'scope': 'project', 'commandId': 'queued-model',
            'projectId': project['id'], 'acceptedRevision': 3, 'epoch': 1,
            'attemptId': before['activeAttempt']['id']})
        missing = self.store.command_receipt(project['id'], 'not-submitted')
        self.assertEqual(missing, {'found': False, 'scope': 'project', 'commandId': 'not-submitted', 'projectId': project['id']})
        self.assertEqual(self.store.creation_receipt('unknown-create'), {
            'found': False, 'scope': 'create', 'commandId': 'unknown-create', 'projectId': None})
        serialized = json.dumps([accepted, missing, self.store.creation_receipt('private-creation')])
        for private in ('private-payload-marker', '私密项目', 'fingerprint', 'response', str(self.root)):
            self.assertNotIn(private, serialized)
        self.assertEqual(self.store.get(project['id']), before)
        self.assertEqual(before['activeAttempt']['status'], 'queued')
        with closing(sqlite3.connect(self.store.db)) as db, db:
            self.assertEqual([db.execute('SELECT COUNT(*) FROM ' + table).fetchone()[0]
                for table in ('projects', 'commands', 'attempts', 'project_creations')], counts)

    def test_project_scope_cannot_find_another_projects_command(self):
        one = self.store.create_project('一')
        two = self.store.create_project('二')
        self.store.command(one['id'], 'one-command', 1, 'edit', {'requirement': '只有一可见'})
        self.assertFalse(self.store.command_receipt(two['id'], 'one-command')['found'])
        with self.assertRaises(Rejected):
            self.store.command_receipt('0' * 32, 'one-command')
        for value in ('', 'x' * 101, None):
            with self.subTest(value=value), self.assertRaises(Rejected):
                self.store.command_receipt(one['id'], value)

    def test_database_failure_rolls_back_receipt_project_and_only_owned_empty_folders(self):
        unknown = self.root / 'projects' / ('f' * 32)
        unknown.mkdir(parents=True)
        (unknown / 'keep').write_bytes(b'unknown directory must survive')
        for table in ('projects', 'project_creations'):
            with self.subTest(table=table):
                with closing(sqlite3.connect(self.store.db)) as db, db:
                    db.execute('CREATE TRIGGER fail_insert BEFORE INSERT ON ' + table + " BEGIN SELECT RAISE(ABORT, 'injected disk write failure'); END")
                try:
                    with self.assertRaises(sqlite3.DatabaseError):
                        self.store.create_project('写入失败', command_id='failed-' + table)
                finally:
                    with closing(sqlite3.connect(self.store.db)) as db, db:
                        db.execute('DROP TRIGGER fail_insert')
                self.assertEqual(self.store.list_projects(), [])
                self.assertFalse(self.store.creation_receipt('failed-' + table)['found'])
                self.assertEqual(list((self.root / 'projects').iterdir()), [unknown])
                self.assertEqual((unknown / 'keep').read_bytes(), b'unknown directory must survive')

    def test_existing_unknown_project_directory_is_never_reused_or_deleted(self):
        identity = uuid.UUID('11111111-1111-1111-1111-111111111111')
        unknown = self.root / 'projects' / identity.hex
        unknown.mkdir(parents=True)
        (unknown / 'keep').write_bytes(b'not ours')
        with patch('state.uuid.uuid4', return_value=identity):
            with self.assertRaises(FileExistsError):
                self.store.create_project('不能覆盖', command_id='collision')
        self.assertEqual((unknown / 'keep').read_bytes(), b'not ours')
        self.assertEqual(self.store.list_projects(), [])
        self.assertFalse(self.store.creation_receipt('collision')['found'])

    def test_creation_replay_returns_original_response_after_project_changes(self):
        first = self.store.create_project('初始响应', command_id='original')
        saved = self.store.command(first['id'], 'edit-later', 1, 'edit', {'requirement': '较新内容'})
        self.assertEqual(self.store.create_project('初始响应', command_id='original'), first)
        self.assertEqual(self.store.get(first['id']), saved)
        self.assertEqual(self.store.creation_receipt('original')['acceptedRevision'], 1)

    def test_old_database_upgrade_preserves_existing_records_and_evidence(self):
        root = self.root / 'legacy'
        root.mkdir()
        identity = 'a' * 32
        state = json.dumps({'id': identity, 'phase': 'draft', 'activeAttempt': None, 'revision': 4,
            'epoch': 2, 'requirement': 'old private content'}, ensure_ascii=False)
        response = json.dumps({'revision': 3, 'epoch': 2, 'activeAttempt': None})
        old_attempt = json.dumps({'id': 'old-attempt', 'private': 'old bytes'})
        with closing(sqlite3.connect(root / 'workbench.sqlite3')) as db, db:
            db.executescript('CREATE TABLE projects(id TEXT PRIMARY KEY, state TEXT NOT NULL);'
                'CREATE TABLE commands(project TEXT,id TEXT,fingerprint TEXT,response TEXT,PRIMARY KEY(project,id));'
                'CREATE TABLE attempts(id TEXT PRIMARY KEY,project TEXT,epoch INTEGER,status TEXT,data TEXT);')
            db.execute('INSERT INTO projects VALUES(?,?)', (identity, state))
            db.execute('INSERT INTO commands VALUES(?,?,?,?)', (identity, 'old-save', 'old-fingerprint', response))
            db.execute('INSERT INTO attempts VALUES(?,?,?,?,?)', ('old-attempt', identity, 1, 'complete', old_attempt))
            before = {table: db.execute('SELECT * FROM ' + table).fetchall() for table in ('projects', 'commands', 'attempts')}
        evidence = root / 'evidence' / 'old.bin'
        evidence.parent.mkdir()
        evidence.write_bytes(b'\x00old immutable evidence\xff')
        store = Store(root)
        self.assertEqual(store.command_receipt(identity, 'old-save'), {
            'found': True, 'scope': 'project', 'commandId': 'old-save', 'projectId': identity,
            'acceptedRevision': 3, 'epoch': 2, 'attemptId': None})
        self.assertFalse(store.creation_receipt('old-save')['found'])
        with closing(sqlite3.connect(store.db)) as db, db:
            for table, rows in before.items():
                self.assertEqual(db.execute('SELECT * FROM ' + table).fetchall(), rows)
        self.assertEqual(evidence.read_bytes(), b'\x00old immutable evidence\xff')


class ReceiptHTTPTests(unittest.TestCase):
    def setUp(self):
        from server import Application
        self.temp = tempfile.TemporaryDirectory(prefix='relay-receipt-http-')
        self.addCleanup(self.temp.cleanup)
        self.app = Application(Path(self.temp.name), port=0, workers=False)
        from support import ReadySetupFixture
        self.app.setup = ReadySetupFixture()
        self.thread = threading.Thread(target=self.app.serve, daemon=True)
        self.thread.start()
        self.addCleanup(self.app.close)
        self.token = self.call('POST', '/api/session', {'bootstrap': self.app.bootstrap}, auth=False)[1]['token']
        self.project = self.call('POST', '/api/projects', {'name': '回执项目'})[1]

    def call(self, method, path, value=None, auth=True, headers=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.app.port, timeout=3)
        request_headers = {'Origin': self.app.origin, 'Content-Type': 'application/json'}
        if auth:
            request_headers['Authorization'] = 'Bearer ' + self.token
        request_headers.update(headers or {})
        try:
            connection.request(method, path, json.dumps(value).encode() if value is not None else None, request_headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def test_receipt_requires_auth_exact_origin_and_scope(self):
        project_id = self.project['id']
        path = '/api/receipts?scope=project&project=' + project_id + '&command=not-sent'
        self.assertEqual(self.call('GET', path, auth=False)[0], 403)
        self.assertEqual(self.call('GET', path, headers={'Origin': 'https://bad.invalid'})[0], 403)
        self.assertEqual(self.call('GET', path, headers={'Host': 'localhost:' + str(self.app.port)})[0], 403)
        self.assertEqual(self.call('GET', path, headers={'Sec-Fetch-Site': 'cross-site'})[0], 403)
        self.assertEqual(self.call('GET', path), (200, {'found': False, 'scope': 'project',
            'commandId': 'not-sent', 'projectId': project_id}))
        for query in ('scope=project&command=x', 'scope=create&command=x&project=' + project_id,
                'scope=other&command=x', 'scope=create&command=x&command=y',
                'scope=create&command=', 'scope=create&command=x&extra=y',
                'scope=create&command=' + 'x' * 101, 'scope=create&command=' + 'x' * 1025,
                'scope=create&command=x&scope=create', 'scope=create&command=x&extra'):
            with self.subTest(query=query[:100]):
                self.assertEqual(self.call('GET', '/api/receipts?' + query)[0], 400)
        self.assertEqual(self.call('GET', path.replace(project_id, '0' * 32))[0], 409)
        self.assertEqual(self.app.store.get(project_id), self.project)

    def test_idempotent_http_creation_keeps_legacy_shape_and_private_receipt(self):
        body = {'name': 'private-name-http', 'commandId': 'create-http'}
        status, project = self.call('POST', '/api/projects', body)
        self.assertEqual(status, 200)
        self.assertEqual(self.call('POST', '/api/projects', body), (200, project))
        status, rejected = self.call('POST', '/api/projects', {**body, 'name': 'changed'})
        self.assertEqual(status, 409)
        self.assertEqual(rejected['outcome'], 'rejected')
        self.assertEqual(len(self.app.store.list_projects()), 2)
        status, receipt = self.call('GET', '/api/receipts?scope=create&command=create-http')
        self.assertEqual(status, 200)
        self.assertEqual(receipt, {'found': True, 'scope': 'create', 'commandId': 'create-http',
            'projectId': project['id'], 'acceptedRevision': 1, 'epoch': 1, 'attemptId': None})
        self.assertNotIn('private-name-http', json.dumps(receipt))
        self.assertEqual(self.call('POST', '/api/projects', {'name': 'legacy'})[0], 200)
        self.assertEqual(self.call('POST', '/api/projects', {**body, 'extra': True})[0], 409)
        self.assertEqual(self.call('POST', '/api/projects', {**body, 'commandId': None})[0], 409)

    def test_receipt_missing_while_post_waits_is_not_rejection(self):
        entered = threading.Event()
        owner = threading.get_ident()
        lock = self.app.action_lock
        class ObservedLock:
            def __enter__(self):
                if threading.get_ident() != owner:
                    entered.set()
                return lock.__enter__()
            def __exit__(self, *args):
                return lock.__exit__(*args)
        self.app.action_lock = ObservedLock()
        body = {'projectId': self.project['id'], 'commandId': 'waiting-save', 'expectedRevision': 1,
            'kind': 'edit', 'payload': {'requirement': 'wait-private-content'}}
        with ThreadPoolExecutor(max_workers=1) as pool:
            with lock:
                pending = pool.submit(self.call, 'POST', '/api/command', body)
                self.assertTrue(entered.wait(1), 'POST must reach the held action lock')
                status, receipt = self.call('GET', '/api/receipts?scope=project&project=' + self.project['id'] + '&command=waiting-save')
                self.assertEqual(status, 200, 'Read-only receipt cannot wait on the POST action lock')
                self.assertEqual(receipt['found'], False)
                self.assertNotIn('outcome', receipt, 'No receipt is not proof of rejection')
                self.assertEqual(self.app.store.get(self.project['id'])['revision'], 1)
            self.assertEqual(pending.result(timeout=4)[0], 200)
        receipt = self.call('GET', '/api/receipts?scope=project&project=' + self.project['id'] + '&command=waiting-save')[1]
        self.assertEqual(receipt['acceptedRevision'], 2)
        self.assertNotIn('wait-private-content', json.dumps(receipt))
        with closing(sqlite3.connect(self.app.store.db)) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM commands').fetchone()[0], 1)
            self.assertEqual(db.execute('SELECT COUNT(*) FROM attempts').fetchone()[0], 0)

    def test_committed_stop_error_stays_unknown_with_receipt(self):
        from types import SimpleNamespace
        for error in (OSError('private-host-path'), ValueError('private-host-path'), Rejected('private-host-path')):
            with self.subTest(error=type(error).__name__):
                project = self.app.store.create_project('停止异常')
                queued = self.app.store.command(project['id'], 'queue-fixture', 1, 'generate', {})
                self.app.store.claim_attempt(project['id'])
                before = self.app.store.get(project['id'])
                def fail_stop(project_id):
                    raise error
                self.app.engine = SimpleNamespace(stop=fail_stop)
                try:
                    status, response = self.call('POST', '/api/command', {'projectId': project['id'],
                        'commandId': 'stop-error', 'expectedRevision': before['revision'], 'kind': 'stop', 'payload': {}})
                finally:
                    self.app.engine = None
                self.assertEqual(status, 500, 'Post-commit errors are never definitive rejections')
                self.assertEqual(response['outcome'], 'unknown')
                self.assertNotIn('private-host-path', json.dumps(response))
                current = self.app.store.get(project['id'])
                self.assertEqual(current['phase'], 'stopping')
                self.assertTrue(current['leaseUnknown'])
                receipt = self.call('GET', '/api/receipts?scope=project&project=' + project['id'] + '&command=stop-error')[1]
                self.assertEqual(receipt['acceptedRevision'], current['revision'])
                self.assertEqual(receipt['epoch'], current['epoch'])
                self.assertEqual(receipt['attemptId'], queued['activeAttempt']['id'])

    def test_definitive_conflict_has_no_receipt_and_keeps_authoritative_state(self):
        body = {'projectId': self.project['id'], 'commandId': 'bad-revision', 'expectedRevision': 99,
            'kind': 'edit', 'payload': {'requirement': 'should not commit'}}
        status, response = self.call('POST', '/api/command', body)
        self.assertEqual(status, 409)
        self.assertEqual(response['outcome'], 'rejected')
        self.assertEqual(self.app.store.get(self.project['id']), self.project)
        receipt = self.call('GET', '/api/receipts?scope=project&project=' + self.project['id'] + '&command=bad-revision')[1]
        self.assertFalse(receipt['found'])
        self.assertEqual(self.call('POST', '/api/command', body, auth=False)[1]['outcome'], 'rejected')


if __name__ == '__main__':
    unittest.main()
