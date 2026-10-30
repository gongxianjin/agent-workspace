"""Linux console status with native execution gated on verified pinned assets.

原实现是无条件 fail-closed（模型操作永远禁用）。本部署版改为诚实门禁：
资产清单（native-runtime.json）逐文件 SHA256 校验 + 受管 systemd 宿主上下文
+ 真实依赖健康检查全部通过才放行 canExecute；任一缺失仍 fail-closed。
不放宽任何校验强度，只把"未验证"推进到"已验证"。
"""
import os
from pathlib import Path
import platform
import threading
import time

from environment import ExecutionSnapshot, dependency_health
from native_environment import resolve_native_environment


def _owned_host_active():
    """受管 systemd 宿主上下文是否在位（relay-console/process-test 单元内运行）。"""
    try:
        from linux_process import active_context
        active_context()
        return True
    except (OSError, ValueError, ImportError):
        return False


class LinuxSetup:
    def __init__(self, root=None):
        self.root = Path(root) if root is not None else Path(__file__).resolve().parent.parent
        self.paths = None
        self.candidate = None
        self.native_assets = 'unchecked'
        self.lock = threading.RLock()
        machine = platform.machine().lower()
        self.platform = {'x86_64': 'linux-x64', 'amd64': 'linux-x64',
            'aarch64': 'linux-arm64', 'arm64': 'linux-arm64'}.get(machine, 'linux-unsupported')
        self.checked_at = None
        self.dependencies = 'unchecked'
        self.host_active = False

    def public(self):
        with self.lock:
            return self._public()

    def _public(self):
        ready = (self.native_assets == 'verified' and self.host_active
            and self.dependencies == 'healthy')
        blockers = []
        if not self.host_active:
            blockers.append('execution')
        if self.native_assets != 'verified':
            blockers.append('native_dependencies')
        if self.dependencies != 'healthy':
            blockers.append('dependency_health')
        if os.getuid() == 0 and self.dependencies != 'healthy':
            blockers.append('root_browser_execution')
        blockers.append('model_connection')
        return {'platform': self.platform,
            'client': 'found' if self.candidate else 'missing',
            'version': None,
            'compatibility': 'supported' if ready else 'unchecked',
            'login': 'unknown',
            'dependencies': 'healthy' if ready else 'missing',
            'nativeAssets': self.native_assets,
            'existingConfirmed': ready, 'canExecute': ready, 'checkedAt': self.checked_at,
            'modelConnection': 'not_verified', 'blockers': blockers,
            'browser': ({'status': 'ready', 'stage': 'ready', 'downloadedBytes': 0,
                'totalBytes': None, 'version': 'pinned-native', 'source': 'native-runtime.json',
                'termsUrl': None,
                'message': '测试浏览器来自本机固定资产清单，逐文件校验通过。'} if ready else
                {'status': 'unavailable', 'stage': 'blocked', 'downloadedBytes': 0,
                'totalBytes': None, 'version': None, 'source': None, 'termsUrl': None,
                'message': 'Linux原生浏览器清单未就绪；不会下载Windows组件或关闭沙箱。'}),
            'distributionTerms': {'required': False, 'accepted': True},
            'message': ('Linux执行链已验证：清单资产、受管宿主与依赖健康检查全部通过；模型连接仍需真实任务验证。' if ready else
                'Linux控制台可用；执行门禁未全部通过（见blockers），模型操作保持禁用。')}

    def _inspect_assets(self):
        self.paths = None
        self.candidate = None
        manifest = self.root / 'native-runtime.json'
        self.native_assets = 'invalid' if manifest.exists() or manifest.is_symlink() else 'missing'
        try:
            assets = resolve_native_environment(self.root)
        except (ValueError, OSError):
            return
        self.candidate = ExecutionSnapshot(assets.claude, assets.paths, assets.reason_sha256)
        self.paths = self.candidate.paths
        self.native_assets = 'verified'

    def recheck(self):
        with self.lock:
            self.checked_at = time.time()
            self._inspect_assets()
            self.host_active = _owned_host_active()
            self.dependencies = 'unchecked'
            if self.native_assets == 'verified' and self.host_active and self.paths is not None:
                try:
                    self.dependencies = 'healthy' if dependency_health(self.root, self.paths) else 'unhealthy'
                except Exception:
                    self.dependencies = 'unhealthy'
            return self._public()

    def confirm_existing(self):
        # 与 Windows 语义一致：仅当检测通过且登录未知时允许显式确认。
        with self.lock:
            if not (self.candidate and self.dependencies == 'healthy' and self.host_active):
                raise ValueError('Linux原生依赖或受管宿主未验证通过，不能确认已有配置。')
            return self._public()

    def prepare_browser(self, confirmed):
        raise ValueError('Linux浏览器已由本机清单提供，无需下载；不会自动接受条款。')

    def accept_distribution(self, confirmed, checksum):
        raise ValueError('当前诊断控制台不提供可接受的Linux发行条款；未记录同意。')

    def snapshot(self):
        return self.execution_snapshot().executable

    def execution_snapshot(self):
        """执行权限 = 清单资产校验 + 受管宿主 + 依赖健康；三者齐备才发快照。"""
        with self.lock:
            self._inspect_assets()
            if self.native_assets != 'verified':
                raise ValueError('Linux原生依赖清单缺失或校验失败；未授权执行。')
            if not _owned_host_active():
                raise ValueError('受管宿主上下文未激活；未授权执行。')
            if self.dependencies != 'healthy':
                raise ValueError('本地依赖健康检查未通过；未授权执行。')
            return self.candidate

    def close(self):
        pass
