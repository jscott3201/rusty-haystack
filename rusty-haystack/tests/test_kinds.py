"""Tests for Kind scalar types: Marker, NA, Remove, Number, Ref, Uri, Symbol, XStr, Coord, HDateTime."""

import datetime
import pytest
import rusty_haystack as rh


# ── Marker ──────────────────────────────────────────────────────────────────

class TestMarker:
    def test_create(self):
        m = rh.Marker()
        assert m is not None

    def test_repr(self):
        assert "Marker" in repr(rh.Marker())

    def test_str(self):
        assert isinstance(str(rh.Marker()), str)

    def test_equality(self):
        assert rh.Marker() == rh.Marker()

    def test_inequality_with_other_types(self):
        assert rh.Marker() != rh.NA()

    def test_hash(self):
        s = {rh.Marker(), rh.Marker()}
        assert len(s) == 1

    def test_bool(self):
        assert bool(rh.Marker()) is True


# ── NA ──────────────────────────────────────────────────────────────────────

class TestNA:
    def test_create(self):
        assert rh.NA() is not None

    def test_equality(self):
        assert rh.NA() == rh.NA()

    def test_inequality(self):
        assert rh.NA() != rh.Marker()

    def test_hash(self):
        assert len({rh.NA(), rh.NA()}) == 1

    def test_bool(self):
        assert bool(rh.NA()) is False


# ── Remove ──────────────────────────────────────────────────────────────────

class TestRemove:
    def test_create(self):
        assert rh.Remove() is not None

    def test_equality(self):
        assert rh.Remove() == rh.Remove()

    def test_hash(self):
        assert len({rh.Remove(), rh.Remove()}) == 1


# ── Number ──────────────────────────────────────────────────────────────────

class TestNumber:
    def test_unitless(self):
        n = rh.Number(42)
        assert n.val == 42.0
        assert n.unit is None

    def test_with_unit(self):
        n = rh.Number(72.5, "°F")
        assert n.val == 72.5
        assert n.unit == "°F"

    def test_float(self):
        assert float(rh.Number(3.14)) == pytest.approx(3.14)

    def test_int(self):
        assert int(rh.Number(42.9)) == 42

    def test_equality(self):
        assert rh.Number(42) == rh.Number(42)
        assert rh.Number(42, "°F") == rh.Number(42, "°F")

    def test_inequality(self):
        assert rh.Number(42) != rh.Number(43)
        assert rh.Number(42, "°F") != rh.Number(42, "°C")

    def test_ordering(self):
        assert rh.Number(1) < rh.Number(2)
        assert rh.Number(2) > rh.Number(1)
        assert rh.Number(1) <= rh.Number(1)
        assert rh.Number(1) >= rh.Number(1)

    def test_hash(self):
        assert len({rh.Number(42), rh.Number(42)}) == 1

    def test_repr(self):
        r = repr(rh.Number(42, "°F"))
        assert "42" in r

    def test_negative(self):
        n = rh.Number(-273.15, "°C")
        assert n.val == pytest.approx(-273.15)

    def test_zero(self):
        assert rh.Number(0).val == 0.0


# ── Ref ─────────────────────────────────────────────────────────────────────

class TestRef:
    def test_without_dis(self):
        r = rh.Ref("site-1")
        assert r.val == "site-1"
        assert r.dis is None

    def test_with_dis(self):
        r = rh.Ref("site-1", "My Site")
        assert r.val == "site-1"
        assert r.dis == "My Site"

    def test_equality_ignores_dis(self):
        assert rh.Ref("x", "A") == rh.Ref("x", "B")
        assert rh.Ref("x") == rh.Ref("x", "Display")

    def test_inequality(self):
        assert rh.Ref("a") != rh.Ref("b")

    def test_hash_ignores_dis(self):
        assert hash(rh.Ref("x", "A")) == hash(rh.Ref("x", "B"))

    def test_repr(self):
        assert "site-1" in repr(rh.Ref("site-1"))

    def test_str(self):
        assert "site-1" in str(rh.Ref("site-1"))


# ── Uri ─────────────────────────────────────────────────────────────────────

class TestUri:
    def test_create(self):
        u = rh.Uri("http://example.com")
        assert u.val == "http://example.com"

    def test_equality(self):
        assert rh.Uri("a") == rh.Uri("a")
        assert rh.Uri("a") != rh.Uri("b")

    def test_hash(self):
        assert len({rh.Uri("x"), rh.Uri("x")}) == 1

    def test_repr(self):
        assert "example.com" in repr(rh.Uri("http://example.com"))


# ── Symbol ──────────────────────────────────────────────────────────────────

class TestSymbol:
    def test_create(self):
        s = rh.Symbol("hot-water")
        assert s.val == "hot-water"

    def test_equality(self):
        assert rh.Symbol("a") == rh.Symbol("a")
        assert rh.Symbol("a") != rh.Symbol("b")

    def test_hash(self):
        assert len({rh.Symbol("x"), rh.Symbol("x")}) == 1


# ── XStr ────────────────────────────────────────────────────────────────────

class TestXStr:
    def test_create(self):
        x = rh.XStr("Bin", "base64data")
        assert x.type_name == "Bin"
        assert x.val == "base64data"

    def test_equality(self):
        assert rh.XStr("Bin", "a") == rh.XStr("Bin", "a")
        assert rh.XStr("Bin", "a") != rh.XStr("Bin", "b")
        assert rh.XStr("Bin", "a") != rh.XStr("Hex", "a")

    def test_hash(self):
        assert len({rh.XStr("Bin", "a"), rh.XStr("Bin", "a")}) == 1


# ── Coord ───────────────────────────────────────────────────────────────────

class TestCoord:
    def test_create(self):
        c = rh.Coord(40.7128, -74.0060)
        assert c.lat == pytest.approx(40.7128)
        assert c.lng == pytest.approx(-74.0060)

    def test_equality(self):
        assert rh.Coord(1.0, 2.0) == rh.Coord(1.0, 2.0)
        assert rh.Coord(1.0, 2.0) != rh.Coord(1.0, 3.0)

    def test_hash(self):
        assert len({rh.Coord(1, 2), rh.Coord(1, 2)}) == 1

    def test_repr(self):
        r = repr(rh.Coord(40.7, -74.0))
        assert "40.7" in r

    def test_boundary_values(self):
        rh.Coord(90.0, 180.0)
        rh.Coord(-90.0, -180.0)
        rh.Coord(0.0, 0.0)


# ── HDateTime ──────────────────────────────────────────────────────────────

class TestHDateTime:
    def test_create(self):
        dt = rh.HDateTime(2024, 1, 15, 10, 30, 0, -18000, "New_York")
        assert dt.tz_name == "New_York"

    def test_to_python_datetime(self):
        dt = rh.HDateTime(2024, 6, 15, 14, 30, 45, 0, "UTC")
        py_dt = dt.dt()
        assert isinstance(py_dt, datetime.datetime)
        assert py_dt.year == 2024
        assert py_dt.month == 6
        assert py_dt.day == 15
        assert py_dt.hour == 14
        assert py_dt.minute == 30
        assert py_dt.second == 45

    def test_ordering(self):
        a = rh.HDateTime(2024, 1, 1, 0, 0, 0, 0, "UTC")
        b = rh.HDateTime(2024, 12, 31, 23, 59, 59, 0, "UTC")
        assert a < b
        assert b > a

    def test_equality(self):
        a = rh.HDateTime(2024, 1, 1, 0, 0, 0, 0, "UTC")
        b = rh.HDateTime(2024, 1, 1, 0, 0, 0, 0, "UTC")
        assert a == b

    def test_hash(self):
        a = rh.HDateTime(2024, 1, 1, 0, 0, 0, 0, "UTC")
        b = rh.HDateTime(2024, 1, 1, 0, 0, 0, 0, "UTC")
        assert hash(a) == hash(b)

    def test_repr(self):
        dt = rh.HDateTime(2024, 1, 15, 10, 30, 0, 0, "UTC")
        assert "2024" in repr(dt)
