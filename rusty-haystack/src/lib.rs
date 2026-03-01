mod convert;
mod kinds;
mod data;
mod codecs;
mod filter;
mod graph;
mod ontology;

use pyo3::prelude::*;

#[pymodule]
fn rusty_haystack(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.1.0")?;

    // Kind types
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

    // Data types
    m.add_class::<data::PyHDict>()?;
    m.add_class::<data::PyHGrid>()?;
    m.add_class::<data::PyHList>()?;
    m.add_class::<data::PyHCol>()?;

    // Codec functions
    m.add_function(wrap_pyfunction!(codecs::encode_grid, m)?)?;
    m.add_function(wrap_pyfunction!(codecs::decode_grid, m)?)?;
    m.add_function(wrap_pyfunction!(codecs::encode_scalar, m)?)?;
    m.add_function(wrap_pyfunction!(codecs::decode_scalar, m)?)?;

    // Filter functions
    m.add_function(wrap_pyfunction!(filter::parse_filter, m)?)?;
    m.add_function(wrap_pyfunction!(filter::matches_filter, m)?)?;

    // Graph
    m.add_class::<graph::PyEntityGraph>()?;

    // Ontology
    m.add_class::<ontology::PyDefNamespace>()?;

    // Ontology - Xeto
    m.add_class::<ontology::PySpec>()?;
    m.add_class::<ontology::PySlot>()?;

    Ok(())
}
