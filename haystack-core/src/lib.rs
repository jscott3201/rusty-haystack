//! # Haystack Core
//!
//! Rust implementation of the [Project Haystack](https://project-haystack.org) data model,
//! codecs, filter engine, entity graph, ontology system, and SCRAM SHA-256 authentication.
//!
//! ## Crate Organization
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`kinds`] | Central value type ([`Kind`](kinds::Kind)) with 15 scalar types (Marker, Number, Str, Ref, etc.) |
//! | [`data`] | Collection types: [`HDict`](data::HDict) (tag map), [`HGrid`](data::HGrid) (table), [`HCol`](data::HCol), [`HList`](data::HList) |
//! | [`codecs`] | Wire format codecs: Zinc, Trio, JSON, Haystack JSON v3, CSV, and RDF (Turtle/JSON-LD) |
//! | [`filter`] | Haystack filter expression parser and evaluator (`site and area > 1000`) |
//! | [`graph`] | In-memory entity graph with bitmap tag indexes, B-tree value indexes, ref adjacency, CSR, and change tracking |
//! | [`ontology`] | Haystack 4 def/lib/namespace system with taxonomy, validation, and Xeto support |
//! | [`auth`] | SCRAM SHA-256 authentication per the Haystack auth specification |
//! | [`xeto`] | Xeto schema language parser and structural type fitting |
//!
//! ## Quick Start
//!
//! ```rust
//! use haystack_core::data::{HDict, HGrid};
//! use haystack_core::kinds::{Kind, Number, HRef};
//! use haystack_core::graph::EntityGraph;
//! use haystack_core::codecs::codec_for;
//!
//! // Build an entity
//! let mut site = HDict::new();
//! site.set("id", Kind::Ref(HRef::from_val("site-1")));
//! site.set("dis", Kind::Str("Main Campus".into()));
//! site.set("site", Kind::Marker);
//! site.set("area", Kind::Number(Number::unitless(50000.0)));
//!
//! // Add to graph and query
//! let mut graph = EntityGraph::new();
//! graph.add(site).unwrap();
//! let results = graph.read_all("site and area > 1000", 0).unwrap();
//! assert_eq!(results.len(), 1);
//!
//! // Encode to Zinc wire format
//! let zinc = codec_for("text/zinc").unwrap();
//! let grid = graph.to_grid("").unwrap();
//! let encoded = zinc.encode_grid(&grid).unwrap();
//! ```

pub mod auth;
pub mod codecs;
pub mod data;
pub mod filter;
pub mod graph;
pub mod kinds;
pub mod ontology;
pub mod xeto;
