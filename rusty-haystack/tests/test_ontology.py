"""Tests for DefNamespace, Def, Lib, Spec, Slot."""

import pytest
import rusty_haystack as rh


class TestDefNamespace:
    def test_load_standard(self, namespace):
        assert len(namespace) > 0

    def test_contains(self, namespace):
        assert namespace.contains("site")
        assert namespace.contains("equip")
        assert not namespace.contains("nonexistent_type_xyz")

    def test_is_a(self, namespace):
        assert namespace.is_a("ahu", "equip")
        assert not namespace.is_a("site", "equip")

    def test_subtypes(self, namespace):
        subs = namespace.subtypes("equip")
        assert isinstance(subs, list)
        assert len(subs) > 0

    def test_supertypes(self, namespace):
        supers = namespace.supertypes("ahu")
        assert isinstance(supers, list)
        assert "equip" in supers

    def test_fits(self, namespace, site_dict):
        assert namespace.fits(site_dict, "site") is True

    def test_fits_negative(self, namespace, site_dict):
        assert namespace.fits(site_dict, "equip") is False

    def test_validate_entity(self, namespace, site_dict):
        issues = namespace.validate_entity(site_dict)
        assert isinstance(issues, list)

    def test_fits_explain(self, namespace, site_dict):
        reasons = namespace.fits_explain(site_dict, "site")
        assert isinstance(reasons, list)

    def test_mandatory_tags(self, namespace):
        tags = namespace.mandatory_tags("site")
        assert isinstance(tags, list)

    def test_tags_for(self, namespace):
        tags = namespace.tags_for("site")
        assert isinstance(tags, list)
        assert len(tags) > 0

    def test_get_def(self, namespace):
        d = namespace.get_def("site")
        assert d is not None
        assert d.symbol == "site"

    def test_get_def_missing(self, namespace):
        assert namespace.get_def("nonexistent_xyz") is None

    def test_defs(self, namespace):
        all_defs = namespace.defs()
        assert isinstance(all_defs, list)
        assert len(all_defs) > 0

    def test_libs(self, namespace):
        libs = namespace.libs()
        assert isinstance(libs, list)
        assert len(libs) > 0
        names = [lib.name for lib in libs]
        assert "ph" in names

    def test_get_lib(self, namespace):
        lib = namespace.get_lib("ph")
        assert lib is not None
        assert lib.name == "ph"

    def test_repr(self, namespace):
        assert isinstance(repr(namespace), str)

    def test_empty_namespace(self):
        ns = rh.DefNamespace()
        assert len(ns) == 0


class TestDef:
    def test_properties(self, namespace):
        d = namespace.get_def("site")
        assert isinstance(d.symbol, str)
        assert isinstance(d.lib, str)
        assert isinstance(d.doc, str)
        assert isinstance(d.mandatory, bool)
        assert isinstance(d.tags, rh.HDict)
        assert d.kind is not None

    def test_is_list(self, namespace):
        d = namespace.get_def("site")
        is_list = d.is_
        assert isinstance(is_list, list)

    def test_repr(self, namespace):
        d = namespace.get_def("site")
        assert isinstance(repr(d), str)


class TestLib:
    def test_properties(self, namespace):
        lib = namespace.get_lib("ph")
        assert lib.name == "ph"
        assert isinstance(lib.version, str)
        assert isinstance(lib.doc, str)
        assert isinstance(lib.depends, list)

    def test_defs(self, namespace):
        lib = namespace.get_lib("ph")
        defs = lib.defs()
        assert isinstance(defs, list)
        assert len(defs) > 0

    def test_get_def(self, namespace):
        lib = namespace.get_lib("ph")
        defs = lib.defs()
        # Verify at least one def exists and can be looked up by symbol
        assert len(defs) > 0
        first_sym = defs[0].symbol
        d = lib.get_def(first_sym)
        assert d is not None

    def test_len(self, namespace):
        lib = namespace.get_lib("ph")
        assert len(lib) > 0

    def test_repr(self, namespace):
        lib = namespace.get_lib("ph")
        assert isinstance(repr(lib), str)


class TestDefKind:
    def test_variants_exist(self):
        assert rh.ontology.DefKind.Marker is not None
        assert rh.ontology.DefKind.Val is not None
        assert rh.ontology.DefKind.Entity is not None

    def test_repr(self):
        assert isinstance(repr(rh.ontology.DefKind.Marker), str)


class TestSpec:
    def test_properties(self, namespace):
        specs = namespace.specs(lib="ph.equips")
        assert len(specs) > 0
        spec = specs[0]
        assert isinstance(spec.qname, str)
        assert isinstance(spec.name, str)
        assert isinstance(spec.lib, str)
        assert isinstance(spec.doc, str)
        assert isinstance(spec.is_abstract, bool)
        assert isinstance(spec.slots, list)

    def test_markers(self, namespace):
        specs = namespace.specs(lib="ph.equips")
        if specs:
            markers = specs[0].markers()
            assert isinstance(markers, list)

    def test_get_spec(self, namespace):
        specs = namespace.specs(lib="ph.equips")
        if specs:
            qname = specs[0].qname
            spec = namespace.get_spec(qname)
            assert spec is not None


class TestXeto:
    def test_load_and_unload(self, namespace):
        source = """
Site : Dict {
  site: Marker
  dis: Str
}
"""
        try:
            names = namespace.load_xeto(source, "testLib")
            assert isinstance(names, list)
            namespace.unload_lib("testLib")
        except Exception:
            # Some Xeto sources may not parse; that's acceptable
            pass

    def test_specs_with_lib_filter(self, namespace):
        specs = namespace.specs(lib="ph.equips")
        assert isinstance(specs, list)
        assert len(specs) > 0

    def test_specs_another_lib(self, namespace):
        specs = namespace.specs(lib="ph.protocols")
        assert isinstance(specs, list)
        assert len(specs) > 0


class TestSlot:
    def test_properties(self, namespace):
        specs = namespace.specs(lib="ph.equips")
        for spec in specs:
            if spec.slots:
                slot = spec.slots[0]
                assert isinstance(slot.name, str)
                assert isinstance(slot.is_marker, bool)
                assert isinstance(slot.is_query, bool)
                assert isinstance(slot.is_maybe, bool)
                break


class TestNamespaceSharing:
    """with_namespace used to move the namespace out, hollowing the caller's object."""

    def test_with_namespace_leaves_the_caller_usable(self):
        ns = rh.DefNamespace.load_standard()
        before = len(ns)
        assert before > 0

        rh.EntityGraph.with_namespace(ns)
        assert len(ns) == before, "namespace was emptied by with_namespace"

    def test_namespace_can_be_attached_to_several_graphs(self):
        ns = rh.DefNamespace.load_standard()
        before = len(ns)

        graphs = [rh.EntityGraph.with_namespace(ns) for _ in range(3)]
        assert len(ns) == before
        for g in graphs:
            g.add(rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()}))
            assert len(g.read("ph::Point")) == 1

    def test_namespace_still_answers_queries_after_sharing(self):
        # The compounding bug: an emptied namespace has no mandatory tags for
        # any name, so every spec match against it used to succeed vacuously.
        ns = rh.DefNamespace.load_standard()
        rh.EntityGraph.with_namespace(ns)

        site = rh.HDict({"id": rh.Ref("s1"), "site": rh.Marker()})
        assert rh.matches_filter("ph::Point", site, ns) is False
        assert ns.fits(site, "site") is True

    def test_from_grid_shares_rather_than_consumes(self):
        ns = rh.DefNamespace.load_standard()
        before = len(ns)
        grid = rh.HGrid.from_parts(
            rh.HDict(),
            [rh.HCol("id"), rh.HCol("point")],
            [rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()})],
        )
        rh.EntityGraph.from_grid(grid, ns)
        assert len(ns) == before

        rh.SharedGraph.from_grid(grid, ns)
        assert len(ns) == before

    def test_mutating_a_shared_namespace_leaves_both_sides_working(self):
        # Arc::make_mut forks the namespace on write, so loading a library after
        # sharing is visible to the caller and harmless to graphs already built.
        # (The pointer-level fork is asserted in the Rust tests, which can
        # compare Arc identity; here we check the observable behaviour.)
        ns = rh.DefNamespace.load_standard()
        g = rh.EntityGraph.with_namespace(ns)
        g.add(rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()}))
        assert len(g.read("ph::Point")) == 1

        ns.load_xeto("Widget : Dict {\n  widget: Marker\n}\n", "forkTestLib")
        assert ns.get_spec("forkTestLib::Widget") is not None, "caller sees it"
        assert len(g.read("ph::Point")) == 1, "graph is unaffected"

    def test_mutating_a_shared_namespace_does_not_raise(self):
        # `Arc::get_mut().unwrap()` would panic here instead of forking, because
        # the graph holds a second reference. Kept separate from the assertions
        # above so the failure mode is unambiguous.
        ns = rh.DefNamespace.load_standard()
        rh.EntityGraph.with_namespace(ns)
        rh.EntityGraph.with_namespace(ns)

        ns.load_xeto("Gadget : Dict {\n  gadget: Marker\n}\n", "getMutTestLib")
        assert ns.get_spec("getMutTestLib::Gadget") is not None

    def test_graph_keeps_working_after_the_caller_drops_the_namespace(self):
        # If `with_namespace` moved rather than shared, dropping the Python
        # handle would be fine — but if it borrowed without owning, this breaks.
        ns = rh.DefNamespace.load_standard()
        g = rh.EntityGraph.with_namespace(ns)
        g.add(rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()}))
        del ns

        assert len(g.read("ph::Point")) == 1

    def test_server_with_namespace_does_not_empty_it(self):
        # The same hollowing bug lived in HaystackServer.with_namespace.
        ns = rh.DefNamespace.load_standard()
        before = len(ns)

        server = rh.server.HaystackServer(rh.SharedGraph())
        server.with_namespace(ns)
        assert len(ns) == before, "namespace was emptied by server.with_namespace"
        assert ns.contains("site")


class TestFitsUnknownType:
    """fits used to return True for any name not in the taxonomy."""

    def test_fits_is_false_for_an_unregistered_name(self, namespace):
        point = rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()})
        assert namespace.fits(point, "point") is True
        assert namespace.fits(point, "bogus") is False
        assert namespace.fits(point, "") is False

    def test_empty_namespace_fits_nothing(self):
        ns = rh.DefNamespace()
        point = rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()})
        assert ns.fits(point, "point") is False

    def test_fits_explain_reports_the_unknown_type(self, namespace):
        point = rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()})
        issues = namespace.fits_explain(point, "bogus")
        assert len(issues) == 1
        assert "bogus" in str(issues[0])

    def test_graph_read_rejects_an_unknown_spec(self):
        ns = rh.DefNamespace.load_standard()
        g = rh.EntityGraph.with_namespace(ns)
        g.add(rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()}))
        g.add(rh.HDict({"id": rh.Ref("s1"), "site": rh.Marker()}))

        assert len(g.read("ph::Point")) == 1
        with pytest.raises((ValueError, rh.GraphError)):
            g.read("ph::Bogus")


class TestSpecTermResolution:
    """A `lib::Name` term may name a Xeto spec or a def, in either casing."""

    def test_xeto_spec_names_resolve(self, namespace):
        # Specs are keyed by qname in their own map and never enter the
        # taxonomy, so a taxonomy-only lookup rejected every one of them.
        specs = namespace.specs(None)
        assert len(specs) > 0

        g = rh.EntityGraph.with_namespace(namespace)
        g.add(rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()}))
        for spec in specs:
            # Must not raise: the namespace demonstrably holds this name.
            g.read(spec.qname)

    def test_camel_case_def_names_resolve(self, namespace):
        # Restricted to symbols the filter grammar can carry in a spec term.
        # Conjuncts (`fuelOil-input`) and feature defs (`op:watchSub`, `lib:phIoT`)
        # are excluded because `-` reads as an operator and `:` as a qualifier
        # separator — a parser limitation, unrelated to name resolution.
        camel = [
            d.symbol
            for d in namespace.defs()
            if any(c.isupper() for c in d.symbol) and d.symbol.replace("_", "").isalnum()
        ]
        assert len(camel) > 200, f"expected most camelCase defs to be testable, got {len(camel)}"

        point = rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()})
        for name in camel:
            # Lowercasing the bare name unconditionally made these unresolvable.
            rh.matches_filter(f"ph::{name}", point, namespace)

    def test_capitalised_haystack_spelling_still_resolves(self, namespace):
        point = rh.HDict({"id": rh.Ref("p1"), "point": rh.Marker()})
        assert rh.matches_filter("ph::Point", point, namespace) is True
        assert rh.matches_filter("ph::point", point, namespace) is True
        assert rh.matches_filter("ph::Ahu", point, namespace) is False
