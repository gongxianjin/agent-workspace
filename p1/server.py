"""Loopback-only control API. No browser-supplied shell, paths or executables."""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import mimetypes
import msvcrt
from pathlib import Path
import re
import secrets
import socket
import sys
import threading
import time
from urllib.parse import unquote, urlsplit, parse_qs
import webbrowser

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime import BASE, CLIENTS, REASON, tree_manifest
from reason_mcp import safe_path
from state import Store, Rejected, fields
from environment import SetupStatus
from telemetry import TelemetryStore

MODEL_COMMANDS = frozenset(('generate', 'split', 'regenerate', 'start', 'resume', 'repair', 'retest', 'retry'))


class OwnedRoot(RuntimeError):
    pass


class RootLock:
    """Exclusive Windows byte lock, acquired before SQLite recovery can mutate state."""
    def __init__(self, root):
        root = Path(root).resolve()
        root.mkdir(parents=True, exist_ok=True)
        self.file = (root / 'service.lock').open('a+b')
        # Windows locks may extend beyond EOF; initializing bytes before locking races the owner.
        self.file.seek(0)
        try:
            msvcrt.locking(self.file.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError as error:
            self.file.close()
            raise OwnedRoot('This runtime root is already owned by another service') from error

    def close(self):
        if not self.file.closed:
            self.file.seek(0)
            msvcrt.locking(self.file.fileno(), msvcrt.LK_UNLCK, 1)
            self.file.close()


class Application:
    def __init__(self, root, port=8787, workers=True):
        self.root_lock = RootLock(root)
        self.http = self.preview = self.preview_thread = self.engine = None
        self.serving = threading.Event()
        self.closed = False
        self.accepting = False
        try:
            self._initialize(root, port, workers)
        except BaseException:
            self.close()
            raise

    def _initialize(self, root, port, workers):
        self.store = Store(root)
        self.action_lock = threading.RLock()
        self.tokens = {}
        self.bootstraps = {}
        self.auth_lock = threading.Lock()
        self.bootstrap = self.issue_bootstrap()
        self.shutdown_requested = threading.Event()
        self.accepting = True
        self.worker_enabled = workers
        self.setup = SetupStatus(BASE)
        self.telemetry = TelemetryStore(self.store.root)
        app = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def setup(self):
                super().setup()
                self.connection.settimeout(10)

            def send_bytes(self, status, raw, content_type='application/json; charset=utf-8', extra=None):
                self.send_response(status)
                self.send_header('Content-Type', content_type)
                self.send_header('Content-Length', str(len(raw)))
                self.send_header('Cache-Control', 'no-store')
                self.send_header('X-Content-Type-Options', 'nosniff')
                self.send_header('Referrer-Policy', 'no-referrer')
                self.send_header('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-src http://127.0.0.1:" + str(app.preview_port) + "; frame-ancestors 'none'; object-src 'none'; base-uri 'none'; form-action 'none'")
                for key, value in (extra or {}).items():
                    self.send_header(key, value)
                try:
                    self.end_headers()
                    self.wfile.write(raw)
                except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
                    # A disconnected peer cannot receive a second error response.
                    self.close_connection = True

            def answer(self, status, value):
                self.send_bytes(status, json.dumps(value, ensure_ascii=False).encode('utf-8'))

            def reject_body(self, status, message):
                self.close_connection = True
                self.send_bytes(status, json.dumps({'error': message}).encode('utf-8'), extra={'Connection': 'close'})
                self.wfile.flush()
                # Send the rejection before discarding a bounded unread body; closing
                # with queued input can reset the socket before the peer reads it.
                deadline, remaining = time.monotonic() + 1, 1024 * 1024
                try:
                    self.connection.shutdown(socket.SHUT_WR)
                    while remaining > 0:
                        wait = deadline - time.monotonic()
                        if wait <= 0:
                            break
                        self.connection.settimeout(wait)
                        chunk = self.rfile.read1(min(8192, remaining))
                        if not chunk:
                            break
                        remaining -= len(chunk)
                except OSError:
                    pass

            def authorized(self):
                auth = self.headers.get('Authorization', '')
                token = auth.removeprefix('Bearer ') if auth.startswith('Bearer ') else ''
                with app.auth_lock:
                    return app.tokens.get(token, 0) > time.time()

            def route(self):
                if self.headers.get('Host') != f'127.0.0.1:{app.port}':
                    self.answer(403, {'error': 'Host denied'}); return
                origin = self.headers.get('Origin')
                if origin is not None and origin != app.origin:
                    self.answer(403, {'error': 'Origin denied'}); return
                if self.headers.get('Sec-Fetch-Site') == 'cross-site':
                    self.answer(403, {'error': 'Cross-site denied'}); return
                path = urlsplit(self.path).path
                if self.command == 'GET' and path in ('/', '/index.html', '/styles.css', '/model.js', '/setup.js', '/live-output.js', '/live.js', '/app.js'):
                    target = BASE / ('index.html' if path == '/' else path[1:])
                    if not target.is_file():
                        self.answer(404, {'error': 'Not found'}); return
                    self.send_bytes(200, target.read_bytes(), mimetypes.guess_type(str(target))[0] or 'application/octet-stream'); return
                if not path.startswith('/api/'):
                    self.answer(404, {'error': 'Not found'}); return
                if self.command != 'GET' and origin != app.origin:
                    self.answer(403, {'error': 'Exact Origin required'}); return
                if path != '/api/session' and not self.authorized():
                    self.answer(403, {'error': 'Session expired or missing; reopen the launch link'}); return
                body = None
                if self.command == 'POST':
                    if self.headers.get('Transfer-Encoding') or self.headers.get('Content-Type', '').split(';')[0] != 'application/json':
                        self.reject_body(400, 'JSON Content-Length required'); return
                    try:
                        length = int(self.headers.get('Content-Length', '0'))
                        if not 0 < length <= 512 * 1024:
                            self.reject_body(413, 'Body exceeds limit'); return
                        body = json.loads(self.rfile.read(length).decode('utf-8', errors='strict'))
                    except (ValueError, UnicodeError):
                        self.answer(400, {'error': 'Invalid JSON/UTF-8'}); return
                try:
                    if self.command == 'POST' and path == '/api/session':
                        fields(body, ['bootstrap'])
                        with app.auth_lock:
                            app._prune_auth()
                            candidate = body['bootstrap']
                            found = next((key for key in app.bootstraps if isinstance(candidate, str) and secrets.compare_digest(candidate, key)), None)
                            if found is None:
                                self.answer(403, {'error': 'Launch link expired, used or invalid; double-click the launcher to reopen'}); return
                            del app.bootstraps[found]
                            token = secrets.token_urlsafe(32)
                            app.tokens[token] = time.time() + 8 * 3600
                        self.answer(200, {'token': token}); return
                    if self.command == 'GET' and path == '/api/telemetry':
                        self.answer(200, app.read_telemetry(parse_qs(urlsplit(self.path).query, keep_blank_values=True))); return
                    if self.command == 'GET' and path == '/api/setup':
                        self.answer(200, app.setup.public()); return
                    if self.command == 'POST' and path == '/api/setup/distribution':
                        fields(body, ['confirmed', 'sha256'])
                        with app.action_lock:
                            if not app.accepting:
                                raise Rejected('工作台正在退出。')
                            try:
                                result = app.setup.accept_distribution(body['confirmed'], body['sha256'])
                            except ValueError as error:
                                raise Rejected(str(error)) from error
                            self.answer(200, result)
                        return
                    if self.command == 'POST' and path == '/api/setup/recheck':
                        fields(body, [])
                        with app.action_lock:
                            if not app.accepting:
                                raise Rejected('工作台正在退出。')
                            self.answer(200, app.setup.recheck())
                        return
                    if self.command == 'POST' and path == '/api/setup/browser':
                        fields(body, ['confirmed'])
                        if body['confirmed'] is not True:
                            raise Rejected('必须明确确认从官方源准备浏览器。')
                        with app.action_lock:
                            if not app.accepting:
                                raise Rejected('工作台正在退出。')
                            states = app.store.list_projects()
                            if any(s['activeAttempt'] or s['leaseUnknown'] or s['recoveryRequired']
                                    or s['phase'] in ('stopping', 'paused') for s in states):
                                raise Rejected('请先等待所有任务停稳，浏览器准备不会打断任务。')
                            try:
                                result = app.setup.prepare_browser(True)
                            except ValueError as error:
                                raise Rejected(str(error)) from error
                            self.answer(200, result)
                        return
                    if self.command == 'POST' and path == '/api/setup/confirm-existing':
                        fields(body, ['confirmed'])
                        if body['confirmed'] is not True:
                            raise Rejected('必须明确确认使用已有本机配置继续，接入仍未验证。')
                        with app.action_lock:
                            if not app.accepting:
                                raise Rejected('工作台正在退出。')
                            self.answer(200, app.setup.confirm_existing())
                        return
                    if self.command == 'POST' and path == '/api/shutdown':
                        fields(body, [])
                        app.request_shutdown()
                        self.answer(200, {'stopping': True}); return
                    if self.command == 'GET' and path == '/api/state':
                        states = app.store.list_projects()
                        for state in states:
                            state['previewUrl'] = f'http://127.0.0.1:{app.preview_port}/{state["id"]}/index.html'
                        self.answer(200, {'projects': states, 'mode': 'live', 'workers': app.worker_enabled,
                            'reason': 'running' if app.engine and app.engine.transport else 'starts on first task',
                            'clients': {name: target.is_file() for name, target in CLIENTS.items()}}); return
                    if self.command == 'POST' and path == '/api/projects':
                        fields(body, ['name'])
                        with app.action_lock:
                            if not app.accepting:
                                raise Rejected('工作台正在退出，不接收新操作。')
                            created = app.store.create_project(body['name'])
                        self.answer(200, created); return
                    if self.command == 'POST' and path in ('/api/command', '/api/documents'):
                        fields(body, ['projectId', 'commandId', 'expectedRevision', 'kind', 'payload'])
                        if path == '/api/documents' and body['kind'] != 'upload':
                            raise Rejected('Document endpoint only accepts uploads')
                        with app.action_lock:
                            if not app.accepting:
                                raise Rejected('工作台正在退出，不接收新操作。')
                            if body['kind'] in MODEL_COMMANDS:
                                try:
                                    app.setup.snapshot()
                                except ValueError as error:
                                    raise Rejected(str(error)) from error
                            state = app.store.command(body['projectId'], body['commandId'], body['expectedRevision'], body['kind'], body['payload'])
                            current = app.store.get(body['projectId'])
                            if body['kind'] == 'stop' and current['epoch'] == state['epoch'] and current['phase'] in ('stopping', 'blocked'):
                                confirmed = app.engine.stop(body['projectId']) if app.engine else not current['recoveryRequired']
                                if confirmed:
                                    state = app.store.confirm_stopped(body['projectId'], True)
                            if app.engine:
                                app.engine.notify()
                        self.answer(200, state); return
                    if self.command == 'GET' and path == '/api/document':
                        project_id = parse_qs(urlsplit(self.path).query).get('project', [''])[0]
                        state = app.store.get(project_id)
                        if not state['generatedDocument']:
                            raise Rejected('尚无设计文档。')
                        self.send_bytes(200, state['generatedDocument']['markdown'].encode('utf-8'), 'text/markdown; charset=utf-8',
                            {'Content-Disposition': 'attachment; filename="design.md"'}); return
                    if self.command == 'GET' and path.startswith('/api/artifacts/'):
                        # IDs refer to trusted check outputs only; never an arbitrary local path.
                        parts = path.split('/')
                        if len(parts) != 5:
                            self.answer(404, {'error': 'Artifact not found'}); return
                        from checks import resolve_artifact
                        try:
                            found = resolve_artifact(app.store.root, parts[3], parts[4])
                        except (OSError, ValueError, KeyError):
                            self.answer(404, {'error': 'Artifact not found'}); return
                        self.send_bytes(200, found.read_bytes(), 'application/octet-stream', {'Content-Disposition': f'attachment; filename="{parts[4]}"'}); return
                    self.answer(404, {'error': 'Not found'})
                except Rejected as error:
                    self.answer(409, {'error': str(error)})
                except (TypeError, KeyError, ValueError) as error:
                    self.answer(400, {'error': str(error)[:500]})
                except Exception:
                    self.answer(500, {'error': 'Local operation failed; inspect local evidence'})

            def do_GET(self):
                self.route()

            def do_POST(self):
                self.route()

            def log_message(self, *args):
                pass

        self.http = ThreadingHTTPServer(('127.0.0.1', port), Handler)
        self.http.daemon_threads = True
        self.port = self.http.server_port
        self.origin = f'http://127.0.0.1:{self.port}'

        class Preview(BaseHTTPRequestHandler):
            def do_GET(self):
                try:
                    if self.headers.get('Host') != f'127.0.0.1:{app.preview_port}':
                        raise ValueError('Host')
                    parts = unquote(urlsplit(self.path).path).lstrip('/').split('/', 1)
                    if len(parts) != 2 or not re.fullmatch(r'[a-f0-9]{32}', parts[0]):
                        raise ValueError('Project ID')
                    app.store.get(parts[0])
                    target = safe_path(app.store.root / 'projects' / parts[0] / 'product', parts[1])
                    if not target.is_file() or target.stat().st_size > 2 * 1024 * 1024:
                        raise ValueError('Preview file')
                    self.send_response(200)
                    self.send_header('Content-Type', (mimetypes.guess_type(str(target))[0] or 'application/octet-stream') + '; charset=utf-8')
                    self.send_header('Content-Security-Policy', "sandbox allow-scripts; default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors " + app.origin)
                    self.send_header('X-Content-Type-Options', 'nosniff')
                    raw = target.read_bytes()
                    self.send_header('Content-Length', str(len(raw)))
                    self.end_headers(); self.wfile.write(raw)
                except (OSError, Rejected, ValueError):
                    self.send_error(404)
            def log_message(self, *args):
                pass
        self.preview = ThreadingHTTPServer(('127.0.0.1', 0), Preview)
        self.preview.daemon_threads = True
        self.preview_port = self.preview.server_port
        self.preview_thread = threading.Thread(target=self.preview.serve_forever, daemon=True)
        self.preview_thread.start()
        if workers:
            from engine import Engine
            self.engine = Engine(self.store, setup=self.setup, telemetry=self.telemetry)

    def read_telemetry(self, query):
        if set(query) - {'project', 'attempt', 'epoch', 'cursor', 'limit'} or any(len(v) != 1 for v in query.values()):
            raise ValueError('Invalid telemetry query')
        project_id = query.get('project', [''])[0]
        if not re.fullmatch('[a-f0-9]{32}', project_id):
            raise ValueError('Invalid telemetry project')
        cursor = query.get('cursor', ['0'])[0]
        limit = query.get('limit', ['200'])[0]
        if not cursor.isascii() or not cursor.isdecimal() or len(cursor) > 16 or not limit.isascii() or not limit.isdecimal() or len(limit) > 4:
            raise ValueError('Invalid telemetry cursor')
        cursor, limit = int(cursor), int(limit)
        if not 1 <= limit <= 200:
            raise ValueError('Invalid telemetry limit')
        bound = 'attempt' in query and 'epoch' in query
        if ('attempt' in query) != ('epoch' in query) or cursor and not bound:
            raise ValueError('Bind cursor to an attempt and epoch')
        state = self.store.get(project_id)
        requested = query['attempt'][0] if bound else None
        if requested and not re.fullmatch('[a-f0-9]{32}', requested):
            raise ValueError('Invalid telemetry attempt')
        current = state['activeAttempt'] or state['lastAttempt']
        selected = requested or (current['id'] if current else None)
        scope = self.telemetry.find(project_id, selected)
        authority = {key: state[key] for key in ('phase', 'epoch', 'revision', 'leaseUnknown', 'recoveryRequired')}
        authority['activeAttemptId'] = state['activeAttempt']['id'] if state['activeAttempt'] else None
        metadata = {}
        if selected:
            try:
                item = self.store.attempt_info(project_id, selected)
            except Rejected as error:
                raise ValueError('Telemetry attempt not found in this project') from error
            metadata = {'role': item['role'], 'kind': item['kind'], 'queuedAt': item['createdAt'] * 1000, 'status': item['status']}
            if scope and any(scope[key] != item[key if key != 'attemptId' else 'id'] for key in scope):
                raise ValueError('Telemetry binding changed')
            if bound and query['epoch'][0] != str(item['epoch']):
                raise ValueError('Telemetry epoch changed')
        page = self.telemetry.read(scope, cursor, limit) if scope else {
            'scope': None, 'events': [], 'nextCursor': 0, 'hasMore': False, 'gap': False, 'sealed': False}
        if cursor and not scope:
            raise ValueError('Telemetry cursor has no scope')
        return {**page, 'serverNow': int(time.time() * 1000), 'metadata': {**page.get('metadata', {}), **metadata},
                'authority': authority, 'reset': False, 'available': scope is not None}

    def _prune_auth(self):
        now = time.time()
        self.tokens = {key: end for key, end in self.tokens.items() if end > now}
        self.bootstraps = {key: end for key, end in self.bootstraps.items() if end > now}

    def issue_bootstrap(self):
        with self.auth_lock:
            self._prune_auth()
            while len(self.bootstraps) >= 8:
                self.bootstraps.pop(next(iter(self.bootstraps)))
            if len(self.tokens) >= 128:
                raise Rejected('页面会话已达上限，请关闭旧会话或等待过期。')
            token = secrets.token_urlsafe(32)
            self.bootstraps[token] = time.time() + 60
            return token

    def open_ui(self):
        if not self.accepting:
            raise Rejected('工作台正在退出。')
        webbrowser.open(self.origin + '/#bootstrap=' + self.issue_bootstrap())

    def request_shutdown(self):
        with self.action_lock:
            from browser_setup import ACTIVE
            if self.setup.public().get('browser', {}).get('status') in ACTIVE:
                raise Rejected('浏览器正在准备，请等待结束后再退出。')
            states = self.store.list_projects()
            if any(s['activeAttempt'] or s['leaseUnknown'] or s['recoveryRequired']
                    or s['phase'] in ('stopping', 'paused') for s in states):
                raise Rejected('存在运行／排队／暂停或未知租约；请先处理任务，退出不会自动终止模型。')
            if self.engine:
                with self.engine.lock:
                    if self.engine.active_project is not None:
                        raise Rejected('执行器尚未空闲，请等待清理结束。')
            self.accepting = False
            self.shutdown_requested.set()

    def serve(self):
        self.serving.set()
        self.http.serve_forever()

    def close(self):
        if self.closed:
            return
        self.accepting = False
        if self.engine:
            self.engine.close()
        setup = getattr(self, 'setup', None)
        if setup and hasattr(setup, 'close'):
            setup.close()
        if self.http:
            if self.serving.is_set():
                self.http.shutdown()
            self.http.server_close()
        if self.preview:
            if self.preview_thread and self.preview_thread.is_alive():
                self.preview.shutdown()
            self.preview.server_close()
        if self.preview_thread and self.preview_thread.ident is not None:
            self.preview_thread.join(timeout=3)
            if self.preview_thread.is_alive():
                raise RuntimeError('Preview host did not stop')
        self.root_lock.close()
        self.closed = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--port', type=int, default=8787)
    parser.add_argument('--root', type=Path, default=BASE / 'dist/runtime')
    parser.add_argument('--no-browser', action='store_true')
    parser.add_argument('--no-workers', action='store_true', help='API/browser fixture tests only; never simulates model completion')
    parser.add_argument('--ready-file', type=Path, help='Local automation launch handoff, contains a one-time capability; do not publish')
    args = parser.parse_args()
    if not CLIENTS['Claude Code'].is_file() or not REASON.is_file():
        raise SystemExit('Claude Code/reason-network executable missing')
    app = Application(args.root, args.port, workers=not args.no_workers)
    launch = app.origin + '/#bootstrap=' + app.bootstrap
    if args.ready_file:
        args.ready_file.write_text(json.dumps({'url': launch, 'origin': app.origin}), encoding='utf-8')
    if not args.no_browser:
        webbrowser.open(launch)
    print('Relay live workbench: ' + app.origin + ' (3 independent Claude Code roles; Ctrl+C stops)', flush=True)
    try:
        app.serve()
    except KeyboardInterrupt:
        pass
    finally:
        if args.ready_file and args.ready_file.exists():
            args.ready_file.write_text('{"stopped":true}', encoding='utf-8')
        # Shutdown must run outside the serve_forever thread.
        thread = threading.Thread(target=app.close)
        thread.start(); thread.join(timeout=30)


if __name__ == '__main__':
    main()
