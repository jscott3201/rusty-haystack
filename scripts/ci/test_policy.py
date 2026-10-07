"""Behavioral policy tests and format-constrained workflow wiring assertions.

Uses only the standard library. actionlint separately validates YAML semantics.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
# Independent oracle; never derive these from policy.py or observed needs.
REQUIRED = ("ci-policy", "fmt", "clippy", "test", "current-stable", "python",
            "deny", "core-features", "codeql")
PROFILES = {"dev": ["ubuntu-latest"],
            "main": ["ubuntu-latest", "macos-latest", "windows-latest"]}


def invoke(command, **environment):
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("CI_", "GITHUB_"))}
    env.update(environment)
    return subprocess.run([sys.executable, str(ROOT / "scripts/ci/policy.py"), command],
                          env=env, text=True, capture_output=True, check=False)


def successful_needs(profile="dev"):
    needs = {job: {"result": "success", "outputs": {}} for job in REQUIRED}
    needs["ci-policy"]["outputs"] = {
        "profile": profile, "test-os": json.dumps(PROFILES[profile]),
    }
    return needs


class AggregateTests(unittest.TestCase):
    def evaluate(self, needs, profile="dev"):
        return invoke("aggregate", CI_PROFILE=profile, CI_NEEDS_JSON=json.dumps(needs))

    def test_all_successes_for_each_profile(self):
        for profile in PROFILES:
            with self.subTest(profile=profile):
                result = self.evaluate(successful_needs(profile), profile)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("CI OK", result.stdout)

    def test_every_job_rejects_non_success_missing_result_and_missing_edge(self):
        for job in REQUIRED:
            for state in ("failure", "cancelled", "skipped", "pending", "", None, 1):
                with self.subTest(job=job, state=state):
                    needs = successful_needs()
                    needs[job]["result"] = state
                    self.assertEqual(self.evaluate(needs).returncode, 1)
            for missing in ("result", "edge"):
                with self.subTest(job=job, missing=missing):
                    needs = successful_needs()
                    if missing == "result":
                        del needs[job]["result"]
                    else:
                        del needs[job]
                    self.assertEqual(self.evaluate(needs).returncode, 1)

    def test_malformed_json_and_shapes_do_not_echo_payload(self):
        secret = "untrusted-payload-do-not-print"
        for raw in ("", "{", "null", "[]", "false", "42", json.dumps(secret),
                    '{"fmt": {}, "fmt": {}}', '{"fmt":{"result":NaN}}', '[' * 2000):
            with self.subTest(raw=raw[:40]):
                result = invoke("aggregate", CI_PROFILE="dev", CI_NEEDS_JSON=raw)
                self.assertEqual(result.returncode, 1)
                self.assertNotIn(secret, result.stdout + result.stderr)
        for value in (None, [], "success", {"result": "success"},
                      {"result": "success", "outputs": []}):
            with self.subTest(value=value):
                needs = successful_needs()
                needs["fmt"] = value
                self.assertEqual(self.evaluate(needs).returncode, 1)

    def test_duplicate_nested_result_cannot_hide_failure(self):
        raw = json.dumps(successful_needs()).replace(
            '"result": "success"', '"result": "failure", "result": "success"', 1)
        self.assertEqual(invoke("aggregate", CI_PROFILE="dev", CI_NEEDS_JSON=raw).returncode, 1)

    def test_unknown_or_missing_profile_fails(self):
        for profile in ("", "release", "MAIN"):
            with self.subTest(profile=profile):
                self.assertEqual(self.evaluate(successful_needs(), profile).returncode, 1)
        self.assertEqual(invoke("aggregate", CI_NEEDS_JSON=json.dumps(successful_needs())).returncode, 1)

    def test_unregistered_job_fails_even_when_successful(self):
        needs = successful_needs()
        needs["new-job"] = {"result": "success", "outputs": {}}
        self.assertEqual(self.evaluate(needs).returncode, 1)

    def test_successful_matrix_does_not_bless_wrong_inventory(self):
        for outputs in ({}, {"profile": "dev", "test-os": "[]"},
                        {"profile": "main", "test-os": '["ubuntu-latest"]'},
                        {"profile": "main", "test-os": "null"},
                        {"profile": "main", "test-os": PROFILES["main"]}):
            with self.subTest(outputs=outputs):
                needs = successful_needs("main")
                needs["ci-policy"]["outputs"] = outputs
                self.assertEqual(self.evaluate(needs, "main").returncode, 1)


class PlanTests(unittest.TestCase):
    def test_expanded_matrix_for_pushes_and_prs_including_retargets(self):
        for branch, expected in PROFILES.items():
            for event, action in (("push", ""), ("pull_request", "opened"),
                                  ("pull_request", "synchronize"),
                                  ("pull_request", "reopened"), ("pull_request", "edited")):
                with self.subTest(branch=branch, event=event, action=action):
                    result = invoke("plan", GITHUB_EVENT_NAME=event, CI_EVENT_ACTION=action,
                                    GITHUB_REF=f"refs/heads/{branch}" if event == "push" else "refs/pull/123/merge",
                                    GITHUB_BASE_REF=branch if event == "pull_request" else "")
                    self.assertEqual(result.returncode, 0, result.stderr)
                    outputs = dict(line.split("=", 1) for line in result.stdout.splitlines())
                    self.assertEqual(outputs["profile"], branch)
                    self.assertEqual(json.loads(outputs["test-os"]), expected)

    def test_retarget_uses_current_base_not_head_or_previous_target(self):
        result = invoke("plan", GITHUB_EVENT_NAME="pull_request", CI_EVENT_ACTION="edited",
                        GITHUB_BASE_REF="dev", GITHUB_REF="refs/heads/main")
        self.assertEqual(result.returncode, 0, result.stderr)
        outputs = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(json.loads(outputs["test-os"]), PROFILES["dev"])

    def test_unrecognized_context_fails_without_outputs(self):
        cases = ({}, {"GITHUB_EVENT_NAME": "workflow_dispatch"},
                 {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/topic"},
                 {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/tags/main"},
                 {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_BASE_REF": "topic", "CI_EVENT_ACTION": "opened"},
                 {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_BASE_REF": "main", "CI_EVENT_ACTION": "closed"})
        for context in cases:
            with self.subTest(context=context):
                result = invoke("plan", **context)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, "")


class WorkflowContractTests(unittest.TestCase):
    @staticmethod
    def jobs(workflow):
        # A guard for this repository's canonical two-space layout, not a YAML
        # parser. Structural changes must update this reviewed contract too.
        body = workflow.split("\njobs:\n", 1)[1]
        headers = list(re.finditer(r"^  ([a-z][a-z0-9-]*):[ \t]*$", body, re.MULTILINE))
        return {match[1]: body[match.end():headers[index + 1].start() if index + 1 < len(headers) else len(body)]
                for index, match in enumerate(headers)}

    def check_contract(self, ci, codeql):
        jobs = self.jobs(ci)
        self.assertEqual(set(jobs), set(REQUIRED) | {"ci-ok"})
        aggregate = jobs["ci-ok"]
        for line in ("    name: CI OK", "    if: ${{ always() }}",
                     "          CI_NEEDS_JSON: ${{ toJSON(needs) }}",
                     "          CI_PROFILE: ${{ needs.ci-policy.outputs.profile }}",
                     "        run: python3 scripts/ci/policy.py aggregate"):
            self.assertIn(f"\n{line}\n", aggregate)
        needs = re.search(r"^    needs: \[([^\]]+)\]$", aggregate, re.MULTILINE)
        self.assertIsNotNone(needs)
        self.assertCountEqual([item.strip() for item in needs[1].split(",")], REQUIRED)
        for job in REQUIRED:
            self.assertNotRegex(jobs[job], r"(?m)^    (if|continue-on-error):")
        self.assertIn("types: [opened, synchronize, reopened, edited]", ci)
        self.assertIn("run: python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v", jobs["ci-policy"])
        self.assertIn('run: python3 scripts/ci/policy.py plan >> "$GITHUB_OUTPUT"', jobs["ci-policy"])
        self.assertIn("CI_EVENT_ACTION: ${{ github.event.action }}", jobs["ci-policy"])
        for name in ("profile", "test-os"):
            self.assertIn(f"{name}: ${{{{ steps.plan.outputs.{name} }}}}", jobs["ci-policy"])
        for line in ("    needs: ci-policy", "    runs-on: ${{ matrix.os }}",
                     "        os: ${{ fromJSON(needs.ci-policy.outputs.test-os) }}",
                     "      fail-fast: false"):
            self.assertIn(f"\n{line}\n", jobs["test"])
        for flags in ("", " --no-default-features"):
            for command in ("clippy", "test"):
                suffix = " --all-targets -- -D warnings" if command == "clippy" else ""
                self.assertIn(f"\n      - run: cargo +1.99.0 {command} --locked -p rusty-haystack-core{flags}{suffix}\n",
                              jobs["core-features"])
        self.assertIn("\n    uses: ./.github/workflows/codeql.yml\n", jobs["codeql"])
        self.assertIn("\n      security-events: write\n", jobs["codeql"])
        for trigger in ("workflow_call", "schedule", "workflow_dispatch"):
            self.assertIn(f"\n  {trigger}:\n", codeql)
        self.assertNotRegex(codeql, r"(?m)^  (pull_request|push):")
        analyze = self.jobs(codeql)["analyze"]
        self.assertNotRegex(analyze, r"(?m)^    (if|continue-on-error):")
        self.assertEqual(re.findall(r"^          - language: (\w+)$", analyze, re.MULTILINE),
                         ["rust", "python", "actions"])
        self.assertIn("\n      fail-fast: false\n", analyze)
        self.assertIn("config-file: ./.github/codeql/codeql-config.yml", analyze)

    def test_live_workflow_contract(self):
        self.check_contract((ROOT / ".github/workflows/ci.yml").read_text(),
                            (ROOT / ".github/workflows/codeql.yml").read_text())

    def test_wiring_mutations_are_detected(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        codeql = (ROOT / ".github/workflows/codeql.yml").read_text()
        mutations = (
            (ci.replace(", core-features, codeql]", ", core-features]"), codeql),
            (ci.replace("    if: ${{ always() }}\n", ""), codeql),
            (ci.replace("fromJSON(needs.ci-policy.outputs.test-os)", "fromJSON('[\"ubuntu-latest\"]')"), codeql),
            (ci.replace("reopened, edited]", "reopened]"), codeql),
            (ci, codeql.replace("          - language: actions\n            build-mode: none\n", "")),
        )
        for index, (changed_ci, changed_codeql) in enumerate(mutations):
            with self.subTest(mutation=index):
                self.assertTrue((changed_ci, changed_codeql) != (ci, codeql), "mutation did not change the workflow")
                with self.assertRaises(AssertionError):
                    self.check_contract(changed_ci, changed_codeql)


if __name__ == "__main__":
    unittest.main()
