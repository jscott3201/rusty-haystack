"""Small fail-closed CI policy; input contexts are data, never shell source."""
import argparse
import json
import os
import sys

# Independent of observed needs. A change requires updating workflow wiring,
# independent tests, and docs/support-matrix.md alongside this inventory.
REQUIRED_JOBS = frozenset({
    "ci-policy", "fmt", "clippy", "test", "current-stable", "python", "deny",
    "core-features", "codeql",
})
PROFILE_OS = {
    "dev": ["ubuntu-latest"],
    "main": ["ubuntu-latest", "macos-latest", "windows-latest"],
}


class PolicyError(ValueError):
    """A bounded diagnostic, never the raw untrusted input."""


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise PolicyError("duplicate JSON object key")
        result[key] = value
    return result


def reject_constant(_value):
    raise PolicyError("non-standard JSON constant")


def read_json(raw):
    if not isinstance(raw, str):
        raise PolicyError("JSON input must be a string")
    try:
        return json.loads(raw, object_pairs_hook=unique_object, parse_constant=reject_constant)
    except (json.JSONDecodeError, RecursionError) as error:
        raise PolicyError("malformed JSON input") from error


def plan():
    event = os.environ.get("GITHUB_EVENT_NAME")
    if event == "pull_request":
        if os.environ.get("CI_EVENT_ACTION") not in {"opened", "synchronize", "reopened", "edited"}:
            raise PolicyError("unsupported pull request action")
        profile = os.environ.get("GITHUB_BASE_REF")
    elif event == "push":
        profile = {"refs/heads/dev": "dev", "refs/heads/main": "main"}.get(os.environ.get("GITHUB_REF"))
    else:
        raise PolicyError("unsupported CI event")
    if profile not in PROFILE_OS:
        raise PolicyError("unsupported CI target branch")
    print(f"profile={profile}")
    print("test-os=" + json.dumps(PROFILE_OS[profile], separators=(",", ":")))


def aggregate():
    profile = os.environ.get("CI_PROFILE")
    if profile not in PROFILE_OS:
        raise PolicyError("unknown or missing CI profile")
    needs = read_json(os.environ.get("CI_NEEDS_JSON", ""))
    if not isinstance(needs, dict) or set(needs) != REQUIRED_JOBS:
        raise PolicyError("required job inventory is missing or changed")
    for job in sorted(REQUIRED_JOBS):
        value = needs[job]
        if not isinstance(value, dict) or not isinstance(value.get("outputs"), dict):
            raise PolicyError(f"invalid job result shape: {job}")
        if value.get("result") != "success":
            raise PolicyError(f"required job did not succeed: {job}")
    outputs = needs["ci-policy"]["outputs"]
    if outputs.get("profile") != profile:
        raise PolicyError("CI plan profile does not match")
    if read_json(outputs.get("test-os")) != PROFILE_OS[profile]:
        raise PolicyError("CI plan does not contain the required OS matrix")
    print(f"CI OK: all {len(REQUIRED_JOBS)} required jobs succeeded for {profile}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("plan", "aggregate"))
    command = parser.parse_args().command
    try:
        {"plan": plan, "aggregate": aggregate}[command]()
    except PolicyError as error:
        print(f"CI policy failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
