"""Deterministic fixture startup checks; real Docker qualification is separate."""
import unittest
from unittest.mock import patch

import qualify


class StartupTests(unittest.TestCase):
    def test_temporary_socket_server_cannot_satisfy_readiness(self):
        phase = "initializing"
        probes = []

        def native(*args, **_kwargs):
            if "pg_isready" in args:
                probes.append(args)
                tcp = "-h" in args and args[args.index("-h") + 1] == "127.0.0.1"
                if tcp and phase == "initializing":
                    raise qualify.FixtureError("final TCP server not ready")
                # The image's temporary server accepts only Unix sockets.
                return ""
            if "psql" in args:
                self.assertEqual(phase, "final", "settings must wait for final server startup")
                return "off" if args[-1] == "SHOW ssl" else "180006|on|on|on"
            if args[1] == "port":
                return "127.0.0.1:12345"
            return ""

        def finish_initialization(_delay):
            nonlocal phase
            phase = "final"

        with patch.object(qualify, "native", side_effect=native), \
                patch.object(qualify.time, "sleep", side_effect=finish_initialization):
            fixtures = qualify.Fixtures()
            try:
                self.assertTrue(fixtures.start(plain=True).is_file())
                self.assertEqual(len(probes), 2)
            finally:
                fixtures.close()

    def test_readiness_deadline_fails_and_owned_fixture_is_removable(self):
        removed = []

        def native(*args, **_kwargs):
            if "pg_isready" in args:
                raise qualify.FixtureError("not ready")
            if args[1] == "rm":
                removed.append(args[-1])
            self.assertNotIn("psql", args)
            return ""

        with patch.object(qualify, "native", side_effect=native), \
                patch.object(qualify.time, "monotonic", side_effect=[0, 61]), \
                patch.object(qualify.time, "sleep") as sleep:
            fixtures = qualify.Fixtures()
            directory = fixtures.directory
            try:
                with self.assertRaisesRegex(qualify.FixtureError, "readiness timed out"):
                    fixtures.start(plain=True)
                sleep.assert_not_called()
            finally:
                fixtures.close()
            self.assertEqual(removed, fixtures.containers)
            self.assertFalse(directory.exists())


if __name__ == "__main__":
    unittest.main()
