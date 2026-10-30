"""Explicit free-test boundary; never claims to have authenticated an AI client."""
from pathlib import Path
import sys


class ReadySetupFixture:
    def public(self):
        return {'client': 'fixture', 'version': 'synthetic', 'compatibility': 'supported',
            'login': 'unknown', 'dependencies': 'healthy', 'existingConfirmed': True,
            'canExecute': True, 'modelConnection': 'not_verified', 'checkedAt': None,
            'message': '仅免费测试边界：没有检测认证，没有调用模型。'}

    def snapshot(self):
        return Path(sys.executable)
