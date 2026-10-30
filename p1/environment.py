"""Trusted bundle paths and discovery of the user's native Claude executable."""
from dataclasses import dataclass, replace
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import sys
import copy
import re
import tempfile
import threading
import time


@dataclass(frozen=True)
class RuntimePaths:
    pythonConsole: Path
    pythonWindowed: Path
    node: Path
    reason: Path
    playwright: Path
    chromium: Path


def file_hash(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def bundle_file(root, relative):
    if not isinstance(relative, str) or '\\' in relative or ':' in relative:
        raise ValueError('Invalid bundle path')
    parts = PurePosixPath(relative).parts
    if not parts or relative.startswith('/') or any(part in ('.', '..') for part in parts):
        raise ValueError('Invalid bundle path')
    current = root
    for part in parts:
        current = current / part
        if current.is_symlink() or (current.exists() and current.is_junction()):
            raise ValueError('Bundle links are not supported')
    resolved = current.resolve()
    if not resolved.is_relative_to(root) or not resolved.is_file():
        raise ValueError('Missing or outside bundle dependency')
    return resolved


def resolve_environment(bundle_root):
    root = Path(bundle_root).resolve()
    manifest = root / 'portable.json'
    if not manifest.is_file() or manifest.stat().st_size > 8 * 1024 * 1024:
        raise ValueError('Missing or oversized portable manifest')
    try:
        data = json.loads(manifest.read_text(encoding='utf-8'))
        if data['format'] not in (1, 2) or data['platform'] != 'win32-x64':
            raise ValueError('Unsupported portable platform')
        first_use = data['format'] == 2
        expected = set(RuntimePaths.__dataclass_fields__) - ({'chromium'} if first_use else set())
        if set(data['files']) != expected:
            raise ValueError('Incomplete dependency manifest')
        if first_use and data.get('browser') != {'mode': 'first-use', 'version': '145.0.7632.6', 'revision': '1208'}:
            raise ValueError('Unsupported first-use browser')
        paths = {}
        for name, entry in data['files'].items():
            path = bundle_file(root, entry['path'])
            if file_hash(path) != entry['sha256']:
                raise ValueError('Bundle integrity check failed')
            paths[name] = path
        for entry in data.get('inventory', []):
            if file_hash(bundle_file(root, entry['path'])) != entry['sha256']:
                raise ValueError('Bundle integrity check failed')
        if first_use:
            from browser_setup import browser_executable
            paths['chromium'] = browser_executable(root)
        return RuntimePaths(**paths)
    except (KeyError, TypeError, OSError, json.JSONDecodeError) as error:
        raise ValueError('Invalid portable manifest or dependency') from error


def native_executable(path):
    try:
        path = Path(path)
        if not path.is_absolute() or path.suffix.lower() != '.exe' or not path.is_file():
            return None
        with path.open('rb') as source:
            if source.read(2) != b'MZ':
                return None
        return path.resolve()
    except (OSError, ValueError):
        return None


def discover_claude(environ=None):
    env = os.environ if environ is None else environ
    candidates = []
    for directory in env.get('PATH', '').split(os.pathsep):
        directory = directory.strip('"')
        if directory and Path(directory).is_absolute():
            candidates.append(Path(directory) / 'claude.exe')
    if env.get('USERPROFILE'):
        candidates.append(Path(env['USERPROFILE']) / '.local/bin/claude.exe')
    if env.get('APPDATA'):
        candidates.append(Path(env['APPDATA']) / 'npm/node_modules/@anthropic-ai/claude-code/bin/claude.exe')
    return next((found for candidate in candidates if (found := native_executable(candidate))), None)


def development_environment(root):
    """Explicit source-tree diagnostic mode; never used when portable.json exists."""
    root = Path(root).resolve()
    python = Path(sys.executable).with_name('python.exe')
    appdata = Path(os.environ.get('APPDATA', ''))
    local = Path(os.environ.get('LOCALAPPDATA', ''))
    return RuntimePaths(python, python.with_name('pythonw.exe'),
        Path(shutil.which('node.exe') or shutil.which('node') or 'node.exe').absolute(),
        root.parent / 'reason-network-v0.10.8-portable/extracted/reason-network.exe',
        appdata / 'npm/node_modules/playwright/package.json',
        local / 'ms-playwright/chromium-1208/chrome-win64/chrome.exe')


def source_or_bundle(root):
    root = Path(root).resolve()
    return resolve_environment(root) if (root / 'portable.json').exists() or (root / 'vendor').exists() else development_environment(root)


PROBE_COMMANDS = (('--version',), ('--help',), ('auth', 'status', '--json'))
REQUIRED_FLAGS = ('--tools', '--permission-mode', '--strict-mcp-config', '--mcp-config',
    '--allowedTools', '--output-format', '--verbose', '--include-partial-messages')


def dependency_health(root, paths):
    """Run owned local checks only; never launches Claude or a reason daemon."""
    from checks import CheckRunner
    from winjob import JobProcess
    for executable, arguments in (
            (paths.pythonConsole, ['-I', '-c', 'import sqlite3,ctypes,_winapi,http.server,ssl']),
            (paths.reason, ['--version'])):
        child = JobProcess([str(executable), *arguments], root, output_limit=16 * 1024)
        try:
            result = child.wait(10)
        finally:
            child.close()
        if result['exitCode'] != 0 or not result['treeStopped'] or result['timedOut'] or result['outputLimited']:
            return False
    with tempfile.TemporaryDirectory(prefix='relay-health-') as directory:
        workspace = Path(directory)
        (workspace / 'product').mkdir()
        (workspace / 'tests').mkdir()
        (workspace / 'product/index.html').write_text('<!doctype html><p id="health">relay-local-health</p>', encoding='utf-8')
        (workspace / 'tests/browser.json').write_text(json.dumps({'version': 1, 'cases': [
            {'name': 'owned local dependency health', 'steps': [{'op': 'text', 'selector': '#health', 'value': 'relay-local-health'}]}]}), encoding='utf-8')
        result = CheckRunner(paths=paths).run(workspace, workspace / 'evidence', timeout=30)
        return result['exitCode'] == 0 and result['assertions'] >= 1


def probe_client(executable, argv, output_limit=128 * 1024):
    if tuple(argv) not in PROBE_COMMANDS:
        raise ValueError('Only fixed non-model detection commands are allowed')
    from winjob import JobProcess
    env = dict(os.environ)
    for name in list(env):
        if name == 'CLAUDECODE' or name.startswith('CLAUDE_CODE_') or name == 'CLAUDE_PID':
            env.pop(name, None)
    with tempfile.TemporaryDirectory(prefix='relay-detect-') as directory:
        child = JobProcess([str(executable), *argv], directory, env=env, output_limit=output_limit)
        try:
            result = child.wait(10)
        finally:
            child.close()
    if result['timedOut'] or result['outputLimited'] or not result['treeStopped']:
        raise ValueError('Client detection timed out or exceeded its output limit')
    return {'exitCode': result['exitCode'], 'stdout': result['stdout'],
        'outputBytes': sum(len(result[name].encode('utf-8')) for name in ('stdout', 'stderr'))}


class SetupStatus:
    """Cached local checks. Public reads never execute a CLI or expose raw output."""
    def __init__(self, root, paths=None, discover=None, probe=None):
        self.root = Path(root).resolve()
        self.paths = paths or source_or_bundle(self.root)
        self.discover = discover or discover_claude
        self.probe = probe
        self.lock = threading.RLock()
        self.detecting = threading.Lock()
        self.candidate = None
        from distribution import DistributionTerms
        self.distribution = DistributionTerms(self.root)
        from browser_setup import BrowserSetup, browser_executable
        self.browser = BrowserSetup(self.root, required=self.paths.chromium == browser_executable(self.root),
            check=lambda executable: dependency_health(self.root, replace(self.paths, chromium=executable)))
        self.state = {'client': 'unchecked', 'version': None, 'compatibility': 'unchecked',
            'login': 'unknown', 'dependencies': 'unchecked', 'checkedAt': None,
            'existingConfirmed': False, 'canExecute': False, 'modelConnection': 'not_verified',
            'message': '请先检测本机 Claude；检测不调用模型，不代表有额度或模型可达。'}

    def public(self):
        with self.lock:
            terms = self.distribution.public()
            state = {**copy.deepcopy(self.state), 'browser': self.browser.public(), 'distributionTerms': terms}
            if terms['required'] and not terms['accepted']:
                state.update(canExecute=False, message='请先阅读并明确同意当前发行条款；拒绝时可以退出工作台。')
            return state

    def accept_distribution(self, confirmed, checksum):
        self.distribution.accept(confirmed, checksum)
        return self.public()

    def prepare_browser(self, confirmed):
        self.distribution.require()
        if not self.detecting.acquire(blocking=False):
            raise ValueError('检测正在进行，请等待后再准备浏览器。')
        try:
            result = self.browser.start(confirmed)
            with self.lock:
                self.state.update(canExecute=False, existingConfirmed=False, dependencies='missing')
            return {**self.public(), 'browser': result}
        finally:
            self.detecting.release()

    def close(self):
        self.browser.close()

    def recheck(self):
        try:
            self.distribution.require()
        except ValueError:
            return self.public()
        if not self.detecting.acquire(blocking=False):
            raise ValueError('检测已在进行，请等待结束。')
        state = {**self.public(), 'client': 'missing', 'version': None, 'compatibility': 'unchecked',
            'login': 'unknown', 'dependencies': 'missing', 'existingConfirmed': False,
            'canExecute': False, 'checkedAt': time.time(), 'message': '未找到 Claude Code 原生程序，请按官方步骤安装后重新检测。'}
        with self.lock:
            self.candidate = None
            self.state = state.copy()
        candidate = None
        remaining = 128 * 1024

        def checked_probe(arguments):
            nonlocal remaining
            if remaining <= 0:
                raise ValueError('Total detection output limit reached')
            result = self.probe(candidate, arguments) if self.probe else probe_client(candidate, arguments, output_limit=remaining)
            size = result.get('outputBytes', sum(len(result.get(name, '').encode('utf-8')) for name in ('stdout', 'stderr')))
            if type(size) is not int or size < 0 or size > remaining:
                raise ValueError('Total detection output limit reached')
            remaining -= size
            return result

        try:
            browser_ready = True
            if self.browser.required:
                from browser_setup import verified_browser
                try:
                    browser_ready = self.browser.public()['status'] == 'ready' and verified_browser(self.root) is not None
                except (ValueError, OSError):
                    browser_ready = False
            if browser_ready and all(getattr(self.paths, name).is_file() for name in RuntimePaths.__dataclass_fields__):
                try:
                    state['dependencies'] = 'healthy' if dependency_health(self.root, self.paths) else 'unhealthy'
                except Exception:
                    state['dependencies'] = 'unhealthy'
            candidate = self.discover()
            if candidate:
                candidate = Path(candidate).resolve()
                state['client'] = 'found'
                version = checked_probe(['--version'])
                match = re.fullmatch(r'\s*(\d+\.\d+\.\d+)(?:\s+\(Claude Code\))?\s*', version.get('stdout', ''))
                if version.get('exitCode') == 0 and match:
                    state['version'] = match[1]
                help_result = checked_probe(['--help'])
                help_text = help_result.get('stdout', '')
                state['compatibility'] = ('supported' if state['version'] and help_result.get('exitCode') == 0
                    and all(flag in help_text for flag in REQUIRED_FLAGS) and 'stream-json' in help_text else 'unsupported')
                state['message'] = '当前 CLI 缺少必需参数或版本不可识别，请按官方说明更新。'
                if state['compatibility'] == 'supported':
                    auth = checked_probe(['auth', 'status', '--json'])
                    # Unknown schema/provider remains unknown; never echo diagnostics or infer from exit 0.
                    state['login'] = self.login_status(auth)
                    state['message'] = ('本机登录状态已报告；实际模型、额度及工具接入未验证。' if state['login'] == 'logged_in' else
                        '尚未登录，请自行运行 claude auth login，然后重新检测。' if state['login'] == 'logged_out' else
                        '登录状态无法按已核实契约确认。可检查本机配置，或明确确认使用已有配置继续；实际接入仍未验证。')
            if state['dependencies'] != 'healthy':
                state['message'] = ('请先明确确认准备固定版本测试浏览器；下载后再重新检测，不会自动调用模型。'
                    if not browser_ready else '工作台依赖不完整，请重新解压完整便携包；不会安装软件或回退到另一套全局依赖。')
            state['canExecute'] = bool(candidate and state['compatibility'] == 'supported'
                and state['dependencies'] == 'healthy' and state['login'] == 'logged_in')
        except Exception:
            state.update(canExecute=False, version=None, login='unknown', compatibility='unknown', message='本机检测失败／超时；请检查安装，更新或登录后重新检测。原始输出不公开。')
        finally:
            with self.lock:
                self.candidate = candidate
                self.state = state
            self.detecting.release()
        return self.public()

    @staticmethod
    def login_status(result):
        try:
            value = json.loads(result.get('stdout', ''))
            if isinstance(value, dict) and result.get('exitCode') == 1:
                return 'logged_out'
        except (ValueError, TypeError, RecursionError):
            pass
        # The complete positive JSON schema/provider contract is not verified.
        return 'unknown'

    def confirm_existing(self):
        self.distribution.require()
        with self.lock:
            if not (self.candidate and self.state['login'] == 'unknown'
                    and self.state['compatibility'] == 'supported' and self.state['dependencies'] == 'healthy'):
                raise ValueError('只有客户端／参数／依赖已检测且登录未知时，才可确认已有配置。')
            self.state.update(existingConfirmed=True, canExecute=True)
        return self.public()

    def snapshot(self):
        self.distribution.require()
        with self.lock:
            if not self.state['canExecute'] or not self.candidate or not self.candidate.is_file():
                raise ValueError('本机 Claude 尚未就绪，请先完成安装／登录／重新检测或显式确认已有配置。')
            return self.candidate
