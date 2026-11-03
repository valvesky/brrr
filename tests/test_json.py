#!/usr/bin/python3
"""JSON compatibility and validation checks through the real configuration CLI."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

BINARY = Path(__file__).resolve().parent.parent / 'omapower'


class JsonConfig(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='omapower-json-')
        self.addCleanup(self.temp.cleanup)
        self.config = Path(self.temp.name) / 'omapower/config.json'
        self.config.parent.mkdir()
        self.env = dict(os.environ, XDG_CONFIG_HOME=self.temp.name)

    def edit(self, name='gcc', success=True):
        result = subprocess.run([str(BINARY), '--add', name], env=self.env,
                                capture_output=True, text=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertEqual(result.returncode, 1, result.stderr)

    def test_strings_and_unknown_fields_round_trip(self):
        names = ['a"b', 'a\\b', 'a\tb', 'a\rb', 'a\x01b', 'é', '工具', '🛠']
        original = dict(commands=names, other={'nested': [None, True, False, {'a\x00b': 'x\x00y'}]},
                        integer=9007199254740993)
        self.config.write_text(json.dumps(original))
        self.edit('clang')
        expected = dict(original, commands=sorted(names + ['clang']))
        self.assertEqual(json.loads(self.config.read_text()), expected)
        self.assertNotIn('\x00', self.config.read_text())
        self.config.write_text('{"commands":[],"huge":1e400,"precise":0.1234567890123456789012345}')
        self.edit()
        self.assertIn('1e400', self.config.read_text())
        self.assertIn('0.1234567890123456789012345', self.config.read_text())

    def test_last_commands_member_wins_and_nul_key_is_unrelated(self):
        self.config.write_text('{"commands":["old"],"commands":["new"],"commands\\u0000":["other"]}')
        self.edit()
        self.assertEqual(json.loads(self.config.read_text()),
                         {'commands': ['gcc', 'new'], 'commands\x00': ['other']})

    def test_invalid_documents_are_not_modified(self):
        cases = [b'', b'null', b'[]', b'{}', b'{"commands":false}',
                 b'{"commands":[1]}', b'{"commands":[""]}', b'{"commands":["a/b"]}',
                 b'{"commands":["a\\nb"]}', b'{"commands":["a\\u0000b"]}',
                 b'{"commands":["1234567890123456"]}',
                 json.dumps({'commands': ['é' * 8]}).encode(),
                 b'{"commands":["\xff"]}', b'{"commands":[]}\x00',
                 b'{"commands":["\\ud800"]}', b'{"commands":["\\udc00"]}',
                 b'{"commands":[],}', b'{"commands":[]} trailing',
                 b'{"commands":[],"x":NaN}', b'{"commands":[],"x":Infinity}',
                 b'{"commands":[],"x":01}', b'{"commands":[],"x":1.}',
                 b'{"commands":[],/*comment*/"x":1}']
        for document in cases:
            with self.subTest(document=document):
                self.config.write_bytes(document)
                self.edit(success=False)
                self.assertEqual(self.config.read_bytes(), document)

    def test_excessive_nesting_is_rejected(self):
        document = b'{"commands":[],"other":' + b'[' * 1100 + b'0' + b']' * 1100 + b'}'
        self.config.write_bytes(document)
        self.edit(success=False)
        self.assertEqual(self.config.read_bytes(), document)


if __name__ == '__main__':
    unittest.main(verbosity=2)
