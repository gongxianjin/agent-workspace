"""Run the explicit file:// simulation suite using trusted runtime paths, no models."""
import os
from pathlib import Path
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'p1'))
from environment import source_or_bundle
from winjob import JobProcess


def main():
    paths = source_or_bundle(ROOT)
    output = Path(os.environ.get('RELAY_STATIC_OUTPUT') or
        ROOT / 'dist/p1' / ('static-browser-' + time.strftime('%Y%m%d-%H%M%S'))).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = {**os.environ, 'RELAY_STATIC_OUTPUT': str(output)}
    child = JobProcess([str(paths.node), str(ROOT / 'tests/browser.cjs'),
        str(paths.playwright), str(paths.chromium)], ROOT, env=env)
    try:
        result = child.wait(120)
    finally:
        child.close()
    print(result['stdout'])
    print(result['stderr'])
    print('Evidence: ' + str(output))
    return result['exitCode'] if result['treeStopped'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
