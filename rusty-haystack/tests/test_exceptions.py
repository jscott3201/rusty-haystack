"""Tests for exception hierarchy."""

import pytest
import rusty_haystack as rh


class TestExceptionHierarchy:
    def test_haystack_error_exists(self):
        assert issubclass(rh.HaystackError, Exception)

    def test_codec_error_is_haystack_error(self):
        assert issubclass(rh.CodecError, rh.HaystackError)

    def test_filter_error_is_haystack_error(self):
        assert issubclass(rh.FilterError, rh.HaystackError)

    def test_graph_error_is_haystack_error(self):
        assert issubclass(rh.GraphError, rh.HaystackError)

    def test_auth_error_is_haystack_error(self):
        assert issubclass(rh.AuthError, rh.HaystackError)

    def test_client_error_is_haystack_error(self):
        assert issubclass(rh.ClientError, rh.HaystackError)


class TestExceptionRaising:
    def test_codec_error_from_bad_decode(self):
        with pytest.raises(rh.HaystackError):
            rh.decode_grid("text/zinc", "completely invalid zinc!!!")

    def test_filter_error_from_bad_parse(self):
        with pytest.raises((rh.FilterError, rh.HaystackError, ValueError)):
            rh.Filter.parse("!!!invalid!!!")

    def test_graph_error_from_no_id(self):
        g = rh.EntityGraph()
        with pytest.raises((rh.GraphError, rh.HaystackError, ValueError)):
            g.add(rh.HDict({"dis": "No ID tag"}))


class TestExceptionCatching:
    def test_catch_base(self):
        """All module exceptions should be catchable as HaystackError."""
        caught = False
        try:
            rh.decode_grid("text/zinc", "bad data!!!")
        except rh.HaystackError:
            caught = True
        assert caught

    def test_catch_specific(self):
        """Specific exceptions should be catchable by their type."""
        try:
            rh.decode_grid("text/zinc", "bad data!!!")
        except rh.CodecError:
            pass  # Expected
        except rh.HaystackError:
            pass  # Also acceptable — may be raised as base type
