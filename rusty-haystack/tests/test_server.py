"""Tests for server types: AuthManager, ConnectorConfig, Federation, HisStore, HaystackServer."""

import pytest
import tempfile
import os
import rusty_haystack as rh


class TestAuthManager:
    def test_empty(self):
        auth = rh.server.AuthManager.empty()
        assert auth.is_enabled() is False

    def test_from_toml_str(self):
        toml_content = """
[users.admin]
password_hash = "dGVzdA==:100000:dGVzdA==:dGVzdA=="
role = "admin"
"""
        auth = rh.server.AuthManager.from_toml_str(toml_content)
        assert auth.is_enabled() is True

    def test_from_toml_file(self):
        toml_content = """
[users.viewer]
password_hash = "dGVzdA==:100000:dGVzdA==:dGVzdA=="
role = "viewer"
"""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".toml", delete=False) as f:
            f.write(toml_content)
            f.flush()
            try:
                auth = rh.server.AuthManager.from_toml(f.name)
                assert auth.is_enabled() is True
            finally:
                os.unlink(f.name)

    def test_from_toml_file_not_found(self):
        with pytest.raises(Exception):
            rh.server.AuthManager.from_toml("/nonexistent/path.toml")

    def test_repr(self):
        auth = rh.server.AuthManager.empty()
        assert isinstance(repr(auth), str)


class TestHisStore:
    def test_create(self):
        store = rh.server.HisStore()
        assert store is not None

    def test_len_unknown_id(self):
        store = rh.server.HisStore()
        assert store.len("nonexistent") == 0

    def test_repr(self):
        store = rh.server.HisStore()
        assert isinstance(repr(store), str)


class TestHaystackServer:
    def test_create(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        assert server is not None

    def test_set_port(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        server.port(9090)
        # No assertion needed — just verify no exception

    def test_set_host(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        server.host("127.0.0.1")

    def test_with_auth(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        auth = rh.server.AuthManager.empty()
        server.with_auth(auth)

    def test_with_namespace(self, namespace):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        server.with_namespace(namespace)

    def test_bg_error_before_run(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        assert server.bg_error() is None

    def test_repr(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        assert isinstance(repr(server), str)
