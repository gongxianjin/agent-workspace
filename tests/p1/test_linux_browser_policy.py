"""Actual Node checker launch options; fake package is not browser/sandbox proof."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


class LinuxBrowserPolicyTests(unittest.TestCase):
    def test_checker_linux_policy_requests_sandbox_without_retry_or_unsafe_flags(self):
        # Break caught: Playwright defaults to disabling Chromium's Linux OS sandbox.
        # Windows runs the actual JS with only process.platform simulated; not native proof.
        from platform_info import native_executable, target
        node = shutil.which('node')
        node = native_executable(node, target()) if node else None
        if node is None:
            self.skipTest('No trusted native Node available for free launch-policy inspection')
        with tempfile.TemporaryDirectory(prefix='relay-browser-policy-') as directory:
            root = Path(directory)
            product, package, output = root / 'product', root / 'package', root / 'output'
            for path in (product, package, output):
                path.mkdir()
            (product / 'index.html').write_text('<p>not loaded by this fixture</p>')
            (package / 'package.json').write_text('{"name":"launch-policy-fixture","main":"index.js"}')
            (package / 'index.js').write_text('''const fs = require('node:fs');
exports.chromium = {launch: async options => {
 fs.appendFileSync(process.env.RELAY_POLICY_CAPTURE, JSON.stringify(options) + '\\n');
 throw Error('Expected fixture stop; no browser started and no sandbox attested');
}};
''')
            spec = root / 'browser.json'
            spec.write_text('{"version":1,"cases":[{"name":"fixture","steps":[{"op":"noErrors"}]}]}')
            capture = root / 'launch.jsonl'
            policy = root / 'linux-platform.cjs'
            policy.write_text("Object.defineProperty(process, 'platform', {value: 'linux'});\n")
            child = subprocess.run([str(node), '--require', str(policy), str(ROOT / 'p1/checks.cjs'), str(product), str(spec),
                str(output), 'generic', str(package / 'package.json'), str(root / 'not-a-browser')],
                env=dict(os.environ, RELAY_POLICY_CAPTURE=str(capture)), capture_output=True, timeout=15)
            self.assertEqual(child.returncode, 1, child.stderr.decode())
            launches = [json.loads(line) for line in capture.read_text().splitlines()]
            self.assertEqual(len(launches), 1, 'Sandbox failure must not retry without protection')
            self.assertIs(launches[0].get('chromiumSandbox'), True)
            self.assertNotIn('--no-sandbox', launches[0].get('args', []))
            report = json.loads((output / 'report.json').read_text())
            self.assertEqual(report['assertions'], 0)
            self.assertFalse(report['cases'][0]['passed'])


if __name__ == '__main__':
    unittest.main()
