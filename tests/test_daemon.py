#!/usr/bin/python3
"""Integration tests: all power-profile calls use a private bus."""
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import warnings

from gi.repository import Gio, GLib

BINARY = str(Path(sys.argv.pop(1) if len(sys.argv) > 1 else './brrr').resolve())
ADDRESS = os.environ['DBUS_SESSION_BUS_ADDRESS']
PP_NAME = 'net.hadess.PowerProfiles'
PP_PATH = '/net/hadess/PowerProfiles'
COMM = 'oma_evt_test'
DEFAULT_COMMANDS = sorted([
    'rustc', 'cargo', 'gcc', 'g++', 'cc', 'c++', 'cc1', 'cc1plus',
    'clang', 'clang++', 'zig', 'go', 'javac', 'swiftc', 'swift-frontend',
    'gfortran', 'flang', 'nvcc', 'ghc', '7z', 'blender', 'ffmpeg', 'obs',
    '7zz', 'HandBrakeCLI', 'ghb', 'resolve',
])
PP_XML = '''<node><interface name="net.hadess.PowerProfiles">
<method name="HoldProfile"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="u" direction="out"/></method>
<method name="ReleaseProfile"><arg type="u" direction="in"/></method>
<signal name="ProfileReleased"><arg type="u"/></signal>
</interface></node>'''
CHILD = '''import ctypes, os, sys, threading
libc = ctypes.CDLL(None)
def rename(name): libc.prctl(15, name.encode(), 0, 0, 0)
rename(sys.argv[1])
print('ready', flush=True)
for line in sys.stdin:
    command, _, value = line.strip().partition(' ')
    if command == 'name': rename(value)
    elif command == 'exec': os.execl('/usr/bin/sleep', 'sleep', '30')
    elif command == 'thread':
        ready = threading.Event()
        def worker():
            rename(value)
            ready.set()
            threading.Event().wait()
        threading.Thread(target=worker, daemon=True).start()
        ready.wait()
    print('ready', flush=True)
'''


def pump(seconds=0):
    end = time.monotonic() + seconds
    while True:
        while GLib.MainContext.default().iteration(False):
            pass
        if time.monotonic() >= end:
            return
        time.sleep(.005)


def until(predicate, timeout=4, message='condition not met'):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        pump()
        if predicate():
            return
        time.sleep(.005)
    raise AssertionError(message)


class Connection:
    def __init__(self, name):
        self.bus = Gio.DBusConnection.new_for_address_sync(ADDRESS,
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
            None, None)
        self.name = name
        self.registrations = []
        self.subscriptions = []

    def export(self, path, xml, method=None, getter=None):
        info = Gio.DBusNodeInfo.new_for_xml(xml)
        self.registrations.append(self.bus.register_object(path, info.interfaces[0], method, getter, None))

    def own(self):
        reply = self.bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
            'org.freedesktop.DBus', 'RequestName', GLib.Variant('(su)', (self.name, 0)),
            GLib.VariantType.new('(u)'), Gio.DBusCallFlags.NONE, 1000, None)
        assert reply.unpack()[0] == 1

    def close(self):
        if self.bus is not None:
            for subscription in self.subscriptions:
                self.bus.signal_unsubscribe(subscription)
            for registration in self.registrations:
                self.bus.unregister_object(registration)
            self.bus.close_sync(None)
            self.bus = None
            pump(.02)


class Profiles(Connection):
    def __init__(self):
        super().__init__(PP_NAME)
        self.holds = {}
        self.hold_calls = self.release_calls = 0
        self.release_errors = 0
        self.hold_error = self.hold_reject = self.delay_hold = self.release_before_reply = False
        self.delayed = None
        self.export(PP_PATH, PP_XML, self.method)
        self.subscriptions.append(self.bus.signal_subscribe('org.freedesktop.DBus',
            'org.freedesktop.DBus', 'NameOwnerChanged', '/org/freedesktop/DBus', None,
            Gio.DBusSignalFlags.NONE, self.owner_changed, None))
        self.own()

    def owner_changed(self, connection, sender, path, interface, member, parameters, data):
        name, old, new = parameters.unpack()
        if name.startswith(':') and not new:
            self.holds = {cookie: owner for cookie, owner in self.holds.items() if owner != name}

    def method(self, connection, sender, path, interface, method, parameters, invocation):
        if method == 'HoldProfile':
            self.hold_calls += 1
            if self.hold_reject:
                # What power-profiles-daemon answers when the profile is unavailable.
                invocation.return_dbus_error('org.freedesktop.DBus.Error.InvalidArgs',
                                             "Cannot hold profile 'performance' as it is not available")
                return
            cookie = self.hold_calls
            self.holds[cookie] = sender
            if self.hold_error:
                invocation.return_dbus_error('org.freedesktop.DBus.Error.NoReply', 'ambiguous acquisition')
            elif self.delay_hold:
                self.delayed = invocation, cookie
            elif self.release_before_reply:
                self.manual_release(cookie)
                invocation.return_value(GLib.Variant('(u)', (cookie,)))
            else:
                invocation.return_value(GLib.Variant('(u)', (cookie,)))
        else:
            self.release_calls += 1
            if self.release_errors:
                self.release_errors -= 1
                invocation.return_dbus_error('org.freedesktop.DBus.Error.Failed', 'injected release failure')
            else:
                self.holds.pop(parameters.unpack()[0], None)
                invocation.return_value(None)

    def manual_release(self, cookie=None):
        cookie = cookie or next(iter(self.holds))
        owner = self.holds.pop(cookie)
        self.bus.emit_signal(owner, PP_PATH, PP_NAME, 'ProfileReleased', GLib.Variant('(u)', (cookie,)))
        self.bus.flush_sync(None)


class Daemon:
    """Runs the helper as the shell plugin does: supervised, state on stdout."""
    def __init__(self, commands, poll=False, address=ADDRESS, base=None, wait=True):
        self.temp = None if base else tempfile.TemporaryDirectory(prefix='brrr-test-')
        self.base = Path(base or self.temp.name)
        self.config = self.base / 'config' / 'brrr' / 'config.json'
        self.config.parent.mkdir(parents=True, exist_ok=True)
        self.runtime = self.base / 'runtime'
        self.runtime.mkdir(mode=0o700, exist_ok=True)
        if commands is not None:
            self.configure(commands)
        self.env = dict(os.environ, DBUS_SYSTEM_BUS_ADDRESS=address,
            XDG_CONFIG_HOME=str(self.base / 'config'), XDG_RUNTIME_DIR=str(self.runtime),
            G_DEBUG='fatal-warnings', GIO_USE_VFS='local')
        self.process = subprocess.Popen([BINARY, '--supervised'] + (['--poll'] if poll else []), env=self.env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.states = []
        self.log = ''
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        if wait:
            until(lambda: self.states or self.process.poll() is not None)
            assert self.process.poll() is None, self.process.stderr.read()

    def read(self):
        for line in self.process.stdout:
            self.states.append(json.loads(line))

    def configure(self, commands, **fields):
        temporary = self.config.with_suffix('.tmp')
        temporary.write_text(json.dumps(dict(fields, commands=commands)))
        temporary.replace(self.config)

    def cli(self, *args):
        return subprocess.run([BINARY, *args], env=self.env, capture_output=True, text=True)

    def state(self):
        return self.states[-1] if self.states else None

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
        until(lambda: self.process.poll() is not None, timeout=7)
        self.reader.join(timeout=1)
        self.log = self.process.stderr.read()
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()
        if self.temp:
            self.temp.cleanup()
        for diagnostic in ('CRITICAL', 'WARNING', 'AddressSanitizer', 'LeakSanitizer', 'runtime error:'):
            assert diagnostic not in self.log, self.log


class Lifecycle(unittest.TestCase):
    def setUp(self):
        self.profiles = Profiles()
        self.daemons = []
        self.children = []

    def tearDown(self):
        for daemon in self.daemons:
            daemon.close()
        for child in self.children:
            if child.poll() is None:
                child.terminate()
            child.communicate(timeout=3)
        self.profiles.close()

    def start(self, commands=(COMM,), poll=False, address=ADDRESS):
        daemon = Daemon(commands, poll, address)
        self.daemons.append(daemon)
        until(lambda: daemon.state()['monitor'] != 'connecting')
        return daemon

    def child(self, name=COMM):
        child = subprocess.Popen(['/usr/bin/python3', '-u', '-c', CHILD, name],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(child)
        self.assertEqual(child.stdout.readline().strip(), 'ready')
        return child

    def command(self, child, command):
        child.stdin.write(command + '\n')
        child.stdin.flush()
        if not command.startswith('exec'):
            self.assertEqual(child.stdout.readline().strip(), 'ready')

    def exit_child(self, child, reap=True):
        child.stdin.close()
        child.stdin = None
        if reap:
            child.wait(timeout=3)

    def held(self, daemon):
        until(lambda: daemon.state()['holding'] and bool(self.profiles.holds))

    def released(self, daemon):
        until(lambda: not daemon.state()['holding'] and not self.profiles.holds)

    def test_idle_status_is_not_repeated(self):
        daemon = self.start(())
        until(lambda: daemon.state()['available'])
        before = len(daemon.states)
        pump(1.1)
        self.assertEqual(len(daemon.states), before)
        self.assertEqual(self.profiles.hold_calls, 0)
        self.assertEqual(daemon.state(), dict(enabled=True, configError=False, commands=[], active=[],
            holding=False, suppressed=False, rejected=False, available=True, monitor=daemon.state()['monitor']))

    def test_startup_and_overlapping_matches(self):
        first = self.child()
        daemon = self.start()
        self.held(daemon)
        second = self.child()
        pump(.1)
        self.exit_child(first)
        pump(.1)
        self.assertTrue(daemon.state()['holding'])
        self.assertEqual(self.profiles.hold_calls, 1)
        self.exit_child(second)
        self.released(daemon)

    def test_overlapping_distinct_commands(self):
        for poll in (False, True):
            with self.subTest(poll=poll):
                names = (COMM, 'oma_other_test', 'oma_third_test')
                daemon = self.start(names, poll=poll)
                initial_holds = self.profiles.hold_calls
                initial_releases = self.profiles.release_calls
                children = []
                for name in names:
                    children.append(self.child(name))
                    until(lambda name=name: name in daemon.state()['active'])
                    self.held(daemon)
                self.assertEqual(self.profiles.hold_calls, initial_holds + 1)
                # Exit out of launch order; neither an exit nor a new match
                # should release or duplicate the shared performance hold.
                for index in (1, 0):
                    self.exit_child(children[index])
                    until(lambda index=index: names[index] not in daemon.state()['active'])
                    self.assertTrue(daemon.state()['holding'])
                    self.assertEqual(self.profiles.release_calls, initial_releases)
                replacement = self.child(names[0])
                until(lambda: names[0] in daemon.state()['active'])
                self.exit_child(children[2])
                until(lambda: names[2] not in daemon.state()['active'])
                self.assertTrue(daemon.state()['holding'])
                self.assertEqual(self.profiles.hold_calls, initial_holds + 1)
                self.exit_child(replacement)
                self.released(daemon)
                self.assertEqual(self.profiles.release_calls, initial_releases + 1)
                daemon.close()
                self.daemons.remove(daemon)

    def test_diagnostics_explain_hold_and_suppression(self):
        daemon = self.start()
        first = self.child()
        self.held(daemon)
        self.profiles.manual_release()
        until(lambda: daemon.state()['suppressed'])
        self.exit_child(first)
        until(lambda: not daemon.state()['suppressed'])
        second = self.child()
        self.held(daemon)
        self.exit_child(second)
        self.released(daemon)
        daemon.close()
        self.daemons.remove(daemon)
        for diagnostic in ('power-profiles owner:', 'requesting performance hold',
                           'hold acquired:', 'ProfileReleased:', '"suppressed":true',
                           'releasing performance hold', 'hold release confirmed'):
            self.assertIn(diagnostic, daemon.log)

    def test_rename_exec_and_thread_names(self):
        daemon = self.start()
        if daemon.state()['monitor'] != 'events':
            self.skipTest('proc connector not available on this kernel/namespace')
        child = self.child('oma_unmatched')
        self.command(child, 'thread ' + COMM)
        pump(.15)
        self.assertEqual(self.profiles.hold_calls, 0)
        self.command(child, 'name ' + COMM)
        self.held(daemon)
        self.command(child, 'name oma_unmatched')
        self.released(daemon)
        self.command(child, 'name ' + COMM)
        self.held(daemon)
        self.command(child, 'exec')
        self.released(daemon)

    def test_zombie_does_not_hold_and_is_not_found_at_startup(self):
        child = self.child()
        daemon = self.start()
        self.held(daemon)
        self.exit_child(child, reap=False)
        until(lambda: os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOWAIT | os.WNOHANG) is not None)
        self.released(daemon)
        second = self.start()
        pump(.1)
        self.assertFalse(second.state()['holding'])
        child.wait(timeout=3)

    def test_exited_leader_with_live_thread_still_holds(self):
        try:
            fd = os.pidfd_open(os.getpid())
        except (AttributeError, OSError):
            self.skipTest('pidfds are unavailable')
        os.close(fd)
        child = subprocess.Popen([str(Path(__file__).with_name('thread-group'))],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(child)
        self.assertEqual(child.stdout.readline().strip(), 'ready')
        until(lambda: Path(f'/proc/{child.pid}/stat').read_text().split(') ', 1)[1][0] == 'Z')
        daemon = self.start()
        self.held(daemon)
        self.exit_child(child)
        self.released(daemon)

    def test_daemon_replacement_reacquires(self):
        self.child()
        daemon = self.start()
        self.held(daemon)
        self.profiles.close()
        self.profiles = Profiles()
        self.held(daemon)
        self.assertEqual(self.profiles.hold_calls, 1)

    def test_daemon_can_appear_after_startup(self):
        self.profiles.close()
        self.child()
        daemon = self.start()
        self.assertFalse(daemon.state()['holding'])
        self.profiles = Profiles()
        self.held(daemon)

    def test_daemon_replacement_during_pending_acquisition(self):
        self.profiles.delay_hold = True
        self.child()
        daemon = self.start()
        until(lambda: self.profiles.delayed is not None)
        self.profiles.close()
        self.profiles = Profiles()
        self.held(daemon)
        self.assertIsNone(daemon.process.poll())

    def test_shutdown_during_pending_acquisition_drops_hold(self):
        self.profiles.delay_hold = True
        self.child()
        daemon = self.start()
        until(lambda: self.profiles.delayed is not None)
        daemon.process.terminate()
        until(lambda: daemon.process.poll() is not None)
        self.assertEqual(daemon.process.returncode, 0)
        until(lambda: not self.profiles.holds)

    def test_new_match_cancels_release_retry(self):
        self.profiles.release_errors = 1
        first = self.child()
        daemon = self.start()
        self.held(daemon)
        self.exit_child(first)
        until(lambda: self.profiles.release_calls == 1)
        second = self.child()
        pump(1.2)
        self.assertTrue(daemon.state()['holding'])
        self.assertEqual(self.profiles.hold_calls, 1)
        self.assertEqual(self.profiles.release_calls, 1)
        self.exit_child(second)
        self.released(daemon)

    def test_release_error_retries_without_reporting_success(self):
        self.profiles.release_errors = 1
        child = self.child()
        daemon = self.start()
        self.held(daemon)
        self.exit_child(child)
        until(lambda: self.profiles.release_calls == 1)
        self.assertTrue(daemon.state()['holding'])
        self.assertTrue(self.profiles.holds)
        self.released(daemon)
        self.assertEqual(self.profiles.release_calls, 2)

    def test_permanent_release_error_disconnects(self):
        self.profiles.release_errors = 20
        child = self.child()
        daemon = self.start()
        self.held(daemon)
        self.exit_child(child)
        until(lambda: daemon.process.poll() is not None, timeout=8)
        self.assertEqual(daemon.process.returncode, 1)
        until(lambda: not self.profiles.holds)
        self.assertEqual(self.profiles.release_calls, 3)

    def test_ambiguous_acquisition_disconnects(self):
        self.profiles.hold_error = True
        self.child()
        daemon = self.start(())
        daemon.configure([COMM])
        until(lambda: daemon.process.poll() is not None)
        self.assertEqual(daemon.process.returncode, 1)
        until(lambda: not self.profiles.holds)
        self.assertEqual(self.profiles.hold_calls, 1)

    def test_rejected_hold_waits_for_commands_to_exit(self):
        self.profiles.hold_reject = True
        child = self.child()
        daemon = self.start()
        until(lambda: daemon.state()['rejected'])
        pump(0.5)
        second = self.child()
        pump(0.5)
        self.assertIsNone(daemon.process.poll())
        self.assertEqual(self.profiles.hold_calls, 1)
        self.assertFalse(daemon.state()['holding'])
        self.exit_child(child)
        self.exit_child(second)
        until(lambda: not daemon.state()['rejected'])
        self.profiles.hold_reject = False
        child = self.child()
        self.held(daemon)
        self.assertEqual(self.profiles.hold_calls, 2)
        self.exit_child(child)
        self.released(daemon)

    def test_exit_during_pending_acquisition(self):
        self.profiles.delay_hold = True
        child = self.child()
        daemon = self.start()
        until(lambda: self.profiles.delayed is not None)
        self.exit_child(child)
        invocation, cookie = self.profiles.delayed
        invocation.return_value(GLib.Variant('(u)', (cookie,)))
        self.released(daemon)
        self.assertEqual(self.profiles.release_calls, 1)

    def test_manual_override_until_last_match_exits(self):
        first = self.child()
        daemon = self.start()
        self.held(daemon)
        self.profiles.manual_release()
        until(lambda: daemon.state()['suppressed'])
        second = self.child()
        self.exit_child(first)
        pump(.15)
        self.assertTrue(daemon.state()['suppressed'])
        self.assertEqual(self.profiles.hold_calls, 1)
        self.exit_child(second)
        until(lambda: not daemon.state()['suppressed'])
        self.child()
        self.held(daemon)
        self.assertEqual(self.profiles.hold_calls, 2)

    def test_release_signal_before_acquisition_reply(self):
        self.profiles.release_before_reply = True
        self.child()
        daemon = self.start()
        until(lambda: daemon.state()['suppressed'])
        self.assertFalse(daemon.state()['holding'])
        self.assertFalse(self.profiles.holds)
        self.assertEqual(self.profiles.hold_calls, 1)

    def test_config_replacement_deletion_and_directory_recreation(self):
        self.child()
        daemon = self.start(())
        daemon.configure([COMM])
        self.held(daemon)
        daemon.configure([])
        self.released(daemon)
        daemon.config.unlink()
        pump(.2)
        daemon.config.parent.rename(daemon.config.parent.with_name('old'))
        pump(.2)
        daemon.config.parent.mkdir()
        daemon.configure([COMM])
        self.held(daemon)
        daemon.configure([])
        self.released(daemon)

    def test_invalid_config_drops_hold_until_fixed(self):
        self.child()
        daemon = self.start()
        self.held(daemon)
        for invalid in ('invalid', '{"commands":["%s"],"enabled":"no"}' % COMM):
            daemon.config.write_text(invalid)
            until(lambda: daemon.state()['configError'])
            self.released(daemon)
            self.assertEqual(daemon.state()['commands'], [])
            self.assertIsNone(daemon.process.poll())
            daemon.configure([COMM])
            until(lambda: not daemon.state()['configError'])
            self.held(daemon)

    def prepared(self, text):
        """Start a daemon whose configuration file already holds text."""
        temp = tempfile.TemporaryDirectory(prefix='brrr-test-')
        self.addCleanup(temp.cleanup)
        config = Path(temp.name) / 'config' / 'brrr' / 'config.json'
        config.parent.mkdir(parents=True)
        config.write_text(text)
        daemon = Daemon(None, base=temp.name)
        self.daemons.append(daemon)
        return daemon

    def test_invalid_config_at_startup_is_reported(self):
        self.child()
        daemon = self.prepared('invalid')
        until(lambda: daemon.state()['configError'])
        self.assertFalse(daemon.state()['enabled'])
        daemon.configure([COMM])
        self.held(daemon)

    def test_disable_releases_and_enable_reacquires(self):
        self.child()
        daemon = self.start()
        self.held(daemon)
        self.assertEqual(daemon.cli('--disable').returncode, 0)
        until(lambda: daemon.state()['enabled'] is False)
        self.released(daemon)
        self.assertFalse(daemon.state()['suppressed'])
        self.assertEqual(json.loads(daemon.config.read_text()), dict(commands=[COMM], enabled=False))
        self.assertEqual(daemon.cli('--enable').returncode, 0)
        until(lambda: daemon.state()['enabled'])
        self.held(daemon)
        self.assertEqual(self.profiles.hold_calls, 2)

    def test_disabled_at_startup_never_holds(self):
        self.child()
        daemon = self.prepared(json.dumps(dict(commands=[COMM], enabled=False)))
        until(lambda: daemon.state()['enabled'] is False and daemon.state()['active'] == [COMM])
        pump(.3)
        self.assertEqual(self.profiles.hold_calls, 0)

    def test_supervisor_pipe_closing_stops_and_drops_hold(self):
        self.child()
        daemon = self.start()
        self.held(daemon)
        daemon.process.stdin.close()
        until(lambda: daemon.process.poll() is not None)
        self.assertEqual(daemon.process.returncode, 0)
        until(lambda: not self.profiles.holds)

    def test_second_instance_is_refused(self):
        daemon = self.start(())
        second = Daemon(None, base=daemon.base, wait=False)
        self.daemons.append(second)
        until(lambda: second.process.poll() is not None, timeout=8)
        self.assertEqual(second.process.returncode, 75)
        self.assertIsNone(daemon.process.poll())

    def test_explicit_polling_fallback(self):
        daemon = self.start(poll=True)
        self.assertEqual(daemon.state()['monitor'], 'polling')
        child = self.child()
        self.held(daemon)
        self.exit_child(child)
        self.released(daemon)

    def test_system_bus_disconnection_exits(self):
        bus = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--print-address=1'],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            address = bus.stdout.readline().strip()
            daemon = self.start((), address=address)
            bus.terminate()
            bus.communicate(timeout=3)
            until(lambda: daemon.process.poll() is not None)
            self.assertEqual(daemon.process.returncode, 1)
        finally:
            if bus.poll() is None:
                bus.terminate()
                bus.communicate(timeout=3)


@unittest.skipUnless(shutil.which('quickshell'), 'quickshell is needed for the headless plugin smoke test')
class PluginSmoke(unittest.TestCase):
    """Run the real plugin service and widget in Quickshell, with visual-only stubs."""
    def setUp(self):
        self.profiles = Profiles()
        self.children = []
        self.temp = tempfile.TemporaryDirectory(prefix='brrr-qml-test-')
        base = Path(self.temp.name)
        self.config = base / 'config' / 'brrr' / 'config.json'
        self.runtime = base / 'runtime'
        self.runtime.mkdir(mode=0o700)
        shell = base / 'shell'
        (shell / 'plugin' / 'i18n').mkdir(parents=True)
        (shell / 'bin').mkdir()
        project = Path(__file__).resolve().parent.parent
        for name in ('Service.qml', 'BarWidget.qml', 'Whitelist.js', 'i18n/en.json'):
            shutil.copyfile(project / 'plugin' / name, shell / 'plugin' / name)
        shutil.copy(project / 'bin' / 'retire-legacy-service', shell / 'bin')
        launcher = shell / 'bin' / 'brrr'
        launcher.write_text('#!/bin/sh\nexec %s "$@"\n' % json.dumps(BINARY))
        launcher.chmod(0o755)
        stubs = {
            'Ui/qmldir': 'module qs.Ui\nBarWidget 1.0 BarWidget.qml\nWidgetButton 1.0 WidgetButton.qml\n',
            'Ui/BarWidget.qml': 'import QtQuick\nItem { property string moduleName; property var bar: null }',
            'Ui/WidgetButton.qml': '''import QtQuick
Item {
    property var bar
    property string fontFamily
    property bool centerFigures
    property string text
    property string tooltipText
    signal pressed(int buttonCode)
    implicitWidth: 24
    implicitHeight: 24
}''',
            'Commons/qmldir': 'module qs.Commons\nTranslations 1.0 Translations.qml\n',
            'Commons/Translations.qml': '''import QtQml
QtObject { property url directory; function t(key) { return key } }''',
            'plugin/Panel.qml': '''import QtQuick
Item {
    property var bar
    property var anchorItem
    property var hostWidget
    property bool opened: false
    property bool popoutSwitchClosing: false
    function open() { opened = true }
    function close() { opened = false }
    function closeForPopoutSwitch() { opened = false }
    function whitelistEditFinished(success) {}
}''',
            'shell.qml': '''import QtQuick
import Quickshell
import Quickshell.Io
import "./plugin" as Plugin
ShellRoot {
    // A plugin update or disable destroys and recreates the service.
    Loader { id: service; sourceComponent: Plugin.Service {} }
    Plugin.BarWidget { id: widget }
    property string snapshot: JSON.stringify({
        valid: widget.statusValid, running: widget.running, reason: widget.stopReason,
        enabled: widget.boostEnabled, configError: widget.configError, commands: widget.commands,
        error: widget.whitelistError, holding: widget.holding, canEdit: widget.canEdit,
        actionError: widget.actionError, editError: widget.whitelistActionError
    })
    onSnapshotChanged: console.log("BRRR_WIDGET " + snapshot)
    FileView {
        path: Quickshell.env("BRRR_CONTROL")
        watchChanges: true
        onFileChanged: reload()
        onLoaded: {
            const command = JSON.parse(text())
            if (command.add) widget.editWhitelist("--add", command.add)
            if (command.toggle) widget.toggleBoost()
            if (command.service !== undefined) service.active = command.service
        }
    }
    Timer { interval: 25000; running: true; onTriggered: Qt.quit() }
}''',
        }
        for name, text in stubs.items():
            path = shell / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        self.control = base / 'control.json'
        self.control.write_text('{}')
        self.base = base
        self.systemctl_fails = base / 'systemctl-fails'
        tools = base / 'tools'
        tools.mkdir()
        (tools / 'systemctl').write_text('#!/bin/sh\n[ ! -e %s ]\n' % json.dumps(str(self.systemctl_fails)))
        (tools / 'systemctl').chmod(0o755)
        self.shell = shell
        # Isolate everything the plugin may touch, including the legacy-unit retirement.
        env = dict(os.environ, QT_QPA_PLATFORM='offscreen', DBUS_SYSTEM_BUS_ADDRESS=ADDRESS, HOME=str(base),
            XDG_CONFIG_HOME=str(base / 'config'), XDG_DATA_HOME=str(base / 'data'),
            XDG_RUNTIME_DIR=str(self.runtime), XDG_STATE_HOME=str(base / 'state'),
            BRRR_CONTROL=str(self.control), GIO_USE_VFS='local', PATH=str(tools) + ':' + os.environ['PATH'])
        env.pop('WAYLAND_DISPLAY', None)
        self.env = env
        self.buffer = b''
        self.output = ''
        self.snapshot = {}
        if not self._testMethodName.endswith('_legacy'):
            self.launch()

    def launch(self):
        self.process = subprocess.Popen(['quickshell', '--no-color', '--path', str(self.shell / 'shell.qml')],
            env=self.env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        os.set_blocking(self.process.stdout.fileno(), False)

    def tearDown(self):
        if self.process.poll() is None:
            self.process.terminate()
        until(lambda: self.process.poll() is not None, timeout=7)
        self.process.communicate(timeout=1)
        for child in self.children:
            if child.poll() is None:
                child.terminate()
            child.communicate(timeout=3)
        until(lambda: self.helper() is None, timeout=7, message='helper outlived the shell')
        self.profiles.close()
        self.temp.cleanup()

    def helper(self):
        """PID of this test's supervised helper, found by its private runtime directory."""
        marker = ('XDG_RUNTIME_DIR=%s' % self.runtime).encode()
        for entry in Path('/proc').iterdir():
            try:
                if not entry.name.isdigit() or os.readlink(entry / 'exe') != BINARY:
                    continue
                if marker in (entry / 'environ').read_bytes().split(b'\0'):
                    return int(entry.name)
            except OSError:
                continue
        return None

    def read_snapshot(self):
        try:
            data = os.read(self.process.stdout.fileno(), 65536)
            self.buffer += data
            self.output += data.decode(errors='replace')
        except BlockingIOError:
            pass
        while b'\n' in self.buffer:
            line, self.buffer = self.buffer.split(b'\n', 1)
            marker = b'BRRR_WIDGET '
            if marker in line:
                self.snapshot = json.loads(line.split(marker, 1)[1])
        return self.snapshot

    def wait(self, predicate, timeout=5):
        try:
            until(lambda: predicate(self.read_snapshot()), timeout=timeout)
        except AssertionError:
            self.read_snapshot()
            self.fail('%s\nlast snapshot: %s' % (self.output, self.snapshot))

    def send(self, **command):
        temporary = self.control.with_suffix('.tmp')
        temporary.write_text(json.dumps(command))
        temporary.replace(self.control)

    def child(self):
        child = subprocess.Popen(['/usr/bin/python3', '-u', '-c', CHILD, COMM],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(child)
        self.assertEqual(child.stdout.readline().strip(), 'ready')
        return child

    def test_install_starts_boost_and_widget_controls_it(self):
        self.wait(lambda s: s.get('running') and s.get('enabled') and s.get('commands') == DEFAULT_COMMANDS)
        self.send(add=COMM)
        self.wait(lambda s: COMM in s.get('commands', []) and s.get('canEdit'))
        self.child()
        self.wait(lambda s: s.get('holding'))
        until(lambda: bool(self.profiles.holds))
        self.send(toggle=True)
        self.wait(lambda s: s.get('enabled') is False and not s.get('holding') and s.get('canEdit'))
        until(lambda: not self.profiles.holds)
        self.send(toggle=True)
        self.wait(lambda s: s.get('enabled') and s.get('holding'))
        self.config.write_text('invalid')
        self.wait(lambda s: s.get('configError') and s.get('error') and not s.get('holding'))
        until(lambda: not self.profiles.holds)
        self.config.write_text(json.dumps(dict(commands=[COMM])))
        self.wait(lambda s: not s.get('configError') and s.get('commands') == [COMM] and s.get('holding'))
        self.assertNotIn('ReferenceError', self.output)
        self.assertNotIn('TypeError', self.output)

    def test_never_runs_beside_the_legacy_service_legacy(self):
        # The retire script still looks for the historical omapower unit.
        data = self.base / 'data' / 'omapower'
        unit = self.base / 'config' / 'systemd' / 'user' / 'omapower.service'
        data.mkdir(parents=True)
        unit.parent.mkdir(parents=True)
        unit.write_text('[Service]\nExecStart="%s"\n' % (data / 'omapower'))
        self.systemctl_fails.touch()
        self.launch()
        self.wait(lambda s: s.get('running') is False and s.get('reason') == 'legacy')
        pump(.5)
        self.assertIsNone(self.helper())
        self.systemctl_fails.unlink()
        self.wait(lambda s: s.get('running'), timeout=9)
        self.assertFalse(unit.exists())

    def test_helper_is_restarted_and_stops_with_the_shell(self):
        self.wait(lambda s: s.get('running'))
        first = self.helper()
        self.assertIsNotNone(first)
        os.kill(first, 9)
        self.wait(lambda s: s.get('running') is False and s.get('reason') == 'stopped')
        self.wait(lambda s: s.get('running'), timeout=9)
        second = self.helper()
        self.assertNotIn(second, (None, first))
        self.send(service=False)
        until(lambda: self.helper() is None, timeout=7, message='helper outlived its plugin service')
        self.send(service=True)
        self.wait(lambda s: s.get('running') is False)
        self.wait(lambda s: s.get('running'))
        self.assertNotIn(self.helper(), (None, second))
        self.process.terminate()
        until(lambda: self.process.poll() is not None, timeout=7)
        until(lambda: self.helper() is None, timeout=7, message='helper outlived the shell')

def setUpModule():
    # unittest installs its filters after importing the module. Suppress only
    # PyGObject's known Python 3.14 asyncio-bridge deprecations, not test errors.
    warnings.filterwarnings('ignore', category=DeprecationWarning, module=r'gi\.events')


if __name__ == '__main__':
    unittest.main(verbosity=2)
