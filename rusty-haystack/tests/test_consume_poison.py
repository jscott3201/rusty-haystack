"""Handing a wrapper to a server or a SharedGraph consumes it (issue #18).

Both constructors move Rust state out of their argument. Before this, they left
a hollow-but-callable object behind, so the mistake was silent: `add_user` on a
consumed AuthManager went somewhere the server could never read, and `add` on a
consumed EntityGraph succeeded against a graph nobody would ever query.

The AuthManager case failed *closed* — the hollow manager authenticated nobody —
so this is a usability fix, not a security fix. The failure mode it removes is
"my users mysteriously do not work", reported far from its cause.
"""

import inspect

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


# Sample arguments by method name, so the sweep below can actually call things.
# A method absent from here is called with no arguments; if that raises TypeError
# the sweep fails loudly rather than skipping it, because a method it cannot call
# is a method it cannot prove is guarded.
_ARGS = {
    "add": (lambda: rh.HDict({"id": rh.Ref("x"), "site": rh.Marker()}),),
    "add_grid": (lambda: rh.HGrid(),),
    "get": (lambda: "x",),
    "update": (lambda: "x", lambda: rh.HDict({})),
    "remove": (lambda: "x",),
    "read": (lambda: "site", lambda: 0),
    "changes_since": (lambda: 0,),
    "index_field": (lambda: "site",),
    "refs_from": (lambda: "x",),
    "refs_to": (lambda: "x",),
    "to_grid": (lambda: "",),
    "classify": (lambda: "x",),
    "site_for": (lambda: "x",),
    "children_of": (lambda: "x",),
    "ref_chain": (lambda: "x", lambda: []),
    "equip_points": (lambda: "x",),
    "hierarchy_tree": (lambda: "x", lambda: 3),
    "__contains__": (lambda: "x",),
}

# repr is the one deliberate exception: a repr that raises breaks debuggers and
# tracebacks, which is where you are standing when you hit the poison.
_EXEMPT = {"__repr__", "__init__", "__new__", "__class__", "__doc__"}


def test_every_method_on_a_consumed_graph_raises():
    """Sweep the whole public surface rather than a list someone maintained.

    The first version of this fix inserted guards with a regex that only matched
    single-line signatures, so `ref_chain`, `equip_points` and `hierarchy_tree`
    were silently skipped and still queried the hollow graph. A hand-kept list of
    methods would have had the same blind spot. This walks the type at runtime, so
    a method added later — or one whose signature wraps — cannot slip past.
    """
    live = rh.EntityGraph()
    consumed = rh.EntityGraph()
    rh.SharedGraph(consumed)

    checked, holes = [], []
    for name in dir(live):
        if name in _EXEMPT or name.startswith("_") and name not in _ARGS:
            continue
        attr = inspect.getattr_static(type(live), name, None)
        # Static methods build a fresh graph rather than reading self.inner, so
        # there is nothing to poison. Detected structurally, not hand-listed.
        if isinstance(attr, staticmethod) or not callable(attr):
            continue
        args = tuple(make() for make in _ARGS.get(name, ()))
        try:
            result = getattr(consumed, name)(*args)
        except RuntimeError as e:
            assert "no longer usable" in str(e), f"{name}: wrong error: {e}"
            checked.append(name)
            continue
        except TypeError as e:
            raise AssertionError(
                f"{name} could not be called, so it cannot be proven guarded. "
                f"Add its arguments to _ARGS. ({e})"
            ) from e
        holes.append(f"{name} -> {result!r}")

    assert not holes, "methods that silently used the hollow graph:\n  " + "\n  ".join(holes)
    assert len(checked) >= 20, f"swept too few methods ({len(checked)}); the sweep is not working"


def test_with_auth_does_not_destroy_the_manager_on_a_consumed_server():
    """Poisoning must not happen before the server is known to be live.

    Taking the manager first would burn a perfectly good AuthManager on an
    already-consumed server and still report success — trading one silent failure
    for another.
    """
    server = rh.server.HaystackServer(rh.SharedGraph())
    # An unbindable host makes the background thread fail immediately, which
    # consumes the inner server without leaving a real listener behind.
    server.host("999.999.999.999")
    server.run_background()

    auth = rh.server.AuthManager.from_toml_str(USERS_TOML)
    with pytest.raises(rh.HaystackError, match="already consumed"):
        server.with_auth(auth)

    # The manager must survive, not be left poisoned by a call that did nothing.
    assert auth.is_enabled() is True
    assert repr(auth) == "AuthManager(enabled=true)"
