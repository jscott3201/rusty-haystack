//! The actual all-boxed MCP wire contract. The catalog's existing schema export
//! describes unboxed Xeto instances and cannot stand in for this representation.
use serde_json::{Value, json};

const QNAME: &str = "^[A-Za-z0-9_.]+::[A-Za-z0-9_.]+$";
const NATIVE: &[&str] = &[
    "sys::Bool",
    "sys::Int",
    "sys::Float",
    "sys::Number",
    "sys::Str",
    "sys::Ref",
    "sys::Marker",
    "sys::None",
    "sys::NA",
    "sys::Buf",
    "sys::Uri",
    "sys::Date",
    "sys::Time",
    "sys::DateTime",
    "sys::Obj",
    "sys::Dict",
    "sys::List",
    "sys::Grid",
    "sys::Remove",
    "sys::Symbol",
    "sys::XStr",
    "sys::Coord",
];
fn structural_spec(base: &str) -> Value {
    json!({"type":"string","pattern":QNAME,"not":{"enum":
        NATIVE.iter().filter(|name| **name != base).collect::<Vec<_>>()}})
}
fn scalar(spec: &str, value: Value) -> Value {
    json!({"type":"object", "properties":{"spec":{"const":spec}, "val":value},
        "required":["spec","val"], "additionalProperties":false})
}
pub(crate) fn jeto(mut envelope: Value) -> Value {
    let mut scalars = vec![
        scalar("sys::Bool", json!({"enum":["true","false"]})),
        scalar(
            "sys::Int",
            json!({"type":"string","pattern":"^-?(0|[1-9][0-9]*)$"}),
        ),
        scalar("sys::Marker", json!({"const":"✓"})),
        scalar("sys::None", json!({"const":"∅"})),
        scalar("sys::NA", json!({"const":"NA"})),
        scalar(
            "sys::Buf",
            json!({"type":"string","pattern":"^[A-Za-z0-9_-]*$"}),
        ),
    ];
    for spec in [
        "sys::Float",
        "sys::Number",
        "sys::Str",
        "sys::Uri",
        "sys::Date",
        "sys::Time",
        "sys::DateTime",
    ] {
        scalars.push(scalar(spec, json!({"type":"string"})));
    }
    let mut reference = scalar("sys::Ref", json!({"type":"string","minLength":1}));
    reference["properties"]["dis"] = json!({"type":"string"});
    scalars.push(reference);
    // Nominal grammar/admission and provenance are semantic checks by the
    // immutable codec context, not permissions granted by JSON Schema. Exclude
    // all native classes so a malformed built-in box cannot enter this branch.
    scalars.push(json!({"type":"object", "properties":{
        "spec":{"type":"string","pattern":QNAME,"not":{"enum":NATIVE}},
        "val":{"type":"string"}},"required":["spec","val"],"additionalProperties":false}));
    let dict = json!({"type":"object","properties":{"spec":structural_spec("sys::Dict")},
        "additionalProperties":{"$ref":"#/$defs/member"}});
    let grid = json!({"type":"object", "properties":{
        "spec":structural_spec("sys::Grid"), "of":{"type":"string","pattern":QNAME},
        "meta":{"$ref":"#/$defs/dict"},
        "cols":{"type":"array","items":{"type":"object","properties":{
            "name":{"type":"string","minLength":1},"of":{"type":"string","pattern":QNAME},
            "meta":{"$ref":"#/$defs/dict"}},"required":["name"],"additionalProperties":false}},
        "rows":{"type":"array","items":{"$ref":"#/$defs/dict"}}},
        "required":["spec","cols","rows"],"additionalProperties":false});
    envelope["$defs"] = json!({
        "scalar":{"oneOf":scalars}, "dict":dict, "grid":grid,
        "member":{"anyOf":[{"$ref":"#/$defs/scalar"},{"$ref":"#/$defs/dict"},
            {"$ref":"#/$defs/grid"},{"type":"array","items":{"$ref":"#/$defs/value"}}]},
        "value":{"anyOf":[{"type":"null"},{"$ref":"#/$defs/member"}]}
    });
    envelope["oneOf"][0]["properties"]["grid"] = json!({"$ref":"#/$defs/grid"});
    envelope["oneOf"][0]["properties"]["catalog"] = json!({"type":"object","properties":{
        "repository":{"type":"string","minLength":1},"revision":{"type":"string","minLength":1},
        "selection":{"type":"string","pattern":"^[0-9a-f]{64}$"}},
        "required":["repository","revision","selection"],"additionalProperties":false});
    envelope["oneOf"][0]["required"]
        .as_array_mut()
        .expect("fixed schema")
        .push(json!("catalog"));
    envelope
}
