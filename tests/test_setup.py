#!/usr/bin/python3
"""Exercise setup with fake package, build and systemd tools; never elevate."""
import json
import os
import shlex
from pathlib import Path
import subprocess
import tempfile
import unittest


class Setup(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='omapower setup %$-')
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = self.base / 'checkout'
        self.project.mkdir()
        self.tools = self.base / 'tools'
        self.tools.mkdir()
        self.log = self.base / 'calls'
        self.missing = self.base / 'missing'
        self.missing.write_text('')
        self.cancel = self.base / 'cancel'
        self.tool('pacman', '''if [[ $1 == -Q ]]; then
  ! grep -qx -- "$2" "$MISSING"
else
  echo "pacman $*" >> "$CALLS"
  : > "$MISSING"
fi
''')
        self.tool('pkexec', '''echo "pkexec $*" >> "$CALLS"
[[ ! -e $CANCEL ]] || exit 126
exec "$@"
''')
        self.tool('make', '''echo "make $*" >> "$CALLS"
printf '#!/bin/sh\\nexit 0\\n' > "${OMAPOWER_OUT:-$PROJECT/omapower}"
''')
        self.tool('systemctl', 'echo "systemctl $*" >> "$CALLS"\n')
        source = (Path(__file__).resolve().parent.parent / 'setup.sh').read_text()
        # Only the test copy resolves privileged commands to harmless fixtures.
        source = source.replace('/usr/bin/pacman', shlex.quote(str(self.tools / 'pacman')))
        source = source.replace('/usr/bin/pkexec', shlex.quote(str(self.tools / 'pkexec')))
        self.script = self.project / 'setup.sh'
        self.script.write_text(source)
        self.env = dict(os.environ, PATH=str(self.tools) + ':' + os.environ['PATH'],
                        HOME=str(self.base), XDG_DATA_HOME=str(self.base / 'data'),
                        XDG_CONFIG_HOME=str(self.base / 'config'), CALLS=str(self.log),
                        MISSING=str(self.missing), CANCEL=str(self.cancel), PROJECT=str(self.project))

    def tool(self, name, body):
        path = self.tools / name
        path.write_text('#!/bin/bash\nset -euo pipefail\n' + body)
        path.chmod(0o755)

    def run_setup(self, option, expected=0):
        result = subprocess.run(['bash', str(self.script), option], env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stderr)
        return result.stdout

    def test_check_has_no_side_effects(self):
        self.missing.write_text('glib2\npower-profiles-daemon\n')
        self.assertEqual(json.loads(self.run_setup('--check')),
                         dict(ready=False, supported=True, missing=['glib2', 'power-profiles-daemon']))
        self.assertFalse(self.log.exists())
        self.assertFalse((self.base / 'data').exists())

    def test_auth_cancellation_never_builds(self):
        self.missing.write_text('glib2\n')
        self.cancel.touch()
        self.run_setup('--install-deps', 126)
        self.assertNotIn('make ', self.log.read_text())
        self.assertNotIn('systemctl ', self.log.read_text())
        self.assertFalse((self.base / 'data').exists())

    def test_install_packages_then_unprivileged_setup(self):
        self.missing.write_text('glib2\n')
        self.run_setup('--setup', 1)
        self.assertFalse(self.log.exists())
        self.run_setup('--install-deps')
        self.assertIn(' -S --needed --noconfirm glib2', self.log.read_text())
        self.log.write_text('')
        self.run_setup('--setup')
        calls = self.log.read_text()
        self.assertNotIn('pkexec', calls)
        self.assertIn('make -C ', calls)
        self.assertIn('systemctl --user daemon-reload', calls)
        self.assertIn('systemctl --user enable omapower.service', calls)
        self.assertIn('systemctl --user restart omapower.service', calls)
        binary = self.base / 'data/omapower/omapower'
        self.assertTrue(os.access(binary, os.X_OK))
        unit = (self.base / 'config/systemd/user/omapower.service').read_text()
        self.assertIn('ExecStart="' + str(binary).replace('%', '%%').replace('$', '$$') + '"', unit)
        self.assertTrue(json.loads(self.run_setup('--check'))['ready'])
        # Writing into the plugin checkout makes the shell reload and kill setup.
        self.assertEqual([p.name for p in self.project.iterdir()], ['setup.sh'])

    def test_failed_service_start_stays_retryable(self):
        self.tool('systemctl', 'exit 1\n')
        self.run_setup('--setup', 1)
        self.assertFalse(json.loads(self.run_setup('--check'))['ready'])
        self.tool('systemctl', 'echo "systemctl $*" >> "$CALLS"\n')
        self.run_setup('--setup')
        self.assertTrue(json.loads(self.run_setup('--check'))['ready'])

    def test_no_auth_when_dependencies_present(self):
        self.run_setup('--install-deps')
        self.assertFalse(self.log.exists())

    def test_build_failure_does_not_write_unit_or_replace_binary(self):
        binary = self.base / 'data/omapower/omapower'
        binary.parent.mkdir(parents=True)
        binary.write_text('old binary')
        self.tool('make', 'exit 2\n')
        self.run_setup('--setup', 2)
        self.assertEqual(binary.read_text(), 'old binary')
        self.assertFalse((self.base / 'config/systemd/user/omapower.service').exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
