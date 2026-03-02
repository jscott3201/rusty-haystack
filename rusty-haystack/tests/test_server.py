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


class TestConnectorConfig:
    def test_create(self):
        cfg = rh.server.ConnectorConfig(
            name="Building A",
            url="http://building-a:8080/api",
            username="fed",
            password="secret",
        )
        assert cfg.name == "Building A"
        assert cfg.url == "http://building-a:8080/api"

    def test_with_optional_fields(self):
        cfg = rh.server.ConnectorConfig(
            name="Building B",
            url="http://building-b:8080/api",
            username="fed",
            password="secret",
            id_prefix="bldg-b-",
            ws_url="ws://building-b:8080/api/ws",
            sync_interval_secs=30,
        )
        assert cfg.name == "Building B"

    def test_repr(self):
        cfg = rh.server.ConnectorConfig(
            name="Test",
            url="http://test:8080/api",
            username="u",
            password="p",
        )
        assert isinstance(repr(cfg), str)


class TestFederation:
    def test_empty(self):
        fed = rh.server.Federation()
        assert fed.connector_count() == 0

    def test_add_connector(self):
        fed = rh.server.Federation()
        cfg = rh.server.ConnectorConfig(
            name="Test",
            url="http://test:8080/api",
            username="u",
            password="p",
        )
        fed.add(cfg)
        assert fed.connector_count() == 1

    def test_from_toml_str(self):
        toml_content = """
[connectors.building-a]
name = "Building A"
url = "http://building-a:8080/api"
username = "federation"
password = "secret"
id_prefix = "bldg-a-"
sync_interval_secs = 30
"""
        fed = rh.server.Federation.from_toml_str(toml_content)
        assert fed.connector_count() == 1

    def test_status(self):
        fed = rh.server.Federation()
        status = fed.status()
        assert isinstance(status, list)

    def test_filter_cached_empty(self):
        fed = rh.server.Federation()
        cfg = rh.server.ConnectorConfig(
            name="Test",
            url="http://test:8080/api",
            username="u",
            password="p",
        )
        fed.add(cfg)
        result = fed.filter_cached("site")
        assert isinstance(result, rh.HGrid)

    def test_repr(self):
        fed = rh.server.Federation()
        assert isinstance(repr(fed), str)


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

    def test_with_federation(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        fed = rh.server.Federation()
        server.with_federation(fed)

    def test_bg_error_before_run(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        assert server.bg_error() is None

    def test_repr(self):
        graph = rh.SharedGraph()
        server = rh.server.HaystackServer(graph)
        assert isinstance(repr(server), str)
