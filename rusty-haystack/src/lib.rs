mod auth;
mod client;
mod codecs;
mod convert;
mod data;
pub mod exceptions;
mod filter;
mod graph;
mod kinds;
mod ontology;
mod server;
mod units;

use pyo3::prelude::*;

/// Register types into a submodule, creating it if needed.
fn add_submodule(
    parent: &Bound<'_, PyModule>,
    name: &str,
    register: impl FnOnce(&Bound<'_, PyModule>) -> PyResult<()>,
) -> PyResult<()> {
    let py = parent.py();
    let sub = PyModule::new(py, name)?;
    register(&sub)?;
    parent.add_submodule(&sub)?;
    // Make importable as `from rusty_haystack.sub import Foo`
    py.import("sys")?
        .getattr("modules")?
        .set_item(format!("rusty_haystack.{}", name), &sub)?;
    Ok(())
}

#[pymodule]
fn rusty_haystack(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.1.0")?;

    // ── Top-level convenience re-exports ──
    // All types also available at top level for backward compatibility
    m.add_class::<kinds::PyMarker>()?;
    m.add_class::<kinds::PyNA>()?;
    m.add_class::<kinds::PyRemove>()?;
    m.add_class::<kinds::PyNumber>()?;
    m.add_class::<kinds::PyRef_>()?;
    m.add_class::<kinds::PyUri>()?;
    m.add_class::<kinds::PySymbol>()?;
    m.add_class::<kinds::PyXStr>()?;
    m.add_class::<kinds::PyCoord>()?;
    m.add_class::<kinds::PyHDateTime>()?;
    m.add_class::<data::PyHDict>()?;
    m.add_class::<data::PyHGrid>()?;
    m.add_class::<data::PyHList>()?;
    m.add_class::<data::PyHCol>()?;
    m.add_class::<graph::PyEntityGraph>()?;
    m.add_class::<graph::PySharedGraph>()?;
    m.add_class::<filter::PyFilter>()?;
    m.add_class::<ontology::PyDefNamespace>()?;

    // Top-level codec/filter functions
    m.add_function(wrap_pyfunction!(codecs::encode_grid, m)?)?;
    m.add_function(wrap_pyfunction!(codecs::decode_grid, m)?)?;
    m.add_function(wrap_pyfunction!(codecs::encode_scalar, m)?)?;
    m.add_function(wrap_pyfunction!(codecs::decode_scalar, m)?)?;
    m.add_function(wrap_pyfunction!(filter::parse_filter, m)?)?;
    m.add_function(wrap_pyfunction!(filter::matches_filter, m)?)?;

    // Top-level unit functions
    m.add_function(wrap_pyfunction!(units::convert, m)?)?;
    m.add_function(wrap_pyfunction!(units::compatible, m)?)?;
    m.add_function(wrap_pyfunction!(units::quantity, m)?)?;
    m.add_function(wrap_pyfunction!(units::base_unit, m)?)?;

    // ── Submodule: kinds ──
    add_submodule(m, "kinds", |sub| {
        sub.add_class::<kinds::PyMarker>()?;
        sub.add_class::<kinds::PyNA>()?;
        sub.add_class::<kinds::PyRemove>()?;
        sub.add_class::<kinds::PyNumber>()?;
        sub.add_class::<kinds::PyRef_>()?;
        sub.add_class::<kinds::PyUri>()?;
        sub.add_class::<kinds::PySymbol>()?;
        sub.add_class::<kinds::PyXStr>()?;
        sub.add_class::<kinds::PyCoord>()?;
        sub.add_class::<kinds::PyHDateTime>()?;
        Ok(())
    })?;

    // ── Submodule: data ──
    add_submodule(m, "data", |sub| {
        sub.add_class::<data::PyHDict>()?;
        sub.add_class::<data::PyHGrid>()?;
        sub.add_class::<data::PyHList>()?;
        sub.add_class::<data::PyHCol>()?;
        Ok(())
    })?;

    // ── Submodule: codecs ──
    add_submodule(m, "codecs", |sub| {
        sub.add_function(wrap_pyfunction!(codecs::encode_grid, sub)?)?;
        sub.add_function(wrap_pyfunction!(codecs::decode_grid, sub)?)?;
        sub.add_function(wrap_pyfunction!(codecs::encode_scalar, sub)?)?;
        sub.add_function(wrap_pyfunction!(codecs::decode_scalar, sub)?)?;
        Ok(())
    })?;

    // ── Submodule: filter ──
    add_submodule(m, "filter", |sub| {
        sub.add_class::<filter::PyFilter>()?;
        sub.add_class::<filter::PyCmpOp>()?;
        sub.add_class::<filter::PyPath>()?;
        sub.add_function(wrap_pyfunction!(filter::parse_filter, sub)?)?;
        sub.add_function(wrap_pyfunction!(filter::matches_filter, sub)?)?;
        Ok(())
    })?;

    // ── Submodule: units ──
    add_submodule(m, "units", |sub| {
        sub.add_function(wrap_pyfunction!(units::convert, sub)?)?;
        sub.add_function(wrap_pyfunction!(units::compatible, sub)?)?;
        sub.add_function(wrap_pyfunction!(units::quantity, sub)?)?;
        sub.add_function(wrap_pyfunction!(units::base_unit, sub)?)?;
        Ok(())
    })?;

    // ── Submodule: graph ──
    add_submodule(m, "graph", |sub| {
        sub.add_class::<graph::PyEntityGraph>()?;
        sub.add_class::<graph::PySharedGraph>()?;
        sub.add_class::<graph::PyDiffOp>()?;
        sub.add_class::<graph::PyGraphDiff>()?;
        Ok(())
    })?;

    // ── Submodule: ontology ──
    add_submodule(m, "ontology", |sub| {
        sub.add_class::<ontology::PyDefNamespace>()?;
        sub.add_class::<ontology::PySpec>()?;
        sub.add_class::<ontology::PySlot>()?;
        sub.add_class::<ontology::PyDef>()?;
        sub.add_class::<ontology::PyDefKind>()?;
        sub.add_class::<ontology::PyLib>()?;
        Ok(())
    })?;

    // ── Submodule: auth ──
    add_submodule(m, "auth", |sub| {
        sub.add_function(wrap_pyfunction!(auth::derive_credentials, sub)?)?;
        sub.add_function(wrap_pyfunction!(auth::generate_nonce, sub)?)?;
        sub.add_function(wrap_pyfunction!(auth::client_first_message, sub)?)?;
        sub.add_function(wrap_pyfunction!(auth::client_final_message, sub)?)?;
        sub.add_function(wrap_pyfunction!(auth::extract_client_nonce, sub)?)?;
        sub.add_function(wrap_pyfunction!(auth::parse_auth_header, sub)?)?;
        sub.add_function(wrap_pyfunction!(auth::format_www_authenticate, sub)?)?;
        sub.add_function(wrap_pyfunction!(auth::format_auth_info, sub)?)?;
        Ok(())
    })?;

    // ── Submodule: client ──
    add_submodule(m, "client", |sub| {
        sub.add_class::<client::PyHaystackClient>()?;
        sub.add_class::<client::PyWsClient>()?;
        sub.add_class::<client::PyTlsConfig>()?;
        Ok(())
    })?;

    // ── Submodule: server ──
    add_submodule(m, "server", |sub| {
        sub.add_class::<server::PyHaystackServer>()?;
        sub.add_class::<server::PyAuthManager>()?;
        sub.add_class::<server::PyHisStore>()?;
        Ok(())
    })?;

    // ── Exceptions (always at top level) ──
    m.add(
        "HaystackError",
        m.py().get_type::<exceptions::HaystackError>(),
    )?;
    m.add("CodecError", m.py().get_type::<exceptions::CodecError>())?;
    m.add("FilterError", m.py().get_type::<exceptions::FilterError>())?;
    m.add("GraphError", m.py().get_type::<exceptions::GraphError>())?;
    m.add("AuthError", m.py().get_type::<exceptions::AuthError>())?;
    m.add("ClientError", m.py().get_type::<exceptions::ClientError>())?;

    Ok(())
}
