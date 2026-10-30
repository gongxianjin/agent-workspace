#!/usr/bin/python3
"""为 Linux 控制台组装 native-runtime.json 资产清单（本地测试用）。

产物放 workbench/native-assets/（root 属主、无符号链接），
生成与 p1/native_environment.py 严格一致的清单：
- files: RuntimePaths 六字段 + claude，逐文件 sha256
- assetRoots: 资产根目录（互不嵌套）
- inventory: 全部实际文件（与磁盘严格一致）
"""
import hashlib
import json
import os
from pathlib import Path

WORKBENCH = Path('/root/gwx/project/agent-workspace/relay-workbench-20261027-095732-a769e9/workbench')
ASSETS = WORKBENCH / 'native-assets'
REASON_SRC = Path('/tmp/reason-0108/extracted/reason-network')
CLAUDE = Path('/root/.nvm/versions/node/v25.8.0/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe')  # 实为 Linux ELF
PYTHON = Path('/usr/bin/python3.12')
NODE = Path('/root/.nvm/versions/node/v25.8.0/bin/node')
CHROMIUM = Path('/root/.cache/ms-playwright/chromium-1217/chrome-linux64/chrome')
PLAYWRIGHT_PKG = Path('/root/gwx/sandbox/aios-sandbox-ui/node_modules/playwright/package.json')


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def place(src, relative, executable=False):
    """复制资产到清单目录；保持属主 root、常规文件、无链接。"""
    target = ASSETS / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    raw = Path(src).read_bytes()
    target.write_bytes(raw)
    mode = 0o755 if executable else 0o644
    os.chmod(target, mode)
    return target


def main():
    if ASSETS.exists():
        raise SystemExit(f'{ASSETS} 已存在；如需重建请先手动删除（保护现有清单）')
    files = {
        'pythonConsole': place(PYTHON, 'bin/python3.12', executable=True),
        'pythonWindowed': place(PYTHON, 'bin/python3.12-windowed', executable=True),  # Linux 无窗口版，用同一二进制
        'node': place(NODE, 'bin/node', executable=True),
        'reason': place(REASON_SRC, 'bin/reason-network', executable=True),
        'chromium': place(CHROMIUM, 'browser/chrome-linux64/chrome', executable=True),
        'playwright': place(PLAYWRIGHT_PKG, 'playwright/package.json', executable=False),
        'claude': place(CLAUDE, 'bin/claude', executable=True),
    }
    # 按清单校验：可执行位不能有组/其他写；playwright 目录只含 package.json
    for name, path in files.items():
        mode = path.stat().st_mode & 0o777
        if mode & 0o022:
            raise SystemExit(f'{name} 权限含组/其他写位: {oct(mode)}')
    manifest = {
        'format': 1,
        'platform': 'linux-x64',
        'files': {name: {'path': str(p), 'sha256': digest(p)} for name, p in files.items()},
        'assetRoots': [str(ASSETS)],
        'inventory': [{'path': str(p), 'sha256': digest(p)} for p in sorted(ASSETS.rglob('*')) if p.is_file()],
    }
    (WORKBENCH / 'native-runtime.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    print(f'清单已生成: {WORKBENCH / "native-runtime.json"}')
    print(f'资产: {ASSETS} ({len(manifest["inventory"])} 个文件)')
    for name, p in files.items():
        print(f'  {name:16} -> {p}')


if __name__ == '__main__':
    main()
