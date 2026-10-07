"""Tests for EntityGraph, SharedGraph, GraphDiff, DiffOp."""

import pytest
import rusty_haystack as rh


class TestEntityGraph:
    def test_empty(self):
        g = rh.EntityGraph()
        assert g.is_empty()
        assert len(g) == 0

    def test_add(self, site_dict):
        g = rh.EntityGraph()
        ref_val = g.add(site_dict)
        assert ref_val == "site-1"
        assert len(g) == 1
        assert not g.is_empty()

    def test_add_requires_id(self):
        g = rh.EntityGraph()
        with pytest.raises((ValueError, rh.GraphError)):
            g.add(rh.HDict({"dis": "No ID"}))

    def test_get(self, sample_graph):
        entity = sample_graph.get("site-1")
        assert entity is not None
        assert entity["dis"] == "Demo Site"

    def test_get_missing(self, sample_graph):
        assert sample_graph.get("nonexistent") is None

    def test_update(self, sample_graph):
        sample_graph.update("site-1", rh.HDict({"dis": "Updated"}))
        entity = sample_graph.get("site-1")
        assert entity["dis"] == "Updated"

    def test_remove(self, sample_graph):
        removed = sample_graph.remove("site-1")
        assert removed["dis"] == "Demo Site"
        assert sample_graph.get("site-1") is None
        assert len(sample_graph) == 2

    def test_contains(self, sample_graph):
        assert "site-1" in sample_graph
        assert "nonexistent" not in sample_graph

    def test_read_filter(self, sample_graph):
        results = sample_graph.read("site")
        assert len(results) == 1
        assert results[0]["dis"] == "Demo Site"

    def test_read_with_limit(self, sample_graph):
        results = sample_graph.read("point or equip or site", limit=1)
        assert len(results) == 1

    def test_read_no_results(self, sample_graph):
        results = sample_graph.read("boiler")
        assert len(results) == 0

    def test_all(self, sample_graph):
        entities = sample_graph.all()
        assert len(entities) == 3

    def test_refs_from(self, sample_graph):
        refs = sample_graph.refs_from("ahu-1")
        assert "site-1" in refs

    def test_refs_from_typed(self, sample_graph):
        refs = sample_graph.refs_from("ahu-1", ref_type="siteRef")
        assert "site-1" in refs

    def test_refs_to(self, sample_graph):
        refs = sample_graph.refs_to("site-1")
        assert "ahu-1" in refs

    def test_to_grid(self, sample_graph):
        grid = sample_graph.to_grid()
        assert len(grid) == 3

    def test_to_grid_filtered(self, sample_graph):
        grid = sample_graph.to_grid("site")
        assert len(grid) == 1

    def test_version(self, sample_graph):
        v1 = sample_graph.version
        sample_graph.update("site-1", rh.HDict({"dis": "V2"}))
        v2 = sample_graph.version
        assert v2 > v1

    def test_changes_since(self, sample_graph):
        v = sample_graph.version
        sample_graph.update("site-1", rh.HDict({"dis": "Changed"}))
        diffs = sample_graph.changes_since(v)
        assert len(diffs) >= 1

    def test_add_grid(self, sample_graph):
        grid = rh.HGrid.from_parts(
            rh.HDict(),
            [rh.HCol("id"), rh.HCol("dis"), rh.HCol("site")],
            [rh.HDict({"id": rh.Ref("site-99"), "dis": "New", "site": rh.Marker()})],
        )
        count = sample_graph.add_grid(grid)
        assert count == 1
        assert sample_graph.get("site-99") is not None

    def test_from_grid(self):
        grid = rh.HGrid.from_parts(
            rh.HDict(),
            [rh.HCol("id"), rh.HCol("dis")],
            [
                rh.HDict({"id": rh.Ref("a"), "dis": "A"}),
                rh.HDict({"id": rh.Ref("b"), "dis": "B"}),
            ],
        )
        g = rh.EntityGraph.from_grid(grid)
        assert len(g) == 2

    def test_repr(self, sample_graph):
        assert isinstance(repr(sample_graph), str)


class TestGraphDiff:
    def test_diff_on_add(self):
        g = rh.EntityGraph()
        v_before = g.version
        g.add(rh.HDict({"id": rh.Ref("x"), "dis": "X"}))
        diffs = g.changes_since(v_before)
        assert len(diffs) == 1
        diff = diffs[0]
        assert diff.op == rh.graph.DiffOp.Add
        assert diff.ref_val == "x"
        assert diff.old is None
        assert diff.new is not None

    def test_diff_on_update(self):
        g = rh.EntityGraph()
        g.add(rh.HDict({"id": rh.Ref("x"), "dis": "Before"}))
        v = g.version
        g.update("x", rh.HDict({"dis": "After"}))
        diffs = g.changes_since(v)
        assert len(diffs) == 1
        d = diffs[0]
        assert d.op == rh.graph.DiffOp.Update
        # An Update carries its payload in changed_tags/previous_tags. `old` and
        # `new` are only populated for Remove and Add respectively.
        assert d.old is None
        assert d.new is None
        assert d.changed_tags is not None
        assert d.changed_tags.get("dis") == "After"
        assert d.previous_tags is not None
        assert d.previous_tags.get("dis") == "Before"

    def test_diff_on_remove(self):
        g = rh.EntityGraph()
        g.add(rh.HDict({"id": rh.Ref("x"), "dis": "X"}))
        v = g.version
        g.remove("x")
        diffs = g.changes_since(v)
        assert len(diffs) == 1
        assert diffs[0].op == rh.graph.DiffOp.Remove


class TestDiffOp:
    def test_variants(self):
        assert rh.graph.DiffOp.Add is not None
        assert rh.graph.DiffOp.Update is not None
        assert rh.graph.DiffOp.Remove is not None

    def test_repr(self):
        assert isinstance(repr(rh.graph.DiffOp.Add), str)


class TestSharedGraph:
    def test_empty(self):
        sg = rh.SharedGraph()
        assert sg.is_empty()
        assert len(sg) == 0

    def test_from_entity_graph(self, sample_graph):
        sg = rh.SharedGraph(sample_graph)
        assert len(sg) == 3

    def test_add(self):
        sg = rh.SharedGraph()
        ref_val = sg.add(rh.HDict({"id": rh.Ref("s-1"), "site": rh.Marker()}))
        assert ref_val == "s-1"
        assert len(sg) == 1

    def test_get(self):
        sg = rh.SharedGraph()
        sg.add(rh.HDict({"id": rh.Ref("s-1"), "dis": "Test"}))
        e = sg.get("s-1")
        assert e is not None
        assert e["dis"] == "Test"

    def test_update(self):
        sg = rh.SharedGraph()
        sg.add(rh.HDict({"id": rh.Ref("s-1"), "dis": "Before"}))
        sg.update("s-1", rh.HDict({"dis": "After"}))
        assert sg.get("s-1")["dis"] == "After"

    def test_remove(self):
        sg = rh.SharedGraph()
        sg.add(rh.HDict({"id": rh.Ref("s-1"), "dis": "X"}))
        removed = sg.remove("s-1")
        assert removed["dis"] == "X"
        assert sg.is_empty()

    def test_read(self):
        sg = rh.SharedGraph()
        sg.add(rh.HDict({"id": rh.Ref("s-1"), "site": rh.Marker()}))
        sg.add(rh.HDict({"id": rh.Ref("e-1"), "equip": rh.Marker()}))
        results = sg.read("site")
        assert len(results) == 1

    def test_contains(self):
        sg = rh.SharedGraph()
        sg.add(rh.HDict({"id": rh.Ref("s-1")}))
        assert sg.contains("s-1")
        assert not sg.contains("missing")

    def test_all(self):
        sg = rh.SharedGraph()
        sg.add(rh.HDict({"id": rh.Ref("a")}))
        sg.add(rh.HDict({"id": rh.Ref("b")}))
        assert len(sg.all()) == 2

    def test_refs_from_to(self):
        sg = rh.SharedGraph()
        sg.add(rh.HDict({"id": rh.Ref("site-1"), "site": rh.Marker()}))
        sg.add(rh.HDict({
            "id": rh.Ref("equip-1"),
            "equip": rh.Marker(),
            "siteRef": rh.Ref("site-1"),
        }))
        assert "site-1" in sg.refs_from("equip-1")
        assert "equip-1" in sg.refs_to("site-1")

    def test_version_and_changes(self):
        sg = rh.SharedGraph()
        v = sg.version
        sg.add(rh.HDict({"id": rh.Ref("x")}))
        assert sg.version > v
        diffs = sg.changes_since(v)
        assert len(diffs) >= 1

    def test_from_grid(self):
        grid = rh.HGrid.from_parts(
            rh.HDict(),
            [rh.HCol("id"), rh.HCol("dis")],
            [rh.HDict({"id": rh.Ref("a"), "dis": "A"})],
        )
        sg = rh.SharedGraph.from_grid(grid)
        assert len(sg) == 1

    def test_repr(self):
        sg = rh.SharedGraph()
        assert isinstance(repr(sg), str)


def result_ids(graph, expression):
    return sorted(row["id"].val for row in graph.read(expression))


@pytest.mark.parametrize("graph_type", [rh.EntityGraph, rh.SharedGraph])
class TestGraphCorrectness:
    @pytest.mark.parametrize("invalid_id", [rh.Ref("renamed"), rh.Remove(), "equip"])
    def test_rejected_identity_patch_preserves_every_view(self, graph_type, invalid_id):
        graph = graph_type()
        graph.add(rh.HDict({
            "id": rh.Ref("equip", "Original display"),
            "siteRef": rh.Ref("site", "Original site"),
            "curVal": rh.Number(21),
        }))
        version = graph.version
        for _ in range(2):
            assert result_ids(graph, "curVal == 21") == ["equip"]
        with pytest.raises(rh.GraphError, match="id cannot"):
            graph.update("equip", rh.HDict({
                "id": invalid_id,
                "siteRef": rh.Ref("other"),
                "curVal": rh.Number(99),
                "newTag": rh.Marker(),
            }))
        assert graph.version == version
        assert len(graph) == 1
        assert graph.get("equip")["id"].dis == "Original display"
        assert graph.get("equip")["siteRef"].dis == "Original site"
        assert graph.get("renamed") is None
        assert result_ids(graph, "curVal == 21") == ["equip"]
        assert result_ids(graph, "curVal == 99 or newTag") == []
        assert graph.refs_from("equip") == ["site"]
        assert graph.refs_to("site") == ["equip"]
        assert graph.refs_to("other") == []
        assert graph.changes_since(version) == []

    def test_same_id_full_row_and_display_updates(self, graph_type):
        graph = graph_type()
        row = rh.HDict({"id": rh.Ref("equip", "Old"), "curVal": rh.Number(21)})
        graph.add(row)
        assert result_ids(graph, "curVal == 21") == ["equip"]
        graph.update("equip", row)
        assert graph.version == 2
        graph.update("equip", rh.HDict({"id": rh.Ref("equip", "New")}))
        assert graph.version == 3
        assert graph.get("equip")["id"].dis == "New"
        assert next(iter(graph.read("curVal == 21")))["id"].dis == "New"
        diff, = graph.changes_since(2)
        assert diff.previous_tags["id"].dis == "Old"
        assert diff.changed_tags["id"].dis == "New"

    def test_empty_update_checks_existence(self, graph_type):
        graph = graph_type()
        graph.add(rh.HDict({"id": rh.Ref("equip")}))
        graph.update("equip", rh.HDict())
        with pytest.raises(rh.GraphError, match="not found"):
            graph.update("missing", rh.HDict())
        assert graph.version == 1
        assert graph.changes_since(1) == []

    def test_ref_query_cache_tracks_replacement_removal_and_readdition(self, graph_type):
        graph = graph_type()
        graph.add(rh.HDict({"id": rh.Ref("site"), "site": rh.Marker()}))
        graph.add(rh.HDict({"id": rh.Ref("equip"), "siteRef": rh.Ref("site", "Label")}))
        for _ in range(2):
            assert result_ids(graph, 'siteRef == @site "Different label"') == ["equip"]
            assert result_ids(graph, "siteRef->site") == ["equip"]
        graph.update("equip", rh.HDict({"siteRef": "site"}))
        assert graph.refs_from("equip") == []
        assert graph.refs_to("site") == []
        assert result_ids(graph, 'siteRef == @site "Different label"') == []
        assert result_ids(graph, "siteRef->site") == []
        graph.remove("equip")
        graph.add(rh.HDict({"id": rh.Ref("equip"), "siteRef": rh.Ref("site")}))
        assert result_ids(graph, "siteRef->site") == ["equip"]
        assert graph.refs_to("site") == ["equip"]

    def test_signed_zero_and_mixed_kind_inequality(self, graph_type):
        graph = graph_type()
        for ref, value in [
            ("minus", rh.Number(-0.0)),
            ("plus", rh.Number(0.0)),
            ("unit", rh.Number(0.0, "kW")),
            ("ref", rh.Ref("target")),
        ]:
            graph.add(rh.HDict({"id": rh.Ref(ref), "curVal": value}))
        assert result_ids(graph, "curVal >= 0") == ["minus", "plus"]
        assert result_ids(graph, "curVal == 0") == ["plus"]
        assert result_ids(graph, "curVal != 0") == ["minus", "ref", "unit"]
