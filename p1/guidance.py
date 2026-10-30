"""Bounded role inbox operations, always inside the owning Store transaction."""
import json
import secrets
import time
import uuid

ROLES = ('architect', 'developer', 'tester')
UNRESOLVED = ('queued', 'received', 'needs_confirmation')
SCHEMA = '''
    CREATE TABLE IF NOT EXISTS agent_messages(
        seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
        project TEXT NOT NULL, role TEXT NOT NULL, binding TEXT NOT NULL,
        target_attempt TEXT, status TEXT NOT NULL, intent TEXT NOT NULL,
        text TEXT NOT NULL, summary TEXT NOT NULL DEFAULT '',
        delivery_attempt TEXT, nonce TEXT, created_at REAL NOT NULL, updated_at REAL NOT NULL);
    CREATE INDEX IF NOT EXISTS agent_thread ON agent_messages(project,role,seq);
    CREATE TABLE IF NOT EXISTS agent_controls(project TEXT PRIMARY KEY, attempt TEXT NOT NULL, data TEXT NOT NULL);
'''
COLUMNS = ('seq', 'id', 'project', 'role', 'binding', 'target_attempt', 'status', 'intent',
    'text', 'summary', 'delivery_attempt', 'nonce', 'created_at', 'updated_at')


class Guidance:
    def __init__(self, require, text, fields, digest, encoded):
        self.require, self.text, self.fields = require, text, fields
        self.digest, self.encoded = digest, encoded

    def binding(self, state):
        doc = state.get('uploadedDocument')
        return self.digest({'epoch': state['epoch'], 'contextRevision': state['contextRevision'],
            'documentHash': doc['sha256'] if doc else None,
            'tasks': {t['id']: [t['version'], t['documentVersion']] for t in state['tasks']}})

    def bounded_text(self, value, limit, label):
        result = self.text(value, limit, label)
        self.require(len(result.encode('utf-8')) <= 16384, label + '超过16KiB。')
        return result

    @staticmethod
    def rows(db, clause, values):
        return [dict(zip(COLUMNS, row)) for row in db.execute('SELECT * FROM agent_messages ' + clause, values)]

    def refresh(self, db, state, expire=False):
        db.execute("UPDATE agent_messages SET status='expired',nonce=NULL,updated_at=? WHERE project=? "
            "AND status IN ('queued','received','needs_confirmation') AND (? OR binding!=?)",
            (time.time(), state['id'], expire, self.binding(state)))
        counts = {role: dict.fromkeys(UNRESOLVED, 0) for role in ROLES}
        for role, status, count in db.execute('SELECT role,status,COUNT(*) FROM agent_messages WHERE project=? GROUP BY role,status', (state['id'],)):
            if status in UNRESOLVED:
                counts[role][status] = count
        state['agentInbox'] = counts

    def save(self, db, state, payload):
        self.fields(payload, ['role', 'text', 'intent', 'targetAttemptId'])
        role, target = payload['role'], payload['targetAttemptId']
        self.require(role in ROLES and payload['intent'] in ('clarification', 'scope_change'), '消息岗位或意图无效。')
        source = self.bounded_text(payload['text'], 4000, '消息')
        active = state['activeAttempt']
        if target is not None:
            self.require(isinstance(target, str) and active and active['id'] == target and
                active['role'] == role and active['status'] in ('queued', 'running') and active['epoch'] == state['epoch'], '消息目标阶段无效。')
        elif active and active['role'] == role:
            target = active['id']
        count = db.execute("SELECT COUNT(*) FROM agent_messages WHERE project=? AND status IN ('queued','received','needs_confirmation')", (state['id'],)).fetchone()[0]
        self.require(count < 100, '本项目已有100条未解决消息，请先处理。')
        now = time.time()
        status = 'needs_confirmation' if payload['intent'] == 'scope_change' else 'queued'
        db.execute('INSERT INTO agent_messages(id,project,role,binding,target_attempt,status,intent,text,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)',
            (uuid.uuid4().hex, state['id'], role, self.binding(state), target, status, payload['intent'], source, now, now))

    def resolve(self, db, state, payload):
        self.fields(payload, ['messageId', 'decision', 'text'])
        self.require(isinstance(payload['messageId'], str), '消息ID无效。')
        source = self.bounded_text(payload['text'], 4000, '处理说明')
        items = self.rows(db, 'WHERE id=? AND project=?', (payload['messageId'], state['id']))
        self.require(items and items[0]['status'] in UNRESOLVED, '消息已解决或不属于当前项目。')
        item, decision = items[0], payload['decision']
        self.require(item['binding'] == self.binding(state), '消息上下文已失效。')
        self.require(decision in ('supersede', 'clarification'), '消息处理决定无效。')
        if decision == 'clarification':
            self.require(item['status'] == 'needs_confirmation', '仅待确认消息可重新解释。')
            db.execute("UPDATE agent_messages SET status='queued',intent='clarification',text=?,summary='',nonce=NULL,delivery_attempt=NULL,updated_at=? WHERE id=?",
                (source, time.time(), item['id']))
        else:
            db.execute("UPDATE agent_messages SET status='superseded',summary=?,nonce=NULL,updated_at=? WHERE id=?", (source[:2000], time.time(), item['id']))

    @staticmethod
    def public(item):
        return {'seq': item['seq'], 'id': item['id'], 'projectId': item['project'], 'role': item['role'],
            'author': 'agent' if item['intent'] == 'reply' else 'user',
            'targetAttemptId': item['target_attempt'], 'status': item['status'], 'intent': item['intent'],
            'text': item['text'], 'summary': item['summary'],
            'createdAt': item['created_at'], 'updatedAt': item['updated_at']}

    def thread(self, db, project_id, role, after, limit):
        self.require(role in ROLES, '消息岗位无效。')
        self.require(type(after) is int and 0 <= after <= 2**53 - 1 and type(limit) is int and 1 <= limit <= 50, '消息分页无效。')
        rows = self.rows(db, 'WHERE project=? AND role=? AND seq>? ORDER BY seq LIMIT ?', (project_id, role, after, limit + 1))
        page = rows[:limit]
        return {'projectId': project_id, 'role': role, 'messages': [self.public(item) for item in page],
            'nextCursor': page[-1]['seq'] if page else after, 'hasMore': len(rows) > limit}

    def live(self, db, attempt_id, get):
        self.require(isinstance(attempt_id, str), '阶段ID无效。')
        row = db.execute('SELECT project,status,data FROM attempts WHERE id=?', (attempt_id,)).fetchone()
        self.require(row and row[1] == 'running', '收件阶段未运行或已结束。')
        state = get(db, row[0])
        active, stored = state['activeAttempt'], json.loads(row[2])
        self.require(active and active['id'] == attempt_id and active['epoch'] == state['epoch'] and
            active['inputHash'] == stored['inputHash'] and not state['leaseUnknown'] and not state['recoveryRequired'], '收件阶段上下文失效。')
        return state, active

    def pending(self, db, state, attempt):
        selected = set(attempt.get('input', {}).get('consultation', {}).get('messageIds', []))
        return [item for item in self.rows(db, "WHERE project=? AND role=? AND binding=? AND (target_attempt IS NULL OR target_attempt=?) "
            "AND status IN ('queued','received','needs_confirmation') ORDER BY seq",
            (state['id'], attempt['role'], self.binding(state), attempt['id'])) if item['id'] not in selected]

    def select(self, db, state, role, message_ids):
        self.require(isinstance(message_ids, list) and len(message_ids) <= 50 and
            all(isinstance(item, str) for item in message_ids) and len(set(message_ids)) == len(message_ids), '消息选择无效。')
        selected = []
        for message_id in message_ids:
            items = self.rows(db, 'WHERE id=? AND project=? AND role=?', (message_id, state['id'], role))
            self.require(items and items[0]['binding'] == self.binding(state) and
                items[0]['status'] in ('queued', 'received') and items[0]['intent'] == 'clarification', '所选消息已失效或需要范围确认。')
            selected.append(self.public(items[0]))
        return selected

    def receive(self, db, state, attempt):
        deliveries = []
        for item in self.pending(db, state, attempt)[:50]:
            if not item['nonce']:
                item['nonce'] = secrets.token_urlsafe(32)
                db.execute('UPDATE agent_messages SET target_attempt=?,delivery_attempt=?,nonce=?,updated_at=? WHERE id=?',
                    (attempt['id'], attempt['id'], item['nonce'], time.time(), item['id']))
                item['target_attempt'], item['delivery_attempt'] = attempt['id'], attempt['id']
            self.require(item['delivery_attempt'] == attempt['id'], '投递阶段失效。')
            deliveries.append({**self.public(item), 'nonce': item['nonce']})
        return {'attemptId': attempt['id'], 'messages': deliveries}

    def acknowledge(self, db, state, attempt, message_id, nonce, status, summary):
        self.require(isinstance(message_id, str) and isinstance(nonce, str) and bool(nonce) and nonce.isascii() and len(nonce) <= 100, '投递凭据无效。')
        summary = self.bounded_text(summary, 2000, '公开回执')
        items = self.rows(db, 'WHERE id=? AND project=?', (message_id, state['id']))
        self.require(bool(items), '消息不属于此项目。')
        item = items[0]
        self.require(item['role'] == attempt['role'] and item['binding'] == self.binding(state) and
            item['delivery_attempt'] == attempt['id'] and item['target_attempt'] == attempt['id'] and
            item['nonce'] and secrets.compare_digest(item['nonce'], nonce), '消息投递凭据或上下文失效。')
        allowed = {'queued': ('received', 'needs_confirmation'), 'received': ('applied', 'needs_confirmation')}
        self.require(status in allowed.get(item['status'], ()), '回执顺序无效；收件、应用和待确认不能混用。')
        db.execute('UPDATE agent_messages SET status=?,summary=?,updated_at=? WHERE id=?', (status, summary, time.time(), item['id']))
        return {'messageId': item['id'], 'status': status, 'summary': summary}

    def control(self, db, state, attempt):
        items = self.pending(db, state, attempt)
        requested = state.get('agentControl', {})
        return {'pending': len(items), 'needsConfirmation': any(item['status'] == 'needs_confirmation' for item in items),
            'handoffRequested': requested.get('status') == 'handoff_requested' and requested.get('attemptId') == attempt['id']}
