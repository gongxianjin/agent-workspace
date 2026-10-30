"""Portable downstream terms use explicit, version-bound local consent; no models."""
import hashlib
import importlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


class DistributionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='条款 consent ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.text = 'Windows binary downstream conditions\n仅约束 Microsoft Distributable Code。\n'
        self.document = self.root / 'DISTRIBUTION-TERMS.txt'
        self.document.write_bytes(self.text.encode('utf-8'))
        self.checksum = hashlib.sha256(self.document.read_bytes()).hexdigest()
        self.write_manifest()

    def write_manifest(self):
        (self.root / 'portable.json').write_text(json.dumps({'format': 2, 'distributionTerms': {
            'path': 'DISTRIBUTION-TERMS.txt', 'sha256': self.checksum}}), encoding='utf-8')

    def setup_status(self):
        environment = importlib.import_module('environment')
        return environment.SetupStatus(self.root, paths=environment.source_or_bundle(ROOT),
            discover=lambda: self.fail('Reviewing or accepting terms must not discover or execute Claude'))

    def test_unaccepted_terms_block_detection_download_and_execution_without_creating_data(self):
        setup = self.setup_status()
        self.addCleanup(setup.close)
        public = setup.public()
        self.assertIn('distributionTerms', public, 'New portable bundles must expose required downstream consent')
        self.assertFalse(public['distributionTerms']['accepted'])
        self.assertEqual(public['distributionTerms']['text'], self.text)
        self.assertEqual(public['distributionTerms']['sha256'], self.checksum)
        self.assertFalse((self.root / 'userdata').exists(), 'Reading terms must not write a receipt')
        self.assertFalse(setup.recheck()['canExecute'])
        with self.assertRaisesRegex(ValueError, '条款'):
            setup.prepare_browser(True)
        with self.assertRaisesRegex(ValueError, '条款'):
            setup.confirm_existing()
        with self.assertRaisesRegex(ValueError, '条款'):
            setup.snapshot()
        self.assertFalse((self.root / 'userdata').exists())

    def test_acceptance_is_explicit_hash_bound_persistent_and_does_not_unlock_models(self):
        setup = self.setup_status()
        self.addCleanup(setup.close)
        accept = getattr(setup, 'accept_distribution', None)
        self.assertTrue(callable(accept), 'The current displayed document needs an explicit acceptance operation')
        for confirmed, checksum in ((False, self.checksum), (True, '0' * 64), (1, self.checksum)):
            with self.subTest(confirmed=confirmed, checksum=checksum), self.assertRaises(ValueError):
                accept(confirmed, checksum)
        self.assertFalse((self.root / 'userdata').exists())
        state = accept(True, self.checksum)
        self.assertTrue(state['distributionTerms']['accepted'])
        self.assertFalse(state['canExecute'], 'Terms consent is not dependency, login or model validation')
        receipt = self.root / 'userdata/distribution-consent.json'
        record = json.loads(receipt.read_text(encoding='utf-8'))
        self.assertEqual(record['sha256'], self.checksum)
        self.assertEqual(set(record), {'version', 'sha256', 'acceptedAt'})
        reopened = self.setup_status()
        self.addCleanup(reopened.close)
        self.assertTrue(reopened.public()['distributionTerms']['accepted'])
        self.document.write_text(self.text + 'Updated conditions.\n', encoding='utf-8')
        self.checksum = hashlib.sha256(self.document.read_bytes()).hexdigest()
        self.write_manifest()
        updated = self.setup_status()
        self.addCleanup(updated.close)
        self.assertFalse(updated.public()['distributionTerms']['accepted'], 'Consent to old text cannot cover new terms')

    def test_changed_or_outside_document_is_never_accepted_and_unknown_receipt_is_preserved(self):
        setup = self.setup_status()
        self.addCleanup(setup.close)
        accept = getattr(setup, 'accept_distribution', None)
        self.assertTrue(callable(accept), 'Consent must revalidate the pinned document before persisting')
        self.document.write_text('changed after display', encoding='utf-8')
        with self.assertRaises(ValueError):
            accept(True, self.checksum)
        self.assertFalse((self.root / 'userdata').exists())
        self.document.write_bytes(self.text.encode('utf-8'))
        directory = self.root / 'userdata'
        directory.mkdir()
        receipt = directory / 'distribution-consent.json'
        receipt.write_bytes(b'unknown pre-existing data')
        with self.assertRaises(ValueError):
            accept(True, self.checksum)
        self.assertEqual(receipt.read_bytes(), b'unknown pre-existing data')
        (self.root / 'portable.json').write_text(json.dumps({'distributionTerms': {
            'path': '../outside.txt', 'sha256': self.checksum}}), encoding='utf-8')
        with self.assertRaises(ValueError):
            self.setup_status()

    def test_source_and_legacy_bundles_do_not_invent_a_new_acceptance(self):
        (self.root / 'portable.json').write_text('{"format":1}', encoding='utf-8')
        setup = self.setup_status()
        self.addCleanup(setup.close)
        self.assertEqual(setup.public().get('distributionTerms'), {'required': False, 'accepted': False})
        self.assertFalse((self.root / 'userdata').exists())


if __name__ == '__main__':
    unittest.main()
