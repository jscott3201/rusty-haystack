"""Shared fixtures for rusty_haystack tests."""

import pytest
import rusty_haystack as rh


@pytest.fixture
def site_dict():
    """A typical site entity dict."""
    return rh.HDict({
        "id": rh.Ref("site-1", "Demo Site"),
        "site": rh.Marker(),
        "dis": "Demo Site",
        "area": rh.Number(5000),
        "geoCoord": rh.Coord(40.7128, -74.0060),
    })


@pytest.fixture
def equip_dict():
    """A typical equip entity dict."""
    return rh.HDict({
        "id": rh.Ref("ahu-1"),
        "equip": rh.Marker(),
        "ahu": rh.Marker(),
        "dis": "AHU-1",
        "siteRef": rh.Ref("site-1"),
    })


@pytest.fixture
def point_dict():
    """A typical point entity dict."""
    return rh.HDict({
        "id": rh.Ref("temp-1"),
        "point": rh.Marker(),
        "temp": rh.Marker(),
        "sensor": rh.Marker(),
        "dis": "Zone Temp",
        "kind": "Number",
        "unit": "°F",
        "equipRef": rh.Ref("ahu-1"),
        "siteRef": rh.Ref("site-1"),
    })


@pytest.fixture
def sample_graph(site_dict, equip_dict, point_dict):
    """EntityGraph with site -> equip -> point."""
    g = rh.EntityGraph()
    g.add(site_dict)
    g.add(equip_dict)
    g.add(point_dict)
    return g


@pytest.fixture
def namespace():
    """Standard Haystack ontology namespace."""
    return rh.DefNamespace.load_standard()
