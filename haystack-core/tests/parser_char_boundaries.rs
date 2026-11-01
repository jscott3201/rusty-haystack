//! The hand-written parsers track a byte offset into a `&str`. Any advance by a
//! fixed byte count that is not first checked against ASCII can leave that
//! offset inside a multi-byte char, and the next `src[pos..]` slice panics.
//! The server decodes untrusted request bodies through the Zinc parser, and
//! Trio, filters and the client's history decoder all reach it too, so such a
//! panic is remotely reachable. These tests pin the fix: malformed non-ASCII
//! input is a parse error, never a panic.
//!
//! The mutation sweep at the bottom is deterministic and dependency-free on
//! purpose; the workspace has no property-testing or fuzzing dependency.

use std::panic::{AssertUnwindSafe, catch_unwind};

use haystack_core::codecs::{trio, zinc};
use haystack_core::filter::parse_filter;
use haystack_core::xeto::parse_xeto;

/// Run `f`, turning a panic into a test failure that names the input.
fn no_panic<T>(what: &str, input: &str, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => panic!("{what} panicked on input {input:?}"),
    }
}

fn zinc_scalar_err(input: &str) {
    let result = no_panic("zinc::decode_scalar", input, || zinc::decode_scalar(input));
    assert!(
        result.is_err(),
        "expected Err for {input:?}, got {result:?}"
    );
}

fn zinc_grid_err(input: &str) {
    let result = no_panic("zinc::decode_grid", input, || zinc::decode_grid(input));
    assert!(
        result.is_err(),
        "expected Err for {input:?}, got {result:?}"
    );
    let result = no_panic("zinc::decode_grid_complete_rows", input, || {
        zinc::decode_grid_complete_rows(input)
    });
    assert!(
        result.is_err(),
        "expected Err for {input:?}, got {result:?}"
    );
}

// ── Reported reproductions ──

#[test]
fn offset_hour_split_by_multibyte_char_in_a_row_is_an_error() {
    zinc_grid_err("ver:\"3.0\"\nts\n2024-06-01T00:00:00+0é:00 UTC\n");
}

#[test]
fn offset_hour_split_by_multibyte_char_in_grid_meta_is_an_error() {
    zinc_grid_err("ver:\"3.0\" hisStart:2024-06-01T00:00:00+0é:00 UTC\nts\n");
}

#[test]
fn offset_hour_split_by_multibyte_char_in_column_meta_is_an_error() {
    zinc_grid_err("ver:\"3.0\"\nts start:2024-06-01T00:00:00+0é:00\n");
}

// ── Every fixed-width advance in the Zinc parser ──

#[test]
fn utc_offset_sign_hours_and_minutes_reject_non_ascii() {
    for offset in [
        "+é:00",
        "+é0:00",
        "+0é:00",
        "-0é:00",
        "+00:é",
        "+00:é0",
        "+00:0é",
        "-00:0€",
        "+0😀:00",
        "+00:0😀",
        "+€",
    ] {
        zinc_scalar_err(&format!("2024-06-01T00:00:00{offset}"));
        zinc_scalar_err(&format!("2024-06-01T00:00:00{offset} UTC"));
    }
}

#[test]
fn datetime_time_components_reject_non_ascii() {
    for time in [
        "é0:00:00Z",
        "0é:00:00Z",
        "00é00:00Z",
        "00:é0:00Z",
        "00:0é:00Z",
        "00:00:é0Z",
        "00:00:0éZ",
        "00:00:0€Z",
        "00:00:0😀Z",
        "0€",
    ] {
        zinc_scalar_err(&format!("2024-06-01T{time}"));
    }
}

#[test]
fn time_seconds_reject_non_ascii() {
    for time in ["12:00:é", "12:00:0é", "12:00:0€", "12:00:0😀", "12:00:😀"] {
        zinc_scalar_err(time);
    }
}

#[test]
fn date_components_reject_non_ascii() {
    for date in ["2024-0é-01", "2024-06-0é", "202é-06-01", "2024-06-01Té"] {
        zinc_scalar_err(date);
    }
}

#[test]
fn unicode_escape_straddling_a_multibyte_char_is_an_error() {
    for s in [
        r#""\u00é""#,
        r#""\u0é""#,
        r#""\u0€""#,
        r#""\u€""#,
        r#""\u😀""#,
        r#""\u0😀""#,
    ] {
        zinc_scalar_err(s);
    }
}

#[test]
fn coord_with_non_ascii_components_is_an_error() {
    for c in ["C(é,1)", "C(1,é)", "C(1é,2)", "C(1,2€)", "C(😀", "C(1,😀"] {
        zinc_scalar_err(c);
    }
}

#[test]
fn xstr_without_an_opening_quote_is_an_error() {
    // `read_str` skips its opening quote unchecked; the XStr path used to call
    // it without looking, so it skipped half a char here, or an ASCII byte.
    for x in ["Foo(é\")", "Foo(€\")", "Foo(x\")", "Foo(1)"] {
        zinc_scalar_err(x);
    }
    // The well-formed spelling still decodes.
    assert!(zinc::decode_scalar("Foo(\"bar\")").is_ok());
}

#[test]
fn numbers_with_non_ascii_exponents_are_errors() {
    for n in ["1eé", "1e+é", "1E-€", "1.é", "-1e😀"] {
        zinc_scalar_err(n);
    }
}

#[test]
fn non_ascii_after_every_sigil_never_panics() {
    // Some of these are valid (non-ASCII is legal in refs, units and tz names);
    // the point is that none of them panic.
    for s in [
        "@é",
        "@a€",
        "^é",
        "`é`",
        "`\\é`",
        "\"\\é\"",
        "1é",
        "1%é",
        "-INFé",
        "INFé",
        "NaN€",
        "2024-06-01T00:00:00Z é",
        "2024-06-01T00:00:00Z UTCé",
        "[é]",
        "{é}",
        "{a:é}",
        "Né",
        "Té",
        "Mé",
        "C(",
        "C(1",
        "\"\\u",
        "12:0é",
    ] {
        let _ = no_panic("zinc::decode_scalar", s, || zinc::decode_scalar(s));
        let grid = format!("ver:\"3.0\"\nv\n{s}\n");
        let _ = no_panic("zinc::decode_grid", &grid, || zinc::decode_grid(&grid));
    }
}

#[test]
fn existing_accepted_datetimes_still_decode() {
    for s in [
        "2024-06-01T00:00:00Z",
        "2024-06-01T00:00:00Z UTC",
        "2024-06-01T00:00:00+05:30 Kolkata",
        "2024-06-01T00:00:00-04:00 New_York",
        "2024-06-01T00:00:00.123-04:00 New_York",
        "2024-06-30T12:00:00+00:00 GMT+5",
        "12:30",
        "12:30:45",
        "12:30:45.5",
        "\"\\u00e9\"",
        "C(37.5,-77.4)",
    ] {
        assert!(zinc::decode_scalar(s).is_ok(), "{s:?} no longer decodes");
    }
}

// ── Parsers that reach the Zinc scalar parser ──

#[test]
fn trio_value_with_split_offset_never_panics() {
    for input in [
        "id: @a\nts: 2024-06-01T00:00:00+0é:00 UTC\n",
        "ts: 2024-06-01T00:00:00+00:0é\n",
        "s: \"\\u00é\"\n",
        "c: C(é,1)\n",
    ] {
        let _ = no_panic("trio::decode_grid", input, || trio::decode_grid(input));
    }
}

#[test]
fn filter_value_with_split_offset_is_an_error() {
    for input in [
        "ts == 2024-06-01T00:00:00+0é:00",
        "ts >= 2024-06-01T00:00:00+00:0é UTC",
        "t == 12:00:0é",
        "s == \"\\u00é\"",
        "c == C(é,1)",
    ] {
        let result = no_panic("parse_filter", input, || parse_filter(input));
        assert!(
            result.is_err(),
            "expected Err for {input:?}, got {result:?}"
        );
    }
}

// ── Deterministic mutation sweep ──

const MULTIBYTE: [char; 4] = ['é', '€', '😀', '\u{0301}'];
const ASCII: [char; 6] = [':', '+', '-', '"', '0', '\\'];

/// Every variant of `seed` with one char inserted at, or substituted for the
/// char at, each char boundary.
fn mutations(seed: &str) -> Vec<String> {
    let boundaries: Vec<usize> = seed
        .char_indices()
        .map(|(i, _)| i)
        .chain([seed.len()])
        .collect();
    let mut out = Vec::new();
    for &at in &boundaries {
        let next = seed[at..].chars().next().map_or(at, |c| at + c.len_utf8());
        for ch in MULTIBYTE.iter().chain(ASCII.iter()) {
            let mut inserted = String::with_capacity(seed.len() + 4);
            inserted.push_str(&seed[..at]);
            inserted.push(*ch);
            inserted.push_str(&seed[at..]);
            out.push(inserted);
            if next > at {
                let mut replaced = String::with_capacity(seed.len() + 4);
                replaced.push_str(&seed[..at]);
                replaced.push(*ch);
                replaced.push_str(&seed[next..]);
                out.push(replaced);
            }
        }
    }
    out
}

const ZINC_SEEDS: &[&str] = &[
    "ver:\"3.0\" hisStart:2024-06-01T00:00:00-04:00 New_York hisEnd:2024-06-02T00:00:00Z UTC\n\
     ts,val\n\
     2024-06-01T00:00:00.5+05:30 Kolkata,72.5°F\n\
     2024-06-01T01:00:00-04:00 New_York,-1.5e-3kW\n",
    "ver:\"3.0\"\n\
     id,dis,site,area,geo,uri,sym\n\
     @site-1 \"Main\",\"Caf\\u00e9 \\\"x\\\"\",M,1_000ft²,C(37.55,-77.45),`http://a/b`,^ahu\n",
    "ver:\"3.0\"\n\
     d,t,l,x,n\n\
     2024-06-01,12:30:45.123,[1,\"a\",{b:T c:F}],Bin(\"text/plain\"),-INF\n",
];

const TRIO_SEEDS: &[&str] = &[
    "id: @a\ndis: \"Ahu é\"\nts: 2024-06-01T00:00:00-04:00 New_York\nt: 12:30:45\n\
     c: C(1.5,2.5)\n---\nid: @b\nnote:\n  multi\n  line\n",
];

const FILTER_SEEDS: &[&str] = &[
    "site and area > 1_000ft² or not equip",
    "ts >= 2024-06-01T00:00:00-04:00 New_York and t < 12:30:45",
    "siteRef->dis == \"Caf\\u00e9\" and ph::ahu-meter and c == C(1,2)",
];

const XETO_SEEDS: &[&str] =
    &["// doc\nAhu : Equip <abstract> {\n  ahu\n  dis: Str \"é\"\n  area: Number 10ft²\n}\n"];

#[test]
fn mutated_inputs_never_panic() {
    let mut cases = 0usize;
    for seed in ZINC_SEEDS {
        assert!(
            zinc::decode_grid(seed).is_ok(),
            "seed must decode: {seed:?}"
        );
        for input in mutations(seed) {
            let _ = no_panic("zinc::decode_grid", &input, || zinc::decode_grid(&input));
            let _ = no_panic("zinc::decode_grid_complete_rows", &input, || {
                zinc::decode_grid_complete_rows(&input)
            });
            cases += 1;
        }
        // Each cell on its own, as a scalar.
        for cell in seed.lines().skip(2).flat_map(|l| l.split(',')) {
            for input in mutations(cell) {
                let _ = no_panic("zinc::decode_scalar", &input, || {
                    zinc::decode_scalar(&input)
                });
                cases += 1;
            }
        }
    }
    for seed in TRIO_SEEDS {
        assert!(
            trio::decode_grid(seed).is_ok(),
            "seed must decode: {seed:?}"
        );
        for input in mutations(seed) {
            let _ = no_panic("trio::decode_grid", &input, || trio::decode_grid(&input));
            cases += 1;
        }
    }
    for seed in FILTER_SEEDS {
        assert!(parse_filter(seed).is_ok(), "seed must parse: {seed:?}");
        for input in mutations(seed) {
            let _ = no_panic("parse_filter", &input, || parse_filter(&input));
            cases += 1;
        }
    }
    for seed in XETO_SEEDS {
        for input in mutations(seed) {
            let _ = no_panic("parse_xeto", &input, || parse_xeto(&input));
            cases += 1;
        }
    }
    assert!(cases > 10_000, "sweep shrank unexpectedly: {cases} cases");
}
