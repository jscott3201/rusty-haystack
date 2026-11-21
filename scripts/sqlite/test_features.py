"""Check resolved normal dependencies of independently selected storage features."""
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]


def dependencies(package, features=None, minimal=True):
    command = ["cargo", "tree", "--locked", "-p", package, "-e", "normal",
               "--prefix", "none", "--format", "{p}"]
    if minimal:
        command.append("--no-default-features")
    if features:
        command += ["--features", features]
    result = subprocess.run(command, cwd=ROOT, check=True, text=True,
                            capture_output=True)
    return {line.split()[0] for line in result.stdout.splitlines() if line.strip()}


class FeatureIsolation(unittest.TestCase):
    def test_core_minimal_and_default_do_not_select_database_drivers(self):
        for minimal in (True, False):
            with self.subTest(minimal=minimal):
                selected = dependencies("rusty-haystack-core", minimal=minimal)
                self.assertFalse({"rusqlite", "libsqlite3-sys", "sqlx", "sqlx-postgres"} & selected)

    def test_application_storage_selections_are_independent(self):
        for features, sqlite, postgres in ((None, False, False),
                                           ("sqlite", True, False),
                                           ("postgres", False, True),
                                           ("postgres,sqlite", True, True)):
            with self.subTest(features=features):
                selected = dependencies("rusty-haystack-app", features)
                self.assertEqual("rusqlite" in selected, sqlite)
                self.assertEqual("libsqlite3-sys" in selected, sqlite)
                self.assertEqual("sqlx-postgres" in selected, postgres)
                self.assertNotIn("sqlx-sqlite", selected)


if __name__ == "__main__":
    unittest.main()
