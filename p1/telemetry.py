"""Bounded, non-authoritative public output. Never persist raw CLI records."""
from collections import OrderedDict
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import threading
import time

RECORD_LIMIT = 512 * 1024
TEXT_LIMIT = 2 * 1024
TAIL_EVENTS = 512
TAIL_BYTES = 256 * 1024
PAGE_EVENTS = 200
PAGE_BYTES = 128 * 1024
DIAGNOSTICS = frozenset(('malformed_record', 'oversized_record', 'unknown_schema', 'stream_gap', 'observer_failed', 'snapshot_mismatch'))


def text_chunks(text):
    chunk, size = [], 0
    for char in text:
        count = len(char.encode('utf-8', errors='replace'))
        if size + count > TEXT_LIMIT:
            yield ''.join(chunk)
            chunk, size = [], 0
        chunk.append(char)
        size += count
    if chunk:
        yield ''.join(chunk)


class Redactor:
    """Hold possible secret prefixes, including incomplete credentials at EOF."""
    MARKERS = ('bearer ', 'authorization:', 'api_key=', 'api-key=', 'access_token=', 'token=', 'sk-')
    DELIMITERS = frozenset(' \t\r\n\"\'<>(),;{}[]')

    def __init__(self, secrets=()):
        self.secrets = tuple(sorted({s for s in secrets if isinstance(s, str) and 0 < len(s) <= 4096}, key=len, reverse=True))[:32]
        self.pending = ''
        self.credential = False
        self.header = False
        self.value_started = False

    def feed(self, text):
        output = []
        for char in text:
            if self.credential:
                if self.header and char not in '\r\n':
                    continue
                if not self.value_started and char in ' \t\"\'':
                    continue
                if char not in self.DELIMITERS:
                    self.value_started = True
                    continue
                self.credential = False
            self.pending += char
            while self.pending:
                secret = next((s for s in self.secrets if self.pending.startswith(s)), None)
                if secret:
                    output.append('[REDACTED]')
                    self.pending = self.pending[len(secret):]
                    continue
                lowered = self.pending.lower()
                marker = next((s for s in self.MARKERS if lowered.startswith(s)), None)
                if marker:
                    output.append('[REDACTED]')
                    self.pending = ''
                    self.credential = True
                    self.header = marker == 'authorization:'
                    self.value_started = False
                    break
                if any(s.startswith(self.pending) for s in self.secrets) or any(s.startswith(lowered) for s in self.MARKERS):
                    break
                output.append(self.pending[0])
                self.pending = self.pending[1:]
        return ''.join(output)

    def finish(self):
        tail = '[REDACTED]' if self.pending else ''
        self.pending = ''
        self.credential = False
        return tail


class ClaudeStreamParser:
    def __init__(self, secrets=()):
        self.buffer = bytearray()
        self.discarding = False
        self.disabled = False
        self.closed = False
        self.current = None
        self.messages = OrderedDict()
        self.evicted = False
        self.diagnostics = set()
        self.redactor = Redactor(secrets)

    def diagnostic(self, code):
        if code in self.diagnostics:
            return []
        self.diagnostics.add(code)
        return [{'kind': 'diagnostic', 'code': code}]

    def public_text(self, text):
        safe = self.redactor.feed(text)
        safe = ''.join(c for c in safe if c in '\n\t' or ord(c) >= 32)
        safe = safe.encode('utf-8', errors='replace').decode('utf-8')
        return [{'kind': 'text', 'text': part} for part in text_chunks(safe)]

    def message(self, identifier, create=True):
        if not isinstance(identifier, str) or not 0 < len(identifier) <= 256:
            return None
        if identifier not in self.messages and create:
            if len(self.messages) >= 64:
                self.messages.popitem(last=False)
                self.evicted = True
            self.messages[identifier] = {}
        return self.messages.get(identifier)

    def delta(self, blocks, index, text):
        if not isinstance(text, str) or type(index) is not int or not 0 <= index < 1024:
            return []
        state = blocks.setdefault(index, [0, hashlib.sha256()])
        state[0] += len(text)
        state[1].update(text.encode('utf-8', errors='replace'))
        return self.public_text(text)

    def project(self, value):
        if not isinstance(value, dict):
            return self.diagnostic('unknown_schema')
        kind = value.get('type')
        if kind == 'stream_event':
            event = value.get('event')
            if not isinstance(event, dict):
                return self.diagnostic('unknown_schema')
            event_type = event.get('type')
            if event_type == 'message_start':
                message = event.get('message')
                self.current = None
                if isinstance(message, dict) and message.get('role') == 'assistant':
                    identifier = message.get('id')
                    if self.message(identifier) is not None:
                        self.current = identifier
                return []
            if event_type == 'message_stop':
                self.current = None
                return []
            blocks = self.message(self.current, create=False)
            if blocks is None:
                return []
            index = event.get('index')
            if type(index) is not int or not 0 <= index < 1024:
                return []
            if event_type == 'content_block_start':
                block = event.get('content_block')
                if isinstance(block, dict) and block.get('type') == 'text':
                    blocks.setdefault(index, [0, hashlib.sha256()])
                    return self.delta(blocks, index, block.get('text', ''))
            if event_type == 'content_block_delta' and index in blocks:
                delta = event.get('delta')
                if isinstance(delta, dict) and delta.get('type') == 'text_delta':
                    return self.delta(blocks, index, delta.get('text'))
            return []
        if kind == 'assistant':
            message = value.get('message')
            if not isinstance(message, dict) or message.get('role') != 'assistant':
                return self.diagnostic('unknown_schema')
            identifier = message.get('id')
            if self.evicted and identifier not in self.messages:
                return []
            blocks = self.message(identifier)
            content = message.get('content')
            if blocks is None or not isinstance(content, list):
                return self.diagnostic('unknown_schema')
            output = []
            for index, block in enumerate(content[:1024]):
                if not isinstance(block, dict) or block.get('type') != 'text' or not isinstance(block.get('text'), str):
                    continue
                text = block['text']
                if index in blocks:
                    length, digest = blocks[index]
                    if len(text) < length or hashlib.sha256(text[:length].encode('utf-8', errors='replace')).digest() != digest.digest():
                        output.extend(self.diagnostic('snapshot_mismatch'))
                        continue
                    text = text[length:]
                output.extend(self.delta(blocks, index, text))
            return output
        if kind in ('user', 'system', 'result', 'rate_limit_event', 'tool_progress', 'tool_use_summary'):
            return []
        return self.diagnostic('unknown_schema')

    def record(self, raw):
        try:
            return self.project(json.loads(raw))
        except (ValueError, TypeError, RecursionError):
            return self.diagnostic('malformed_record')

    def feed(self, data):
        if self.disabled or self.closed:
            return []
        events, offset = [], 0
        while offset < len(data):
            end = data.find(b'\n', offset)
            stop = len(data) if end < 0 else end
            if not self.discarding:
                if len(self.buffer) + stop - offset > RECORD_LIMIT:
                    self.buffer.clear()
                    self.discarding = True
                    events.extend(self.diagnostic('oversized_record'))
                else:
                    self.buffer.extend(data[offset:stop])
            if end >= 0:
                if self.buffer and not self.discarding:
                    events.extend(self.record(bytes(self.buffer)))
                self.buffer.clear()
                self.discarding = False
            offset = stop + 1
        return events

    def gap(self):
        self.disabled = True
        self.buffer.clear()
        self.redactor.pending = ''
        return self.diagnostic('stream_gap')

    def finish(self):
        if self.closed:
            return []
        self.closed = True
        events = []
        if not self.disabled:
            if self.buffer and not self.discarding:
                events.extend(self.record(bytes(self.buffer)))
            events.extend({'kind': 'text', 'text': part} for part in text_chunks(self.redactor.finish()))
        self.buffer.clear()
        return events


def scope_key(scope):
    if not isinstance(scope, dict) or set(scope) != {'projectId', 'attemptId', 'epoch', 'runId', 'inputHash'}:
        raise ValueError('Invalid telemetry scope')
    for name in ('projectId', 'attemptId'):
        if not isinstance(scope[name], str) or not re.fullmatch('[a-f0-9]{32}', scope[name]):
            raise ValueError('Invalid telemetry scope')
    if type(scope['epoch']) is not int or scope['epoch'] < 1:
        raise ValueError('Invalid telemetry epoch')
    if scope['runId'] is not None and (not isinstance(scope['runId'], str) or not re.fullmatch('[a-f0-9]{32}', scope['runId'])):
        raise ValueError('Invalid telemetry run')
    if not isinstance(scope['inputHash'], str) or not re.fullmatch('[a-f0-9]{64}', scope['inputHash']):
        raise ValueError('Invalid telemetry input')
    return json.dumps(scope, sort_keys=True, separators=(',', ':'))


def public_event(event):
    if not isinstance(event, dict):
        raise ValueError('Invalid public event')
    if event.get('kind') == 'text' and set(event) == {'kind', 'text'}:
        text = event['text']
        if isinstance(text, str) and len(text.encode('utf-8')) <= TEXT_LIMIT:
            return dict(event)
    if event.get('kind') == 'diagnostic' and set(event) == {'kind', 'code'} and event['code'] in DIAGNOSTICS:
        return dict(event)
    if event.get('kind') == 'stage' and set(event) == {'kind', 'stage'} and event['stage'] in (
            'preparing', 'cli_running', 'cli_exited', 'host_validation', 'trusted_rerun'):
        return dict(event)
    if event.get('kind') == 'tool' and event.get('tool') in (
            'receive_task', 'send_ack', 'send_result', 'read_project', 'write_project', 'run_check'):
        if event.get('phase') == 'started' and set(event) == {'kind', 'tool', 'phase'}:
            return dict(event)
        if event.get('phase') == 'finished' and set(event) == {'kind', 'tool', 'phase', 'outcome'} and event['outcome'] in ('ok', 'error'):
            return dict(event)
    if event.get('kind') == 'process':
        if set(event) == {'kind', 'pid', 'startedAt'} and type(event['pid']) is int and event['pid'] > 0 and type(event['startedAt']) in (int, float) and 0 <= event['startedAt'] <= 9007199254740991:
            return dict(event)
        if set(event) == {'kind', 'waitStartedAt', 'timeoutAt'} and all(type(event[k]) in (int, float) and 0 <= event[k] <= 9007199254740991 for k in ('waitStartedAt', 'timeoutAt')):
            return dict(event)
    raise ValueError('Invalid public event')


class TelemetryStore:
    def __init__(self, root):
        root = Path(root)
        root.mkdir(parents=True, exist_ok=True)
        self.database = root / 'telemetry.sqlite3'
        self.lock = threading.RLock()
        with self.connection() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS tails (
                    id INTEGER PRIMARY KEY, scope TEXT UNIQUE NOT NULL,
                    project TEXT NOT NULL, attempt TEXT NOT NULL,
                    sealed INTEGER NOT NULL DEFAULT 0, nextSeq INTEGER NOT NULL DEFAULT 1,
                    metadata TEXT NOT NULL DEFAULT '{}');
                CREATE TABLE IF NOT EXISTS events (
                    scope TEXT NOT NULL, seq INTEGER NOT NULL,
                    data TEXT NOT NULL, bytes INTEGER NOT NULL,
                    PRIMARY KEY(scope, seq));
            ''')
            if 'metadata' not in {row[1] for row in db.execute('PRAGMA table_info(tails)')}:
                db.execute("ALTER TABLE tails ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}'")

    @contextmanager
    def connection(self):
        with self.lock:
            db = sqlite3.connect(self.database, timeout=5)
            db.row_factory = sqlite3.Row
            try:
                with db:
                    yield db
            finally:
                db.close()

    def prune(self, db):
        rows = db.execute('SELECT scope, sealed FROM tails ORDER BY id DESC').fetchall()
        for index, row in enumerate(rows):
            if index >= 20 and row['sealed']:
                db.execute('DELETE FROM events WHERE scope=?', (row['scope'],))
                db.execute('DELETE FROM tails WHERE scope=?', (row['scope'],))

    def begin(self, scope):
        key = scope_key(scope)
        with self.connection() as db:
            try:
                db.execute('INSERT INTO tails(scope,project,attempt) VALUES(?,?,?)', (key, scope['projectId'], scope['attemptId']))
            except sqlite3.IntegrityError as error:
                raise ValueError('Telemetry already exists') from error
            self.prune(db)

    def append(self, scope, event):
        key = scope_key(scope)
        safe = public_event(event)
        with self.connection() as db:
            row = db.execute('SELECT * FROM tails WHERE scope=?', (key,)).fetchone()
            if row is None:
                raise ValueError('Unknown telemetry scope')
            if row['sealed']:
                return False
            safe.update(seq=row['nextSeq'], at=int(time.time() * 1000))
            data = json.dumps(safe, ensure_ascii=False, separators=(',', ':'))
            db.execute('INSERT INTO events VALUES(?,?,?,?)', (key, row['nextSeq'], data, len(data.encode('utf-8'))))
            metadata = json.loads(row['metadata'])
            metadata['lastEventAt'] = safe['at']
            if safe['kind'] == 'process':
                metadata.update({k: v for k, v in safe.items() if k not in ('kind', 'seq', 'at')})
            elif safe['kind'] == 'stage':
                metadata['stage'] = safe['stage']
                if safe['stage'] == 'cli_exited':
                    metadata['cliEndedAt'] = safe['at']
            db.execute('UPDATE tails SET nextSeq=nextSeq+1,metadata=? WHERE scope=?', (json.dumps(metadata), key))
            rows = db.execute('SELECT seq,bytes FROM events WHERE scope=? ORDER BY seq DESC', (key,)).fetchall()
            size = 0
            for index, item in enumerate(rows):
                size += item['bytes']
                if index >= TAIL_EVENTS or size > TAIL_BYTES:
                    db.execute('DELETE FROM events WHERE scope=? AND seq<=?', (key, item['seq']))
                    break
            return True

    def seal(self, scope):
        key = scope_key(scope)
        with self.connection() as db:
            if not db.execute('UPDATE tails SET sealed=1 WHERE scope=?', (key,)).rowcount:
                raise ValueError('Unknown telemetry scope')
            self.prune(db)

    def find(self, project_id, attempt_id=None):
        with self.connection() as db:
            if attempt_id is None:
                row = db.execute('SELECT scope FROM tails WHERE project=? ORDER BY id DESC LIMIT 1', (project_id,)).fetchone()
            else:
                row = db.execute('SELECT scope FROM tails WHERE project=? AND attempt=? ORDER BY id DESC LIMIT 1', (project_id, attempt_id)).fetchone()
            return json.loads(row['scope']) if row else None

    def read(self, scope, cursor=0, limit=PAGE_EVENTS):
        key = scope_key(scope)
        if type(cursor) is not int or cursor < 0 or type(limit) is not int or limit < 1:
            raise ValueError('Invalid telemetry cursor')
        with self.connection() as db:
            tail = db.execute('SELECT * FROM tails WHERE scope=?', (key,)).fetchone()
            if tail is None:
                raise ValueError('Unknown telemetry scope')
            if cursor >= tail['nextSeq']:
                raise ValueError('Invalid telemetry cursor')
            oldest = db.execute('SELECT MIN(seq) FROM events WHERE scope=?', (key,)).fetchone()[0] or tail['nextSeq']
            rows = db.execute('SELECT data FROM events WHERE scope=? AND seq>? ORDER BY seq LIMIT ?', (key, cursor, min(limit, PAGE_EVENTS))).fetchall()
            events, size = [], 0
            for row in rows:
                event = json.loads(row['data'])
                count = len(json.dumps(event).encode('utf-8')) + 2
                if size + count > PAGE_BYTES - 2048:
                    break
                events.append(event)
                size += count
            next_cursor = events[-1]['seq'] if events else cursor
            return {'scope': dict(scope), 'events': events, 'metadata': json.loads(tail['metadata']), 'nextCursor': next_cursor,
                    'hasMore': next_cursor < tail['nextSeq'] - 1, 'gap': cursor < oldest - 1, 'sealed': bool(tail['sealed'])}

    def stats(self, scope):
        with self.connection() as db:
            row = db.execute('SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM events WHERE scope=?', (scope_key(scope),)).fetchone()
            return {'events': row[0], 'bytes': row[1]}
