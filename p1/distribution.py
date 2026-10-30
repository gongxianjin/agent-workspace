"""Version-bound local downstream consent, separate from model or project authority."""
import hashlib
import json
import math
import os
from pathlib import Path
import re
import tempfile
import threading
import time

from browser_setup import local_path


class DistributionTerms:
    def __init__(self, root):
        self.root = Path(root).resolve()
        self.lock = threading.RLock()
        self.checksum = None
        manifest = self.root / 'portable.json'
        if not manifest.is_file():
            return
        if manifest.stat().st_size > 8 * 1024 * 1024:
            raise ValueError('发行清单过大。')
        data = json.loads(manifest.read_text(encoding='utf-8'))
        if 'distributionTerms' not in data:
            return
        entry = data['distributionTerms']
        if (not isinstance(entry, dict) or set(entry) != {'path', 'sha256'}
                or entry['path'] != 'DISTRIBUTION-TERMS.txt'
                or not isinstance(entry['sha256'], str) or not re.fullmatch('[a-f0-9]{64}', entry['sha256'])):
            raise ValueError('发行条款清单无效。')
        self.checksum = entry['sha256']
        self._text()

    def _text(self):
        document = local_path(self.root, 'DISTRIBUTION-TERMS.txt')
        if not document.is_file() or not 0 < document.stat().st_size <= 64 * 1024:
            raise ValueError('发行条款文件缺失或过大。')
        raw = document.read_bytes()
        if hashlib.sha256(raw).hexdigest() != self.checksum:
            raise ValueError('发行条款内容已改变，请重新解压完整包。')
        return raw.decode('utf-8')

    def _receipt(self):
        receipt = local_path(self.root, 'userdata/distribution-consent.json')
        if not receipt.exists():
            return None
        if not receipt.is_file() or receipt.stat().st_size > 4096:
            raise ValueError('已有条款确认记录无效；不会覆盖未知数据。')
        try:
            record = json.loads(receipt.read_text(encoding='utf-8'))
            if (not isinstance(record, dict) or set(record) != {'version', 'sha256', 'acceptedAt'}
                    or type(record['version']) is not int or record['version'] != 1
                    or not isinstance(record['sha256'], str) or not re.fullmatch('[a-f0-9]{64}', record['sha256'])
                    or type(record['acceptedAt']) not in (int, float)
                    or not math.isfinite(record['acceptedAt']) or record['acceptedAt'] <= 0):
                raise ValueError('Invalid consent receipt')
            return record
        except (ValueError, UnicodeError, TypeError, RecursionError) as error:
            raise ValueError('已有条款确认记录无效；不会覆盖未知数据。') from error

    def public(self):
        if self.checksum is None:
            return {'required': False, 'accepted': False}
        with self.lock:
            text = self._text()
            try:
                record = self._receipt()
            except ValueError:
                record = None
            return {'required': True, 'accepted': bool(record and record['sha256'] == self.checksum),
                'sha256': self.checksum, 'text': text}

    def require(self):
        state = self.public()
        if state['required'] and not state['accepted']:
            raise ValueError('请先阅读并明确同意当前发行条款；未确认前不执行依赖检测、浏览器准备或模型。')

    def accept(self, confirmed, checksum):
        with self.lock:
            if self.checksum is None or confirmed is not True or checksum != self.checksum:
                raise ValueError('必须明确同意页面展示的当前发行条款。')
            self._text()
            record = self._receipt()
            if record and record['sha256'] == self.checksum:
                return
            receipt = local_path(self.root, 'userdata/distribution-consent.json')
            receipt.parent.mkdir(parents=True, exist_ok=True)
            raw = json.dumps({'version': 1, 'sha256': self.checksum, 'acceptedAt': time.time()}).encode('utf-8')
            temporary = None
            try:
                with tempfile.NamedTemporaryFile(prefix='.consent-', dir=receipt.parent, delete=False) as stream:
                    temporary = Path(stream.name)
                    stream.write(raw)
                    stream.flush()
                    os.fsync(stream.fileno())
                # Re-read an existing target before replacing only a recognized old receipt.
                if self._receipt() != record:
                    raise ValueError('条款确认记录已变化，请重新读取。')
                if record is None:
                    temporary.rename(receipt)
                else:
                    temporary.replace(receipt)
            finally:
                if temporary and temporary.exists():
                    temporary.unlink()
