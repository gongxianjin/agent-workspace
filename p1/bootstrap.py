"""Owned windowless host. Double-click reopens, never restarts an existing executor."""
import argparse
import ctypes
import json
from pathlib import Path
import sys
import threading

BASE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BASE / 'p1'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=0)
    parser.add_argument('--root', type=Path, default=BASE / 'userdata')
    parser.add_argument('--no-browser', action='store_true')
    parser.add_argument('--no-workers', action='store_true')
    parser.add_argument('--background', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--ready-file', type=Path)
    args = parser.parse_args()
    app = channel = thread = None
    stage = 'imports'
    try:
        from server import Application, OwnedRoot
        from instance import InstanceChannel, request
        stage = 'root'
        root = args.root.resolve()
        if str(root).startswith('\\\\'):
            raise ValueError('Local writable data directory required')
        stage = 'initialize'
        try:
            app = Application(root, port=args.port, workers=not args.no_workers)
        except OwnedRoot:
            stage = 'reopen'
            request(root, 'probe' if args.no_browser else 'open-ui', timeout=8)
            return 0
        stage = 'channel'
        channel = InstanceChannel(root, (lambda: None) if args.no_browser else app.open_ui)
        stage = 'serve'
        thread = threading.Thread(target=app.serve)
        thread.start()
        if args.ready_file:
            stage = 'handoff'
            # Explicit diagnostic handoff only; the ordinary launcher writes no URL/token.
            with args.ready_file.open('x', encoding='utf-8') as target:
                json.dump({'origin': app.origin, 'url': app.origin + '/#bootstrap=' + app.bootstrap}, target)
        if not args.no_browser:
            stage = 'open_ui'
            app.open_ui()
        stage = 'running'
        try:
            app.shutdown_requested.wait()
        except KeyboardInterrupt:
            pass
        stage = 'shutdown'
        channel.close()
        channel = None
        app.close()
        thread.join(5)
        if thread.is_alive():
            raise RuntimeError('HTTP host did not stop')
        if args.ready_file:
            args.ready_file.write_text('{"stopped":true}', encoding='utf-8')
        return 0
    except Exception as error:
        # Never write exception text: dependency paths/output can contain user data.
        message = ('工作台无法启动或未能正常退出。请检查便携包完整性、当前用户本地目录写权限；'
            '已有实例时请等待其退出，不要删除 userdata 或强行启动第二份。'
            '使用带参数的前台诊断入口可检查启动状态。')
        try:
            root = args.root.resolve()
            if not str(root).startswith('\\\\'):
                root.mkdir(parents=True, exist_ok=True)
                diagnostic = root / 'startup-error.json'
                if not diagnostic.is_symlink() and not diagnostic.is_junction():
                    category = ('timeout' if isinstance(error, TimeoutError) else
                        'os_error' if isinstance(error, OSError) else
                        'invalid_input' if isinstance(error, ValueError) else 'internal_error')
                    codes = {}
                    for name in ('errno', 'winerror'):
                        value = getattr(error, name, None) if isinstance(error, OSError) else None
                        codes[name] = value if type(value) is int and -(2 ** 31) <= value < 2 ** 31 else None
                    diagnostic.write_text(json.dumps({'code': 'startup_failed', 'message': message,
                        'stage': stage, 'category': category, **codes}, ensure_ascii=False), encoding='utf-8')
                    message += '\n诊断记录：' + str(diagnostic)
        except OSError:
            message += '\n数据目录不可写，请将完整文件夹移至当前用户可写的本地目录。'
        if not args.no_browser:
            ctypes.windll.user32.MessageBoxW(None, message, '接力工作台', 0x10)
        elif sys.stderr:
            print(message, file=sys.stderr)
        return 1
    finally:
        if channel:
            channel.close()
        if app and not app.closed:
            app.close()
        if thread:
            thread.join(5)


if __name__ == '__main__':
    raise SystemExit(main())
