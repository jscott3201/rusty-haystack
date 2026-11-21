"""Real short-lived process probes for standalone runner cancellation ownership."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import standalone


WORKER = r'''
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

role, mode, folder = sys.argv[1:]
folder = Path(folder)
if role == "child":
    # Match nextest's process_group(0), without detaching into another session.
    os.setpgrp()
    def child_signal(selected, _frame):
        (folder / "child-signalled").write_text(str(selected))
        if mode == "graceful" and selected == signal.SIGTERM:
            (folder / "child-cleaned").touch()
            raise SystemExit(0)
    signal.signal(signal.SIGINT, child_signal)
    signal.signal(signal.SIGTERM, child_signal)
    (folder / "child.pid").write_text(str(os.getpid()))
    (folder / "child-ready").touch()
    # Even a broken cleanup implementation cannot leave an endless test probe.
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        time.sleep(0.02)
    raise SystemExit(2)

child = subprocess.Popen([sys.executable, __file__, "child", mode, str(folder)])
def leader_signal(selected, _frame):
    (folder / "leader-signalled").write_text(str(selected))
    if mode == "graceful":
        child.send_signal(signal.SIGTERM)
        child.wait(timeout=2)
        (folder / "leader-cleaned").touch()
        raise SystemExit(0)
signal.signal(signal.SIGINT, leader_signal)
signal.signal(signal.SIGTERM, leader_signal)
deadline = time.monotonic() + 5
while not (folder / "child-ready").exists():
    if time.monotonic() >= deadline:
        child.kill()
        child.wait()
        raise SystemExit(3)
    time.sleep(0.01)
(folder / "leader.pid").write_text(str(os.getpid()))
(folder / "leader-ready").touch()
if mode == "leader-exits":
    raise SystemExit(0)
try:
    child.wait(timeout=15)
except subprocess.TimeoutExpired:
    child.kill()
    child.wait()
'''


def running(pid):
    result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                            check=False, capture_output=True, text=True, timeout=5)
    state = result.stdout.strip()
    return bool(state) and not state.startswith("Z")


@unittest.skipUnless(os.name == "posix", "standalone cleanup qualifies POSIX process sessions")
class CommandOwnershipTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="haystack-command-test-")
        self.directory = Path(self.temporary.name)
        self.worker = self.directory / "worker.py"
        self.worker.write_text(WORKER)
        self.graces = patch.multiple(standalone, CANCEL_GRACE=0.2, TERMINATE_GRACE=0.2, KILL_GRACE=1)
        self.graces.start()

    def tearDown(self):
        # Keep failures reviewable without leaking the exact task-owned probes.
        for name in ("child.pid", "leader.pid"):
            file = self.directory / name
            if file.exists():
                pid = int(file.read_text())
                if running(pid):
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
        self.graces.stop()
        self.temporary.cleanup()

    def command(self, mode):
        return [sys.executable, str(self.worker), "leader", mode, str(self.directory)]

    def assert_stopped(self):
        for name in ("child.pid", "leader.pid"):
            self.assertTrue((self.directory / name).exists(), "probe did not reach its ownership barrier")
            self.assertFalse(running(int((self.directory / name).read_text())),
                             "owned process survived checked() cleanup")

    def test_timeout_allows_runner_to_forward_and_reap_separate_group(self):
        with self.assertRaisesRegex(standalone.FixtureError, "timed out; owned processes stopped"):
            standalone.checked(self.command("graceful"), timeout=1)
        self.assertTrue((self.directory / "child-cleaned").exists())
        self.assertTrue((self.directory / "leader-cleaned").exists())
        self.assert_stopped()

    def test_timeout_force_stops_signal_resistant_separate_group(self):
        with self.assertRaisesRegex(standalone.FixtureError, "timed out; owned processes stopped"):
            standalone.checked(self.command("resistant"), timeout=1)
        self.assertEqual((self.directory / "child-signalled").read_text(), str(signal.SIGTERM))
        self.assertFalse((self.directory / "child-cleaned").exists())
        self.assert_stopped()

    def test_leader_exit_does_not_hide_signal_resistant_child(self):
        with self.assertRaisesRegex(standalone.FixtureError, "exited with running children"):
            standalone.checked(self.command("leader-exits"), timeout=5)
        self.assert_stopped()

    def test_keyboard_interrupt_cleans_owned_children_before_propagating(self):
        controller = (
            "import sys\n"
            f"sys.path.insert(0, {str(Path(standalone.__file__).parent)!r})\n"
            "import standalone\n"
            "from pathlib import Path\n"
            "standalone.CANCEL_GRACE = 0.2\n"
            "standalone.TERMINATE_GRACE = 0.2\n"
            "standalone.KILL_GRACE = 1\n"
            "try:\n"
            f"    standalone.checked({self.command('resistant')!r}, timeout=10)\n"
            "except KeyboardInterrupt:\n"
            f"    Path({str(self.directory / 'interrupted')!r}).touch()\n"
        )
        process = subprocess.Popen([sys.executable, "-c", controller], start_new_session=True,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 5
            while not (self.directory / "leader-ready").exists():
                if time.monotonic() >= deadline:
                    self.fail("controller did not reach the process ownership barrier")
                time.sleep(0.01)
            process.send_signal(signal.SIGINT)
            self.assertEqual(process.wait(timeout=5), 0)
            self.assertTrue((self.directory / "interrupted").exists())
            self.assert_stopped()
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
