"""Explicit first-use download of one fixed browser; never a shell/package installer."""
import copy
import errno
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import ssl
import stat
import tempfile
import threading
import time
from urllib.parse import urlsplit
from concurrent.futures import ThreadPoolExecutor, as_completed
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener
import zipfile

BROWSER = {'version': '145.0.7632.6', 'revision': '1208',
    'url': 'https://cdn.playwright.dev/builds/cft/145.0.7632.6/win64/chrome-win64.zip',
    'termsUrl': 'https://www.google.com/chrome/terms/',
    'sha256': '08476e6fe550ccbc0d11d1ab030292fc4e32d4987400c77b2d1905a6bd13b4f6', 'bytes': 181196793}
ACTIVE = frozenset(('downloading', 'verifying', 'extracting', 'checking'))


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def local_path(root, relative):
    root = Path(root).resolve()
    parts = PurePosixPath(relative).parts
    if not parts or relative.startswith('/') or '\\' in relative or ':' in relative or '..' in parts:
        raise ValueError('Unsafe browser path')
    current = root
    for part in parts:
        current = current / part
        if current.is_symlink() or current.is_junction():
            raise ValueError('Browser links are not supported')
    if not current.resolve().is_relative_to(root):
        raise ValueError('Browser outside local root')
    return current


def browser_directory(root):
    return local_path(root, 'userdata/browser/chromium-1208')


def browser_executable(root):
    return local_path(root, 'userdata/browser/chromium-1208/chrome-win64/chrome.exe')


def inventory(directory):
    entries = []
    for path in sorted(directory.rglob('*')):
        relative = path.relative_to(directory).as_posix()
        local_path(directory, relative)
        if path.is_file():
            entries.append({'path': relative, 'bytes': path.stat().st_size, 'sha256': digest(path)})
        if len(entries) > 10000:
            raise ValueError('Browser inventory exceeds limit')
    return entries


def verified_browser(root):
    directory = browser_directory(root)
    executable = browser_executable(root)
    if not directory.exists():
        return None
    receipt = local_path(directory, 'receipt.json')
    if not receipt.is_file() or receipt.stat().st_size > 4 * 1024 * 1024:
        raise ValueError('Browser receipt missing or oversized')
    try:
        record = json.loads(receipt.read_text(encoding='utf-8'))
        actual = [entry for entry in inventory(directory) if entry['path'] != 'receipt.json']
        if record['version'] != BROWSER['version'] or record['files'] != actual or not executable.is_file():
            raise ValueError('Browser integrity check failed')
    except (KeyError, TypeError, json.JSONDecodeError) as error:
        raise ValueError('Invalid browser receipt') from error
    return executable


def allowed_url(url):
    parsed = urlsplit(url)
    return (parsed.scheme == 'https' and parsed.hostname in ('cdn.playwright.dev', 'storage.googleapis.com')
        and parsed.port in (None, 443) and parsed.username is None and parsed.password is None
        and not parsed.fragment)


class OfficialRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        if not allowed_url(newurl):
            raise ValueError('Browser redirect outside official HTTPS source')
        return super().redirect_request(request, fp, code, message, headers, newurl)


class BrowserHTTPError(ValueError):
    pass


class BrowserPathError(ValueError):
    pass


def preparation_failure(error, stage, cancelled):
    reason = error.reason if isinstance(error, URLError) else error
    if cancelled:
        code = 'cancelled'
    elif isinstance(error, BrowserPathError):
        code = 'path_too_long'
    elif isinstance(error, (BrowserHTTPError, HTTPError)):
        code = 'http_error'
    elif isinstance(reason, ssl.SSLError):
        code = 'tls_error'
    elif stage == 'download' and isinstance(reason, TimeoutError):
        code = 'network_timeout'
    elif isinstance(error, URLError):
        code = 'network_error'
    elif isinstance(error, OSError) and (error.errno == errno.ENOSPC or getattr(error, 'winerror', None) == 112):
        code = 'disk_full'
    elif isinstance(error, PermissionError):
        code = 'permission_denied'
    elif isinstance(error, OSError):
        code = 'storage_error'
    else:
        code = {'download': 'download_invalid', 'verify': 'archive_integrity',
            'extract': 'archive_invalid', 'check': 'health_failed'}.get(stage, 'preparation_failed')
    messages = {
        'cancelled': '浏览器准备已取消；临时内容不会作为可用浏览器安装。',
        'path_too_long': '工作台目录过深，浏览器路径超过当前支持的Windows长度。请正常退出，把整个工作台移至更短的本地可写目录，再明确重试；不要修改系统长路径设置。',
        'http_error': '官方源返回HTTP拒绝响应；请检查网络访问限制，稍后明确重试。',
        'tls_error': '官方源TLS连接或证书验证失败；无法判断中断发生在哪一跳。请检查本机网络、时间和证书，不要关闭TLS验证。',
        'network_timeout': '官方源连接或读取超时；请检查网络连通性后明确重试。',
        'network_error': '连接官方源失败；请检查DNS、网络及已有网络设置后明确重试。',
        'disk_full': '本地磁盘空间不足；请释放空间后明确重试。',
        'permission_denied': '本地文件访问被拒绝；请检查当前用户对工作台目录的写入权限，不要以提权代替检查。',
        'storage_error': '本地文件操作失败；请检查磁盘、目录和文件占用情况后明确重试。',
        'download_invalid': '下载内容或字节范围不符合固定要求；未安装，不要更改来源或摘要绕过校验。',
        'archive_integrity': '完整归档的大小或SHA256校验失败；未执行下载内容，请检查网络后明确重试。',
        'archive_invalid': '归档格式或路径安全校验失败；未安装，请保留固定版本和摘要，不要手工放入未校验文件。',
        'health_failed': '已校验浏览器未通过本地运行自检；请检查系统兼容性或程序拦截情况后明确重试。',
        'preparation_failed': '浏览器准备失败；请检查网络、空间和当前用户目录权限后明确重试。',
    }
    return code, messages[code] + ' 没有安装到系统、没有自动重试；已有未知目录不会覆盖。'


def download(url, destination, progress, cancelled):
    if not allowed_url(url):
        raise ValueError('Browser requires official HTTPS source')
    size, parts = BROWSER['bytes'], 8
    if not isinstance(size, int) or not 0 < size <= 512 * 1024 * 1024:
        raise ValueError('Browser download size is not pinned')
    deadline = time.monotonic() + 900
    failed, lock = threading.Event(), threading.Lock()
    counts = [0] * parts
    def boundary():
        if cancelled.is_set() or failed.is_set() or time.monotonic() >= deadline:
            raise ValueError('Browser download cancelled or timed out')
    with tempfile.TemporaryDirectory(prefix='.ranges-', dir=destination.parent) as temporary:
        def request(index):
            start, end = size * index // parts, size * (index + 1) // parts - 1
            target = Path(temporary) / str(index)
            boundary()
            try:
                incoming = build_opener(OfficialRedirect()).open(Request(url,
                    headers={'Range': f'bytes={start}-{end}', 'Accept-Encoding': 'identity'}), timeout=10)
            except HTTPError as error:
                error.close()
                raise BrowserHTTPError('Official browser source refused the request') from None
            with incoming, target.open('xb') as outgoing:
                if (not allowed_url(incoming.geturl()) or incoming.status != 206
                        or incoming.headers.get('Content-Range') != f'bytes {start}-{end}/{size}'
                        or incoming.headers.get('Content-Length') != str(end-start+1)):
                    raise ValueError('Invalid browser byte range')
                count = 0
                while True:
                    boundary()
                    raw = incoming.read1(64 * 1024)
                    if not raw:
                        break
                    count += len(raw)
                    if count > end-start+1:
                        raise ValueError('Browser range exceeds limit')
                    outgoing.write(raw)
                    with lock:
                        counts[index] = count
                        progress(sum(counts))
                if count != end-start+1:
                    raise ValueError('Incomplete browser byte range')
            return target
        with ThreadPoolExecutor(max_workers=parts, thread_name_prefix='browser-range') as executor:
            pending = [executor.submit(request, index) for index in range(parts)]
            try:
                for future in as_completed(pending):
                    future.result()
            except BaseException:
                failed.set()
                for future in pending:
                    future.cancel()
                raise
        boundary()
        with destination.open('xb') as outgoing:
            for future in pending:
                boundary()
                with future.result().open('rb') as incoming:
                    shutil.copyfileobj(incoming, outgoing)


def extract(archive, destination, cancelled):
    total, members = 0, []
    with zipfile.ZipFile(archive) as source:
        if len(source.infolist()) > 10000:
            raise ValueError('Too many browser files')
        for entry in source.infolist():
            if cancelled.is_set():
                raise ValueError('Browser preparation cancelled')
            target = local_path(destination, entry.filename)
            mode = entry.external_attr >> 16
            total += entry.file_size
            if (stat.S_ISLNK(mode) or total > 2 * 1024 ** 3
                    or not entry.filename.startswith('chrome-win64/')):
                raise ValueError('Unsafe or oversized browser archive')
            if os.name == 'nt' and len(str(target).encode('utf-16-le')) // 2 >= 248:
                raise BrowserPathError('Browser destination exceeds supported Windows path length')
            members.append((entry, target))
        destination.mkdir()
        for entry, target in members:
            if cancelled.is_set():
                raise ValueError('Browser preparation cancelled')
            if entry.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.open(entry) as incoming, target.open('xb') as outgoing:
                    shutil.copyfileobj(incoming, outgoing)
    executable = destination / 'chrome-win64/chrome.exe'
    if not executable.is_file():
        raise ValueError('Browser executable missing')
    with executable.open('rb') as source:
        if source.read(2) != b'MZ':
            raise ValueError('Invalid Windows browser executable')


class BrowserSetup:
    def __init__(self, root, required=False, descriptor=None, fetch=None, check=None):
        self.root = Path(root).resolve()
        self.required = required
        self.descriptor = copy.deepcopy(BROWSER if descriptor is None else descriptor)
        self.fetch = fetch or download
        self.check = check
        self.lock = threading.RLock()
        self.cancelled = threading.Event()
        self.thread = None
        self.state = {'status': 'missing' if required else 'not_required', 'downloadedBytes': 0,
            'stage': 'idle', 'failureCode': None,
            'totalBytes': self.descriptor['bytes'], 'version': self.descriptor['version'],
            'source': self.descriptor['url'], 'termsUrl': self.descriptor['termsUrl'],
            'message': '首次点击确认后才下载测试浏览器；不会安装到系统或调用模型。'}
        if required:
            try:
                if verified_browser(self.root):
                    self.state.update(status='ready', stage='ready', message='本机已准备固定版本浏览器；真实模型接入仍未验证。')
            except (OSError, ValueError):
                self.state.update(status='failed', stage='existing', failureCode='existing_integrity',
                    message='已有浏览器校验失败；未覆盖原目录。请退出后检查或重新解压完整工作台。')

    def public(self):
        with self.lock:
            return copy.deepcopy(self.state)

    def update(self, **value):
        with self.lock:
            self.state.update(value)

    def start(self, confirmed):
        with self.lock:
            if confirmed is not True or not self.required:
                raise ValueError('必须明确确认从官方源下载浏览器并阅读适用条款。')
            if self.thread and self.thread.is_alive():
                raise ValueError('浏览器正在准备，请等待；不会启动第二个下载。')
            if browser_directory(self.root).exists():
                raise ValueError('浏览器目录已存在，不覆盖已有文件；请先检查现有目录。')
            self.cancelled.clear()
            self.state.update(status='downloading', stage='download', failureCode=None, downloadedBytes=0,
                message='正在从固定官方源下载；失败不会自动重试。')
            self.thread = threading.Thread(target=self.run, name='relay-browser-prepare')
            try:
                self.thread.start()
            except BaseException:
                self.state.update(status='failed', stage='prepare', failureCode='preparation_failed', message='无法启动浏览器准备。')
                raise
        return self.public()

    def run(self):
        stage = 'prepare'
        try:
            parent = local_path(self.root, 'userdata/browser')
            parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='.prepare-', dir=parent) as temporary:
                workspace = Path(temporary)
                incoming = workspace / 'download'
                incoming.mkdir()
                archive = incoming / 'browser.zip'
                stage = 'download'
                self.fetch(self.descriptor['url'], archive,
                    lambda count: self.update(downloadedBytes=count), self.cancelled)
                stage = 'verify'
                self.update(status='verifying', stage=stage, message='正在校验完整下载的大小及 SHA256。')
                if (self.cancelled.is_set() or archive.stat().st_size != self.descriptor['bytes']
                        or digest(archive) != self.descriptor['sha256']):
                    raise ValueError('Browser archive integrity check failed')
                stage = 'extract'
                self.update(status='extracting', stage=stage, message='正在解压到新的隔离目录。')
                extracted = workspace / 'extracted'
                extract(archive, extracted, self.cancelled)
                stage = 'check'
                self.update(status='checking', stage=stage, message='正在运行固定本地浏览器自检；不调用模型。')
                if self.cancelled.is_set() or not self.check or not self.check(extracted / 'chrome-win64/chrome.exe'):
                    raise ValueError('Browser local health check failed')
                stage = 'install'
                self.update(stage=stage)
                entries = inventory(extracted)
                (extracted / 'receipt.json').write_text(json.dumps({'version': BROWSER['version'],
                    'archiveSha256': self.descriptor['sha256'], 'files': entries}), encoding='utf-8')
                if self.cancelled.is_set():
                    raise ValueError('Browser preparation cancelled')
                extracted.rename(browser_directory(self.root))
            self.update(status='ready', stage='ready', failureCode=None,
                message='浏览器准备及本地自检完成。请重新检测 Claude；真实模型接入仍未验证。')
        except Exception as error:
            code, message = preparation_failure(error, stage, self.cancelled.is_set())
            self.update(status='failed', stage=stage, failureCode=code, message=message)

    def close(self):
        self.cancelled.set()
        if self.thread and self.thread.ident is not None:
            self.thread.join(45)
            if self.thread.is_alive():
                raise RuntimeError('Browser preparation has not stopped')
