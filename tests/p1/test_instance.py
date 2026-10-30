"""Real Windows owner-only reopen protocol; no model calls."""
import importlib
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


class InstanceTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue((ROOT / 'p1/instance.py').exists(), 'Double-click reopen requires a local owner protocol')
        self.module = importlib.import_module('instance')
        self.temp = tempfile.TemporaryDirectory(prefix='接力实例 ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_reopen_notifies_same_owner_without_transferring_capabilities(self):
        opened = threading.Event()
        owner = self.module.InstanceChannel(self.root, opened.set)
        self.addCleanup(owner.close)
        reply = self.module.request(self.root, 'probe')
        self.assertEqual(reply, {'ok': True})
        self.assertFalse(opened.is_set())
        self.assertEqual(self.module.request(self.root, 'open-ui'), {'ok': True})
        self.assertTrue(opened.wait(2))
        self.assertFalse(any(self.root.iterdir()), 'Pipe protocol must not persist URL, token or credentials')

    def test_only_fixed_protocol_and_one_owner_are_allowed(self):
        owner = self.module.InstanceChannel(self.root, lambda: None)
        self.addCleanup(owner.close)
        with self.assertRaises((ValueError, OSError)):
            self.module.request(self.root, 'execute arbitrary command')
        with self.assertRaises(OSError):
            self.module.InstanceChannel(self.root, lambda: None)

    def test_closed_or_other_scope_cannot_be_reopened(self):
        owner = self.module.InstanceChannel(self.root, lambda: None)
        owner.close()
        with self.assertRaises((OSError, TimeoutError)):
            self.module.request(self.root, 'open-ui', timeout=.15)
        with self.assertRaises((OSError, TimeoutError)):
            self.module.request(self.root / 'other', 'probe', timeout=.15)


if __name__ == '__main__':
    unittest.main()
