"""Scoped stdio MCP tools. ACK/RESULT are sent only on model tool calls."""
import ctypes
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import sys
import threading
import time
import urllib.request
import uuid

if __package__:
    from .winjob import JobProcess
else:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from winjob import JobProcess


def parse_messages(raw):
    if re.fullmatch(r'No messages\. You are [^\r\n]+\.\s*', raw):
        return []
    pattern = re.compile(r'<reason-message\s+([^>]+)>\r?\n([^\r\n]+)\r?\n</reason-message>', re.M)
    matches = list(pattern.finditer(raw))
    if not matches or raw.count('<reason-message') != len(matches) or raw.count('</reason-message>') != len(matches):
        raise ValueError('Unrecognized reason listen format')
    result = []
    for match in matches:
        attrs = dict(re.findall(r'([\w-]+)="([^"<>]*)"', match[1]))
        if not attrs.get('from') or not attrs.get('id'):
            raise ValueError('Message identity missing')
        body = json.loads(match[2])
        if not isinstance(body, dict):
            raise ValueError('Envelope must be a JSON object')
        result.append({'sender': attrs['from'], 'id': attrs['id'], 'body': body})
    return result


def safe_path(root, relative):
    root = Path(root).resolve()
    if not isinstance(relative, str) or not relative or len(relative) > 240 or '\\' in relative or ':' in relative:
        raise ValueError('Use a bounded relative slash path')
    parts = PurePosixPath(relative).parts
    if PurePosixPath(relative).is_absolute() or any(p in ('.', '..') or p.startswith('.') or p.rstrip(' .') != p
        or re.fullmatch(r'(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?', p, re.I) for p in parts):
        raise ValueError('Path is not permitted')
    target = root
    for part in parts:
        target = target / part
        if target.exists() or target.is_symlink():
            stat = target.lstat()
            if target.is_symlink() or getattr(stat, 'st_file_attributes', 0) & 0x400:
                raise ValueError('Reparse points are not permitted')
    if not target.resolve().is_relative_to(root):
        raise ValueError('Path escaped project')
    return target


class Exchange:
    def __init__(self, project_id, run_id, attempt_id, epoch, input_hash):
        self.binding = {'projectId': project_id, 'runId': run_id, 'attempt': attempt_id, 'epoch': epoch, 'inputHash': input_hash}
        self.nonce = secrets.token_hex(20)
        self.acknowledged = False
        self.finished = False
        self.cancelled = False

    def envelope(self, kind, payload):
        return {**self.binding, 'messageId': uuid.uuid4().hex, 'kind': kind, 'nonce': self.nonce, 'payload': payload}

    def task(self, payload):
        return self.envelope('TASK', payload)

    def wakeup(self):
        return {'attempt': self.binding['attempt'], 'instruction': 'Call receive_task, explicitly acknowledge, then perform the task.'}

    def _valid(self, nonce):
        if self.cancelled or self.finished or not secrets.compare_digest(str(nonce), self.nonce):
            raise ValueError('Cancelled, stale or mismatched exchange')

    def ack(self, nonce, summary):
        self._valid(nonce)
        if self.acknowledged or not isinstance(summary, str) or not summary.strip() or len(summary) > 2000:
            raise ValueError('Invalid or duplicate ACK')
        self.acknowledged = True
        return self.envelope('ACK', {'summary': summary})

    def result(self, nonce, result):
        self._valid(nonce)
        if not self.acknowledged or not isinstance(result, dict):
            raise ValueError('Explicit ACK and object result required')
        self.finished = True
        return self.envelope('RESULT', result)

    def matches(self, envelope):
        return isinstance(envelope, dict) and all(envelope.get(k) == v for k, v in self.binding.items()) and envelope.get('nonce') == self.nonce


class ReasonTransport:
    def __init__(self, exe, home, cwd):
        self.exe, self.home, self.cwd = Path(exe), Path(home), Path(cwd)
        self.home.mkdir(parents=True, exist_ok=True)
        self.env = dict(os.environ, REASON_NETWORK_HOME=str(self.home), REASON_NETWORK_NO_SERVICE='1', REASON_NETWORK_NO_UPDATE='1')
        self.daemon = None
        self.lock = threading.Lock()
        self.audit = []

    def start(self):
        self.daemon = JobProcess([str(self.exe), 'daemon'], self.cwd, env=self.env)
        time.sleep(.5)
        if self.daemon.poll() is not None:
            raise RuntimeError('Dedicated reason daemon failed to start')

    def close(self):
        if self.daemon:
            self.daemon.close()
            self.daemon = None

    def call(self, args, source=b'', timeout=20):
        if not self.daemon or self.daemon.poll() is not None:
            raise RuntimeError('Owned reason daemon is not running; no automatic restart')
        child = JobProcess([str(self.exe), *args], self.cwd, env=self.env, input_data=source)
        try:
            result = child.wait(timeout)
            self.audit.append({'argv': args, **result})
            if result['exitCode'] != 0 or not result['treeStopped'] or result['timedOut']:
                raise RuntimeError('reason operation failed: ' + (result['stderr'] or result['stdout'])[:1000])
            return result['stdout']
        finally:
            child.close()

    def register(self, name):
        self.call(['listen', '--as', name, '--wait', '0'])
        directory = json.loads(self.call(['who', '--all', '--json']))
        addresses = [s['address'] for s in directory['sessions'] if s.get('name') == name]
        if len(addresses) != 1:
            raise ValueError('Mailbox address is missing or ambiguous')
        return addresses[0]

    def send(self, sender, target, envelope):
        raw = self.call(['send', target, '--from', sender, '-'], json.dumps(envelope, ensure_ascii=False, separators=(',', ':')).encode('utf-8'))
        match = re.search(r'^message id ([A-Z0-9]+)\s*$', raw, re.M)
        if not match:
            raise ValueError('Unrecognized reason send receipt')
        return match[1]

    def listen(self, name):
        return parse_messages(self.call(['listen', '--as', name, '--wait', '0']))


def schema(properties, required=None):
    return {'type': 'object', 'properties': properties, 'required': list(properties) if required is None else required, 'additionalProperties': False}


TOOLS = [
    {'name': 'receive_task', 'description': 'Receive your real task and nonce from reason-network. Call first.', 'inputSchema': schema({})},
    {'name': 'send_ack', 'description': 'Explicitly acknowledge the task you just read; never assume sent means read.', 'inputSchema': schema({'nonce': {'type': 'string'}, 'summary': {'type': 'string'}})},
    {'name': 'send_result', 'description': 'Send your final typed result through reason-network, after completing the task.', 'inputSchema': schema({'nonce': {'type': 'string'}, 'result': {'type': 'object'}})},
    {'name': 'read_project', 'description': 'Read a UTF-8 managed project file. product/ or tests/ relative path only.', 'inputSchema': schema({'relativePath': {'type': 'string'}})},
    {'name': 'write_project', 'description': 'Write one UTF-8 file in your leased role area only. No shell commands.', 'inputSchema': schema({'relativePath': {'type': 'string'}, 'text': {'type': 'string'}})},
    {'name': 'run_check', 'description': 'Execute a fixed trusted check: inventory or browser. Returns actual evidence.', 'inputSchema': schema({'checkId': {'type': 'string', 'enum': ['inventory', 'browser']}})},
]


def mcp_dispatch(request, handler):
    if not isinstance(request, dict) or request.get('jsonrpc') != '2.0':
        return {'jsonrpc': '2.0', 'id': None, 'error': {'code': -32600, 'message': 'Invalid JSON-RPC'}}
    request_id = request.get('id')
    method = request.get('method', '')
    if request_id is None:
        return None
    reply = {'jsonrpc': '2.0', 'id': request_id}
    if method == 'initialize':
        reply['result'] = {'protocolVersion': request.get('params', {}).get('protocolVersion', '2024-11-05'),
            'capabilities': {'tools': {}}, 'serverInfo': {'name': 'relay-scoped', 'version': '1.0.0'}}
    elif method == 'ping':
        reply['result'] = {}
    elif method == 'tools/list':
        reply['result'] = {'tools': TOOLS}
    elif method == 'tools/call':
        try:
            params = request.get('params', {})
            name, arguments = params.get('name'), params.get('arguments', {})
            tool = next((t for t in TOOLS if t['name'] == name), None)
            if not tool or not isinstance(arguments, dict) or set(arguments) != set(tool['inputSchema']['required']):
                raise ValueError('Unknown tool or invalid arguments')
            data = handler(name, arguments)
            reply['result'] = {'content': [{'type': 'text', 'text': json.dumps(data, ensure_ascii=False)}]}
        except Exception as error:
            reply['result'] = {'isError': True, 'content': [{'type': 'text', 'text': str(error)[:2000]}]}
    else:
        reply['error'] = {'code': -32601, 'message': 'Method not supported'}
    return reply


def stdio_main():
    # Capability arrives through the MCP child's private environment, never browser logs.
    endpoint = os.environ['RELAY_MCP_ENDPOINT']
    token = os.environ['RELAY_MCP_TOKEN']
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def handler(name, arguments):
        req = urllib.request.Request(endpoint, data=json.dumps({'name': name, 'arguments': arguments}).encode('utf-8'),
            headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token}, method='POST')
        try:
            with opener.open(req, timeout=120) as response:
                result = json.load(response)
        except urllib.error.HTTPError as error:
            raise ValueError('Tool denied or attempt no longer active') from error
        if 'error' in result:
            raise ValueError(result['error'])
        return result
    for line in sys.stdin.buffer:
        if len(line) > 1024 * 1024:
            break
        try:
            reply = mcp_dispatch(json.loads(line), handler)
        except Exception:
            reply = {'jsonrpc': '2.0', 'id': None, 'error': {'code': -32700, 'message': 'Parse error'}}
        if reply is not None:
            sys.stdout.write(json.dumps(reply, ensure_ascii=False) + '\n')
            sys.stdout.flush()


if __name__ == '__main__':
    stdio_main()
