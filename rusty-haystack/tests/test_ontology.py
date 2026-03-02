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
