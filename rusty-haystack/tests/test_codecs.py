"""Tests for codec encode/decode: Zinc, Trio, JSON, CSV."""

import pytest
import rusty_haystack as rh


CODECS = ["text/zinc", "text/trio", "application/json", "application/json;v=3"]


class TestGridRoundtrip:
    """Encode then decode a grid and verify data survives the roundtrip."""

    def _make_grid(self):
        meta = rh.HDict({"ver": "3.0"})
        cols = [rh.HCol("id"), rh.HCol("dis"), rh.HCol("site")]
        rows = [
            rh.HDict({
                "id": rh.Ref("site-1"),
                "dis": "Demo Site",
                "site": rh.Marker(),
            }),
            rh.HDict({
                "id": rh.Ref("site-2"),
                "dis": "Second Site",
                "site": rh.Marker(),
            }),
        ]
        return rh.HGrid.from_parts(meta, cols, rows)

    @pytest.mark.parametrize("codec", CODECS)
    def test_roundtrip(self, codec):
        grid = self._make_grid()
        encoded = rh.encode_grid(codec, grid)
        assert isinstance(encoded, str)
        assert len(encoded) > 0

        decoded = rh.decode_grid(codec, encoded)
        assert len(decoded) == 2
        assert decoded[0]["dis"] == "Demo Site"
        assert decoded[1]["dis"] == "Second Site"

    @pytest.mark.parametrize("codec", CODECS)
    def test_empty_grid_roundtrip(self, codec):
        grid = rh.HGrid()
        encoded = rh.encode_grid(codec, grid)
        decoded = rh.decode_grid(codec, encoded)
        assert decoded.is_empty()

    def test_csv_encode(self):
        grid = self._make_grid()
        csv_str = rh.encode_grid("text/csv", grid)
        assert "Demo Site" in csv_str
        assert "," in csv_str


class TestScalarRoundtrip:
    def test_number_zinc(self):
        n = rh.Number(72.5, "°F")
        encoded = rh.encode_scalar("text/zinc", n)
        assert "72.5" in encoded

    def test_string_zinc(self):
        encoded = rh.encode_scalar("text/zinc", "hello world")
        decoded = rh.decode_scalar("text/zinc", encoded)
        assert decoded == "hello world"

    def test_ref_zinc(self):
        r = rh.Ref("site-1", "My Site")
        encoded = rh.encode_scalar("text/zinc", r)
        assert "site-1" in encoded

    def test_marker_zinc(self):
        encoded = rh.encode_scalar("text/zinc", rh.Marker())
        assert isinstance(encoded, str)

    def test_coord_zinc(self):
        c = rh.Coord(40.7128, -74.006)
        encoded = rh.encode_scalar("text/zinc", c)
        assert "40.7128" in encoded


class TestCodecErrors:
    def test_unknown_codec_encode(self):
        with pytest.raises(rh.CodecError):
            rh.encode_grid("text/unknown", rh.HGrid())

    def test_unknown_codec_decode(self):
        with pytest.raises(rh.CodecError):
            rh.decode_grid("text/unknown", "data")

    def test_invalid_zinc_decode(self):
        with pytest.raises((ValueError, Exception)):
            rh.decode_grid("text/zinc", "not valid zinc at all!!!")

    def test_invalid_json_decode(self):
        with pytest.raises((ValueError, Exception)):
            rh.decode_grid("application/json", "{invalid json")


class TestZincSpecifics:
    def test_encode_number_with_unit(self):
        g = rh.HGrid.from_parts(
            rh.HDict(),
            [rh.HCol("val")],
            [rh.HDict({"val": rh.Number(72.5, "°F")})],
        )
        zinc = rh.encode_grid("text/zinc", g)
        assert "72.5" in zinc

    def test_encode_datetime(self):
        dt = rh.HDateTime(2024, 6, 15, 12, 0, 0, 0, "UTC")
        g = rh.HGrid.from_parts(
            rh.HDict(),
            [rh.HCol("ts")],
            [rh.HDict({"ts": dt})],
        )
        zinc = rh.encode_grid("text/zinc", g)
        assert "2024" in zinc

    def test_encode_with_all_types(self):
        row = rh.HDict({
            "id": rh.Ref("p-1"),
            "marker": rh.Marker(),
            "num": rh.Number(42),
            "str_val": "hello",
            "uri": rh.Uri("http://example.com"),
            "coord": rh.Coord(1.0, 2.0),
        })
        cols = [rh.HCol(k) for k in ["id", "marker", "num", "str_val", "uri", "coord"]]
        g = rh.HGrid.from_parts(rh.HDict(), cols, [row])
        zinc = rh.encode_grid("text/zinc", g)
        assert isinstance(zinc, str)
        assert len(zinc) > 0
