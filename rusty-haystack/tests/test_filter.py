"""Tests for filter engine: parse_filter, matches_filter, Filter builder, Path, CmpOp."""

import pytest
import rusty_haystack as rh


class TestParseFilter:
    def test_simple_tag(self):
        result = rh.parse_filter("site")
        assert isinstance(result, str)
        assert len(result) > 0

    def test_compound(self):
        result = rh.parse_filter("site and area > 1000")
        assert isinstance(result, str)

    def test_invalid_filter(self):
        with pytest.raises((ValueError, rh.FilterError)):
            rh.parse_filter("!!!invalid!!!")


class TestMatchesFilter:
    def test_marker_match(self):
        e = rh.HDict({"site": rh.Marker(), "dis": "Test"})
        assert rh.matches_filter("site", e) is True
        assert rh.matches_filter("equip", e) is False

    def test_comparison(self):
        e = rh.HDict({"area": rh.Number(5000)})
        assert rh.matches_filter("area > 1000", e) is True
        assert rh.matches_filter("area < 1000", e) is False
        assert rh.matches_filter("area == 5000", e) is True

    def test_string_comparison(self):
        e = rh.HDict({"dis": "Main Site"})
        assert rh.matches_filter('dis == "Main Site"', e) is True
        assert rh.matches_filter('dis == "Other"', e) is False

    def test_and_filter(self):
        e = rh.HDict({"site": rh.Marker(), "area": rh.Number(5000)})
        assert rh.matches_filter("site and area > 1000", e) is True
        assert rh.matches_filter("site and area < 1000", e) is False

    def test_or_filter(self):
        e = rh.HDict({"site": rh.Marker()})
        assert rh.matches_filter("site or equip", e) is True
        assert rh.matches_filter("equip or ahu", e) is False

    def test_not_filter(self):
        e = rh.HDict({"site": rh.Marker()})
        assert rh.matches_filter("not equip", e) is True
        assert rh.matches_filter("not site", e) is False

    def test_invalid_filter(self):
        e = rh.HDict({"site": rh.Marker()})
        with pytest.raises((ValueError, rh.FilterError)):
            rh.matches_filter("!!!bad!!!", e)


class TestPath:
    def test_single(self):
        p = rh.filter.Path.single("siteRef")
        assert p.first() == "siteRef"
        assert p.is_single()
        assert len(p) == 1

    def test_multi_segment(self):
        p = rh.filter.Path(["equipRef", "siteRef", "dis"])
        assert len(p) == 3
        assert p.first() == "equipRef"
        assert not p.is_single()

    def test_segments(self):
        p = rh.filter.Path(["a", "b", "c"])
        assert p.segments == ["a", "b", "c"]

    def test_repr(self):
        p = rh.filter.Path.single("siteRef")
        assert "siteRef" in repr(p)


class TestCmpOp:
    def test_variants_exist(self):
        assert rh.filter.CmpOp.Eq is not None
        assert rh.filter.CmpOp.Ne is not None
        assert rh.filter.CmpOp.Lt is not None
        assert rh.filter.CmpOp.Le is not None
        assert rh.filter.CmpOp.Gt is not None
        assert rh.filter.CmpOp.Ge is not None

    def test_repr(self):
        assert isinstance(repr(rh.filter.CmpOp.Eq), str)


class TestFilterBuilder:
    def test_has(self):
        f = rh.Filter.has("site")
        e = rh.HDict({"site": rh.Marker()})
        assert f.matches(e) is True
        assert f.node_type == "has"

    def test_missing(self):
        f = rh.Filter.missing("deprecated")
        e = rh.HDict({"site": rh.Marker()})
        assert f.matches(e) is True
        assert f.node_type == "missing"

    def test_cmp(self):
        p = rh.filter.Path.single("area")
        f = rh.Filter.cmp(p, rh.filter.CmpOp.Gt, rh.Number(1000))
        e = rh.HDict({"area": rh.Number(5000)})
        assert f.matches(e) is True

    def test_eq_shorthand(self):
        f = rh.Filter.eq("dis", "Main Site")
        e = rh.HDict({"dis": "Main Site"})
        assert f.matches(e) is True

    def test_and(self):
        f = rh.Filter.has("site").and_(rh.Filter.has("area"))
        e = rh.HDict({"site": rh.Marker(), "area": rh.Number(5000)})
        assert f.matches(e) is True
        assert f.node_type == "and"

    def test_or(self):
        f = rh.Filter.has("site").or_(rh.Filter.has("equip"))
        e = rh.HDict({"equip": rh.Marker()})
        assert f.matches(e) is True
        assert f.node_type == "or"

    def test_operator_and(self):
        f = rh.Filter.has("site") & rh.Filter.has("area")
        e = rh.HDict({"site": rh.Marker(), "area": rh.Number(100)})
        assert f.matches(e) is True

    def test_operator_or(self):
        f = rh.Filter.has("site") | rh.Filter.has("equip")
        e = rh.HDict({"site": rh.Marker()})
        assert f.matches(e) is True

    def test_parse(self):
        f = rh.Filter.parse("site and area > 1000")
        e = rh.HDict({"site": rh.Marker(), "area": rh.Number(5000)})
        assert f.matches(e) is True

    def test_parse_invalid(self):
        with pytest.raises((ValueError, rh.FilterError)):
            rh.Filter.parse("!!!invalid!!!")

    def test_str(self):
        f = rh.Filter.has("site")
        s = str(f)
        assert isinstance(s, str)
        assert len(s) > 0

    def test_path_property(self):
        f = rh.Filter.has("site")
        assert f.path is not None
        assert f.path.first() == "site"

    def test_children(self):
        f = rh.Filter.has("site").and_(rh.Filter.has("equip"))
        children = f.children()
        assert children is not None
        assert len(children) == 2

    def test_val(self):
        f = rh.Filter.eq("dis", "hello")
        v = f.val()
        assert v == "hello"

    def test_no_match(self):
        f = rh.Filter.has("site")
        e = rh.HDict({"equip": rh.Marker()})
        assert f.matches(e) is False
