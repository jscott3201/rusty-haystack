"""Handing a wrapper to a server or a SharedGraph consumes it (issue #18).

Both constructors move Rust state out of their argument. Before this, they left
a hollow-but-callable object behind, so the mistake was silent: `add_user` on a
consumed AuthManager went somewhere the server could never read, and `add` on a
consumed EntityGraph succeeded against a graph nobody would ever query.

The AuthManager case failed *closed* — the hollow manager authenticated nobody —
so this is a usability fix, not a security fix. The failure mode it removes is
"my users mysteriously do not work", reported far from its cause.
"""

import pytest

import rusty_haystack as rh

USERS_TOML = """
[users.alice]
password_hash = "dGVzdA==:100000:dGVzdA==:dGVzdA=="
role = "admin"
"""


def test_auth_manager_is_poisoned_after_with_auth():
    auth = rh.server.AuthManager.from_toml_str(USERS_TOML)
    assert auth.is_enabled()

    server = rh.server.HaystackServer(rh.SharedGraph())
    server.with_auth(auth)

    # The object is still alive. Before, this returned False and told you nothing.
    with pytest.raises(RuntimeError, match="no longer usable"):
        auth.is_enabled()


def test_consumed_auth_manager_cannot_be_handed_to_a_second_server():
    auth = rh.server.AuthManager.from_toml_str(USERS_TOML)
    rh.server.HaystackServer(rh.SharedGraph()).with_auth(auth)

    with pytest.raises(RuntimeError, match="no longer usable"):
        rh.server.HaystackServer(rh.SharedGraph()).with_auth(auth)


def test_auth_manager_repr_reports_consumption_without_raising():
    # repr must never raise: it is what a debugger and a traceback call, and
    # that is exactly where you are standing when you hit the poison.
    auth = rh.server.AuthManager.empty()
    rh.server.HaystackServer(rh.SharedGraph()).with_auth(auth)
    assert repr(auth) == "AuthManager(consumed)"


def test_entity_graph_is_poisoned_after_shared_graph():
    graph = rh.EntityGraph()
    graph.add(rh.HDict({"id": rh.Ref("site-1"), "site": rh.Marker()}))
    assert len(graph) == 1

    shared = rh.SharedGraph(graph)
    assert len(shared) == 1, "the entities moved to the SharedGraph"

    # This is the bug: it used to succeed against an empty orphaned graph.
    with pytest.raises(RuntimeError, match="no longer usable"):
        graph.add(rh.HDict({"id": rh.Ref("site-2"), "site": rh.Marker()}))

    with pytest.raises(RuntimeError, match="no longer usable"):
        len(graph)

    assert repr(graph) == "EntityGraph(consumed)"

    # The write that used to vanish must not have reached anything.
    assert len(shared) == 1


def test_shared_graph_without_an_argument_still_works():
    # The None path must be unaffected — it consumes nothing.
    shared = rh.SharedGraph()
    assert len(shared) == 0
