"""Tests for SCRAM SHA-256 auth functions."""

import pytest
import rusty_haystack as rh


class TestGenerateNonce:
    def test_returns_string(self):
        nonce = rh.auth.generate_nonce()
        assert isinstance(nonce, str)
        assert len(nonce) > 0

    def test_unique(self):
        nonces = {rh.auth.generate_nonce() for _ in range(100)}
        assert len(nonces) == 100


class TestClientFirstMessage:
    def test_returns_tuple(self):
        nonce, client_first_b64 = rh.auth.client_first_message("admin")
        assert isinstance(nonce, str)
        assert isinstance(client_first_b64, str)
        assert len(nonce) > 0
        assert len(client_first_b64) > 0

    def test_contains_username(self):
        import base64
        nonce, client_first_b64 = rh.auth.client_first_message("testuser")
        decoded = base64.b64decode(client_first_b64).decode()
        assert "testuser" in decoded


class TestDeriveCredentials:
    def test_returns_bytes_tuple(self):
        salt = b"\x00" * 16
        stored_key, server_key = rh.auth.derive_credentials("password", salt, 4096)
        assert isinstance(stored_key, bytes)
        assert isinstance(server_key, bytes)
        assert len(stored_key) == 32  # SHA-256
        assert len(server_key) == 32

    def test_deterministic(self):
        salt = b"\x01\x02\x03\x04" * 4
        a = rh.auth.derive_credentials("pass", salt, 4096)
        b = rh.auth.derive_credentials("pass", salt, 4096)
        assert a == b

    def test_different_passwords(self):
        salt = b"\x00" * 16
        a = rh.auth.derive_credentials("pass1", salt, 4096)
        b = rh.auth.derive_credentials("pass2", salt, 4096)
        assert a != b

    def test_different_salts(self):
        a = rh.auth.derive_credentials("pass", b"\x00" * 16, 4096)
        b = rh.auth.derive_credentials("pass", b"\x01" * 16, 4096)
        assert a != b


class TestExtractClientNonce:
    def test_roundtrip(self):
        original_nonce, client_first_b64 = rh.auth.client_first_message("user")
        extracted = rh.auth.extract_client_nonce(client_first_b64)
        assert extracted == original_nonce


class TestParseAuthHeader:
    def test_bearer(self):
        result = rh.auth.parse_auth_header("BEARER authToken=abc123")
        assert isinstance(result, dict)
        assert result.get("type") == "bearer"

    def test_hello(self):
        import base64
        username_b64 = base64.b64encode(b"admin").decode()
        data_b64 = base64.b64encode(b"n,,n=admin,r=nonce123").decode()
        header = f"HELLO username={username_b64}, data={data_b64}"
        result = rh.auth.parse_auth_header(header)
        assert isinstance(result, dict)
        assert result.get("type") == "hello"


class TestFormatHelpers:
    def test_format_www_authenticate(self):
        result = rh.auth.format_www_authenticate("token123", "sha-256", "data456")
        assert isinstance(result, str)
        assert "token123" in result

    def test_format_auth_info(self):
        result = rh.auth.format_auth_info("authToken123", "data456")
        assert isinstance(result, str)
        assert "authToken123" in result
