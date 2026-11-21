"""Cross-language value identity and atomic Python conversion regressions."""

import datetime as dt
import json
from pathlib import Path
import struct

import pytest
import rusty_haystack as rh


def payload(value):
    return json.dumps({"version": 1, "value": value})


@pytest.mark.parametrize("value", [2**53 + 1, -(2**53 + 1), 2**63 - 1])
def test_native_int_requires_exact_binary64(value):
    for convert in (rh.Number, lambda x: rh.Coord(x, 0), lambda x: rh.HList([x])):
        with pytest.raises(ValueError, match="exact"):
            convert(value)


@pytest.mark.parametrize("value", [2**63, -(2**63) - 1, 10**100])
def test_native_int_requires_i64(value):
    for convert in (rh.Number, lambda x: rh.Coord(0, x), lambda x: rh.HDict({"v": x})):
        with pytest.raises(OverflowError):
            convert(value)


def test_numeric_hooks_are_never_called():
    class Numeric:
        def __float__(self):
            pytest.fail("arbitrary __float__ hook called")

        def __index__(self):
            pytest.fail("arbitrary __index__ hook called")

    for convert in (rh.Number, lambda x: rh.Coord(x, 0), lambda x: rh.HList([x])):
        with pytest.raises(TypeError):
            convert(Numeric())


def test_number_integer_upper_bound_and_native_mapping():
    with pytest.raises(OverflowError):
        int(rh.Number(float(2**63)))
    assert int(rh.Number(float(-(2**63)))) == -(2**63)
    assert list(rh.HList([True, 2**53 + 2, -2**63])) == [True, rh.Number(2**53 + 2), rh.Number(-2**63)]


def test_nested_errors_preserve_original_exception_and_destination():
    d = rh.HDict({"v": "original"})
    values = rh.HList(["original"])
    for value, error in [("\ud800", UnicodeEncodeError), (2**53 + 1, ValueError), (object(), TypeError)]:
        with pytest.raises(error):
            d.set("v", value)
        with pytest.raises(error):
            values[0] = value
        assert d["v"] == values[0] == "original"
        with pytest.raises(error):
            rh.HDict({"ok": rh.HList([1]), "bad": value})
    with pytest.raises(UnicodeEncodeError):
        rh.HDict({"\ud800": 1})
    with pytest.raises(RuntimeError, match="borrow"):
        d.set("self", d)
    assert d.to_dict() == {"v": "original"}


@pytest.mark.parametrize("time", [dt.time(1, tzinfo=dt.timezone.utc), dt.time(1, fold=1)])
def test_time_input_rejects_zone_and_fold(time):
    with pytest.raises(ValueError):
        rh.HList([time])


def test_rich_values_preserve_variant_and_exact_bits():
    values = [rh.Int(2**63 - 1), rh.Int(-2**63), rh.NoneValue(), rh.Buf(b"\x00\xff"), rh.Nominal("example::Serial", "A", "r1", "00042")]
    for bits in [0, 1 << 63, 0x7FF0000000000000, 0x7FF0000000000001, 0x7FF8000000000001, 0xFFF8000000000001, 1]:
        value = rh.Float.from_bits(bits)
        assert value.bits == bits
        values.append(value)
    for value in values:
        decoded = rh.decode_typed_value(rh.encode_typed_value(value))
        assert type(decoded) is type(value)
        assert decoded == value and hash(decoded) == hash(value)
        assert rh.HDict({"v": rh.HList([value])})["v"][0] == value
    assert rh.Int(42) != rh.Float(42.0)
    assert rh.Int(42) != rh.Number(42)
    assert rh.NoneValue() is not None
    assert rh.Float.from_bits(0) != rh.Float.from_bits(1 << 63)


def test_identity_ordering_and_readonly_fields():
    nan = struct.unpack("!d", bytes.fromhex("7ff8000000000001"))[0]
    for make in (lambda x: rh.Number(x, "kW"), lambda x: rh.Coord(x, 1.0)):
        assert make(nan) == make(nan)
        assert hash(make(nan)) == hash(make(nan))
        assert make(0.0) != make(-0.0)
    assert not (rh.Number(nan) < rh.Number(nan))
    assert not (rh.Number(1, "kW") < rh.Number(2, "W"))
    assert rh.Number(-0.0) <= rh.Number(0.0)
    for value, field in [(rh.Number(1), "val"), (rh.Number(1), "unit"), (rh.Coord(0, 0), "lat"), (rh.Int(1), "value"), (rh.Float(1.0), "bits"), (rh.Buf(b"x"), "data"), (rh.Nominal("a::B", "a", "r", "x"), "value")]:
        with pytest.raises(AttributeError):
            setattr(value, field, 2)


def test_shared_independent_typed_fixture_and_strict_h4_codecs():
    source = Path(__file__).resolve().parents[2] / "haystack-core/tests/fixtures/typed-v1-entity.json"
    expected = json.loads(source.read_text())
    value = rh.decode_typed_value(json.dumps(expected))
    assert json.loads(rh.encode_typed_value(value)) == expected
    for codec in ("text/zinc", "application/json", "text/trio", "text/csv"):
        with pytest.raises(rh.CodecError):
            rh.encode_scalar(codec, rh.HList([rh.Int(1)]))
        grid = rh.HGrid.from_parts(rh.HDict(), [], [value])
        with pytest.raises(rh.CodecError):
            rh.encode_grid(codec, grid)


@pytest.mark.parametrize("wire", [
    {"kind": "time", "seconds": 1, "nanos": 1},
    {"kind": "time", "seconds": 59, "nanos": 1_000_000_000},
    {"kind": "date", "value": "0000-01-01"},
])
def test_temporal_projection_rejects_loss_recursively(wire):
    with pytest.raises(ValueError):
        rh.decode_typed_value(payload(wire))
    with pytest.raises(ValueError):
        rh.decode_typed_value(payload({"kind": "dict", "tags": {"v": {"kind": "list", "items": [wire]}}}))


def test_datetime_preserves_nanos_and_opaque_zone_label():
    wire = {"kind": "dateTime", "seconds": "0", "nanos": 1, "offset": 3600, "timezone": "opaque-label"}
    exact = rh.decode_typed_value(payload(wire))
    assert exact.tz_name == "opaque-label"
    assert json.loads(rh.encode_typed_value(exact))["value"] == wire
    with pytest.raises(ValueError, match="microsecond"):
        exact.dt()
    fixed = rh.HDateTime(2024, 3, 10, 2, 30, 0, -18000, "New_York")
    assert fixed.dt().utcoffset() == dt.timedelta(hours=-5)
    assert fixed.dt().hour == 2
    with pytest.raises(TypeError, match="HDateTime"):
        rh.HList([dt.datetime.now(dt.timezone.utc)])


def test_typed_payload_bounds_and_validation():
    for data in ('{"version":2,"value":{"kind":"null"}}', '{"version":1,"value":{"kind":"int","value":"9223372036854775808"}}', " " * 1_048_577):
        with pytest.raises(rh.CodecError):
            rh.decode_typed_value(data)


def test_recursive_depth_limit_is_atomic():
    value = None
    for _ in range(64):
        value = rh.HList([value])
    assert isinstance(value, rh.HList)
    destination = rh.HDict({"v": "unchanged"})
    with pytest.raises(ValueError, match="nesting"):
        destination["v"] = value
    assert destination["v"] == "unchanged"
    with pytest.raises(ValueError, match="nesting"):
        rh.HList([value])
    with pytest.raises(rh.CodecError):
        rh.encode_typed_value(value)  # The typed JSON envelope has its own depth bound.


@pytest.mark.parametrize("graph_type", [rh.EntityGraph, rh.SharedGraph])
def test_rich_graph_reads_changes_and_removal(graph_type):
    graph = graph_type()
    entity = rh.HDict({"id": rh.Ref("typed"), "v": rh.HList([rh.Int(2**63 - 1)])})
    graph.add(entity)
    assert graph.get("typed") == entity
    assert graph.read("v")[0] == entity
    assert graph.changes_since(0)[0].new == entity
    assert graph.remove("typed") == entity
    assert graph.get("typed") is None


def test_date_subclasses_use_stored_fields_without_callbacks():
    calls = []

    class Index:
        def __index__(self):
            calls.append("index")
            return 2000

    class BrokenDate(dt.date):
        @property
        def year(self):
            calls.append("year")
            raise LookupError("date property failed")

    class AlteredDate(dt.date):
        @property
        def year(self):
            calls.append("year")
            return Index()

        month = property(lambda self: 12)
        day = property(lambda self: 31)

    for date in (BrokenDate(2024, 2, 3), AlteredDate(2024, 2, 3)):
        assert rh.HDict({"nested": date})["nested"] == dt.date(2024, 2, 3)
    assert calls == []


@pytest.mark.parametrize("kwargs", [{"tzinfo": dt.timezone(dt.timedelta(hours=3))}, {"fold": 1}])
def test_time_subclasses_cannot_hide_stored_zone_or_fold(kwargs):
    calls = []

    class HiddenTime(dt.time):
        @property
        def tzinfo(self):
            calls.append("tzinfo")
            return None

        @property
        def fold(self):
            calls.append("fold")
            return 0

    destination = rh.HDict({"value": "unchanged"})
    with pytest.raises(ValueError, match="tzinfo=None and fold=0"):
        destination["value"] = HiddenTime(12, **kwargs)
    assert destination["value"] == "unchanged"
    assert calls == []


def test_time_subclasses_use_stored_clock_fields_without_callbacks():
    calls = []

    def override(self):
        calls.append("clock")
        return 0

    class AlteredTime(dt.time):
        hour = property(override)
        minute = property(override)
        second = property(override)
        microsecond = property(override)

    time = AlteredTime(1, 2, 3, 456789)
    assert rh.HList([time])[0] == dt.time(1, 2, 3, 456789)
    assert calls == []


@pytest.mark.parametrize("component", range(7))
def test_datetime_components_reject_numeric_hooks(component):
    calls = []
    arguments = [2024, 1, 2, 3, 4, 5, 3600]

    class Index:
        def __index__(self):
            calls.append(component)
            return [2024, 1, 2, 3, 4, 5, 3600][component]

    arguments[component] = Index()
    with pytest.raises(TypeError, match="int"):
        rh.HDateTime(*arguments, "opaque")
    assert calls == []


def test_datetime_concrete_integer_components_preserve_admission_and_errors():
    class NativeInt(int):
        def __index__(self):
            pytest.fail("int subclass index hook called")

    assert rh.HDateTime(*(NativeInt(n) for n in [2024, 1, 2, 3, 4, 5, 3600]), "opaque").dt() == dt.datetime(2024, 1, 2, 3, 4, 5, tzinfo=dt.timezone(dt.timedelta(hours=1)))
    assert rh.HDateTime(True, True, True, False, False, False, False, "opaque").dt() == dt.datetime(1, 1, 1, tzinfo=dt.timezone.utc)
    for component, value, error in [(0, 2**31, OverflowError), (1, -1, OverflowError), (1, 2**32, OverflowError), (6, 2**31, OverflowError), (1, 13, ValueError), (6, 86400, ValueError)]:
        arguments = [2024, 1, 2, 3, 4, 5, 0]
        arguments[component] = value
        with pytest.raises(error):
            rh.HDateTime(*arguments, "opaque")


@pytest.mark.parametrize("seconds,offset", [(8210266876799, 1), (-8334601228800, -1)])
def test_datetime_projection_rejects_local_calendar_overflow(seconds, offset):
    wire = {"kind": "dateTime", "seconds": str(seconds), "nanos": 0, "offset": offset, "timezone": "edge"}
    value = rh.decode_typed_value(payload(wire))
    assert json.loads(rh.encode_typed_value(value))["value"] == wire
    with pytest.raises(ValueError, match="range"):
        value.dt()


def test_builtin_numeric_subclasses_do_not_call_hooks():
    class NativeInt(int):
        def __index__(self):
            pytest.fail("int subclass index hook called")

        def __float__(self):
            pytest.fail("int subclass float hook called")

    class NativeFloat(float):
        def __float__(self):
            pytest.fail("float subclass float hook called")

    assert rh.Number(NativeInt(42)) == rh.Number(42)
    assert rh.Number(NativeFloat(1.5)) == rh.Number(1.5)


@pytest.mark.parametrize("wire", [
    {"kind": "dateTime", "seconds": "-62167219200", "nanos": 0, "offset": 0, "timezone": "UTC"},
    {"kind": "dateTime", "seconds": "59", "nanos": 1_000_000_000, "offset": 0, "timezone": "UTC"},
])
def test_datetime_native_projection_rejects_year_and_leap_loss(wire):
    value = rh.decode_typed_value(payload(wire))
    assert json.loads(rh.encode_typed_value(value))["value"] == wire
    with pytest.raises(ValueError):
        value.dt()
