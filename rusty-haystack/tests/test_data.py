"""Tests for data structures: HDict, HGrid, HList, HCol."""

import pytest
import rusty_haystack as rh


# ── HDict ───────────────────────────────────────────────────────────────────

class TestHDict:
    def test_empty(self):
        d = rh.HDict()
        assert d.is_empty()
        assert len(d) == 0

    def test_from_dict(self):
        d = rh.HDict({"dis": "Hello", "site": rh.Marker()})
        assert len(d) == 2
        assert d["dis"] == "Hello"

    def test_has_and_missing(self):
        d = rh.HDict({"site": rh.Marker()})
        assert d.has("site")
        assert d.missing("equip")
        assert not d.has("equip")
        assert not d.missing("site")

    def test_get_existing(self):
        d = rh.HDict({"dis": "Hello"})
        assert d.get("dis") == "Hello"

    def test_get_missing_returns_none(self):
        d = rh.HDict()
        assert d.get("missing") is None

    def test_id_and_dis(self):
        d = rh.HDict({
            "id": rh.Ref("site-1", "My Site"),
            "dis": "My Site",
        })
        assert d.id() == rh.Ref("site-1")
        assert d.dis() == "My Site"

    def test_id_none_when_missing(self):
        d = rh.HDict({"dis": "No ID"})
        assert d.id() is None

    def test_set(self):
        d = rh.HDict()
        d.set("tag", rh.Marker())
        assert d.has("tag")

    def test_setitem(self):
        d = rh.HDict()
        d["name"] = "test"
        assert d["name"] == "test"

    def test_delitem(self):
        d = rh.HDict({"a": "1", "b": "2"})
        del d["a"]
        assert d.missing("a")
        assert len(d) == 1

    def test_contains(self):
        d = rh.HDict({"site": rh.Marker()})
        assert "site" in d
        assert "equip" not in d

    def test_merge(self):
        a = rh.HDict({"x": "1"})
        b = rh.HDict({"y": "2", "x": "overwritten"})
        a.merge(b)
        assert a["x"] == "overwritten"
        assert a["y"] == "2"

    def test_keys_values_items(self):
        d = rh.HDict({"a": "1", "b": "2"})
        keys = d.keys()
        assert set(keys) == {"a", "b"}
        assert len(d.values()) == 2
        items = d.items()
        assert len(items) == 2
        assert all(isinstance(k, str) for k, _ in items)

    def test_tag_names(self):
        d = rh.HDict({"site": rh.Marker(), "dis": "X"})
        names = d.tag_names()
        assert set(names) == {"site", "dis"}

    def test_copy(self):
        original = rh.HDict({"dis": "Original"})
        clone = original.copy()
        clone["dis"] = "Clone"
        assert original["dis"] == "Original"
        assert clone["dis"] == "Clone"

    def test_iteration(self):
        d = rh.HDict({"a": "1", "b": "2", "c": "3"})
        keys = list(d)
        assert len(keys) == 3

    def test_repr(self):
        d = rh.HDict({"dis": "Test"})
        assert "dis" in repr(d)

    def test_equality(self):
        a = rh.HDict({"x": "1"})
        b = rh.HDict({"x": "1"})
        assert a == b

    def test_inequality(self):
        a = rh.HDict({"x": "1"})
        b = rh.HDict({"x": "2"})
        assert a != b

    def test_various_value_types(self):
        d = rh.HDict({
            "str_val": "hello",
            "num_val": rh.Number(42, "°F"),
            "bool_val": True,
            "marker_val": rh.Marker(),
            "ref_val": rh.Ref("r-1"),
            "coord_val": rh.Coord(1.0, 2.0),
        })
        assert len(d) == 6
        assert d["str_val"] == "hello"
        assert d["bool_val"] is True

    def test_none_value(self):
        d = rh.HDict({"x": None})
        # None maps to Null
        assert d.get("x") is None or d.has("x")

    def test_int_float_coercion(self):
        d = rh.HDict({"n": 42})
        # Integers become Number(unitless)
        val = d.get("n")
        assert val is not None


# ── HCol ────────────────────────────────────────────────────────────────────

class TestHCol:
    def test_create(self):
        col = rh.HCol("temperature")
        assert col.name == "temperature"

    def test_repr(self):
        assert "temperature" in repr(rh.HCol("temperature"))

    def test_meta(self):
        meta = rh.HDict({"unit": "°F"})
        col = rh.HCol("temp", meta)
        assert col.meta.has("unit")

    def test_meta_default_empty(self):
        col = rh.HCol("temp")
        assert col.meta.is_empty()


# ── HGrid ───────────────────────────────────────────────────────────────────

class TestHGrid:
    def test_empty(self):
        g = rh.HGrid()
        assert g.is_empty()
        assert len(g) == 0
        assert g.num_cols() == 0

    def test_from_parts(self):
        meta = rh.HDict({"ver": "3.0"})
        cols = [rh.HCol("id"), rh.HCol("dis")]
        rows = [rh.HDict({"id": rh.Ref("a"), "dis": "A"})]
        g = rh.HGrid.from_parts(meta, cols, rows)
        assert len(g) == 1
        assert g.num_cols() == 2

    def test_add_row(self):
        g = rh.HGrid()
        g.add_col("dis")
        g.add_row(rh.HDict({"dis": "Test"}))
        assert len(g) == 1

    def test_set_meta(self):
        g = rh.HGrid()
        g.set_meta(rh.HDict({"ver": "3.0"}))
        assert g.meta().has("ver")

    def test_col_names(self):
        meta = rh.HDict()
        cols = [rh.HCol("id"), rh.HCol("dis"), rh.HCol("site")]
        g = rh.HGrid.from_parts(meta, cols, [])
        assert g.col_names() == ["id", "dis", "site"]

    def test_col_lookup(self):
        meta = rh.HDict()
        cols = [rh.HCol("id"), rh.HCol("dis")]
        g = rh.HGrid.from_parts(meta, cols, [])
        assert g.col("id") is not None
        assert g.col("missing") is None

    def test_indexing(self):
        rows = [
            rh.HDict({"id": rh.Ref("a"), "dis": "A"}),
            rh.HDict({"id": rh.Ref("b"), "dis": "B"}),
        ]
        g = rh.HGrid.from_parts(rh.HDict(), [rh.HCol("id"), rh.HCol("dis")], rows)
        assert g[0]["dis"] == "A"
        assert g[1]["dis"] == "B"

    def test_negative_indexing(self):
        rows = [
            rh.HDict({"dis": "First"}),
            rh.HDict({"dis": "Last"}),
        ]
        g = rh.HGrid.from_parts(rh.HDict(), [rh.HCol("dis")], rows)
        assert g[-1]["dis"] == "Last"

    def test_iteration(self):
        rows = [rh.HDict({"n": str(i)}) for i in range(5)]
        g = rh.HGrid.from_parts(rh.HDict(), [rh.HCol("n")], rows)
        collected = list(g)
        assert len(collected) == 5

    def test_is_err(self):
        g = rh.HGrid()
        assert not g.is_err()

    def test_meta_default_empty(self):
        g = rh.HGrid()
        assert g.meta().is_empty()

    def test_repr(self):
        g = rh.HGrid()
        assert isinstance(repr(g), str)

    def test_rows(self):
        rows = [rh.HDict({"dis": "A"}), rh.HDict({"dis": "B"})]
        g = rh.HGrid.from_parts(rh.HDict(), [rh.HCol("dis")], rows)
        assert len(g.rows()) == 2

    def test_cols(self):
        g = rh.HGrid.from_parts(rh.HDict(), [rh.HCol("a"), rh.HCol("b")], [])
        assert len(g.cols()) == 2


# ── HList ───────────────────────────────────────────────────────────────────

class TestHList:
    def test_empty(self):
        lst = rh.HList()
        assert lst.is_empty()
        assert len(lst) == 0

    def test_from_items(self):
        lst = rh.HList([rh.Number(1), rh.Number(2), rh.Number(3)])
        assert len(lst) == 3

    def test_indexing(self):
        lst = rh.HList([rh.Number(10), rh.Number(20)])
        assert lst[0] == rh.Number(10)
        assert lst[1] == rh.Number(20)

    def test_negative_indexing(self):
        lst = rh.HList([rh.Number(10), rh.Number(20)])
        assert lst[-1] == rh.Number(20)

    def test_push(self):
        lst = rh.HList()
        lst.push(rh.Number(42))
        assert len(lst) == 1
        assert lst[0] == rh.Number(42)

    def test_extend(self):
        a = rh.HList([rh.Number(1)])
        b = rh.HList([rh.Number(2), rh.Number(3)])
        a.extend(b)
        assert len(a) == 3

    def test_clear(self):
        lst = rh.HList([rh.Number(1), rh.Number(2)])
        lst.clear()
        assert lst.is_empty()

    def test_iteration(self):
        lst = rh.HList([rh.Number(i) for i in range(5)])
        collected = list(lst)
        assert len(collected) == 5

    def test_repr(self):
        lst = rh.HList([rh.Number(1)])
        assert isinstance(repr(lst), str)

    def test_equality(self):
        a = rh.HList([rh.Number(1), rh.Number(2)])
        b = rh.HList([rh.Number(1), rh.Number(2)])
        assert a == b

    def test_inequality(self):
        a = rh.HList([rh.Number(1)])
        b = rh.HList([rh.Number(2)])
        assert a != b

    def test_mixed_types(self):
        lst = rh.HList([rh.Number(1), "hello", rh.Marker(), rh.Ref("x")])
        assert len(lst) == 4
