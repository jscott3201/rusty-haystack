//! RDF export endpoints — Turtle and JSON-LD serialization of all entities.

use actix_web::{HttpResponse, web};

use crate::state::AppState;

/// GET /api/rdf/turtle
///
/// Returns all entities in the graph serialized as RDF Turtle.
pub async fn handle_turtle(state: web::Data<AppState>) -> HttpResponse {
    let entities = state.graph.read(|g| {
        g.to_grid("")
            .map(|grid| grid.rows.clone())
            .unwrap_or_default()
    });
    let turtle = haystack_core::codecs::rdf::to_turtle(&entities);
    HttpResponse::Ok().content_type("text/turtle").body(turtle)
}

/// GET /api/rdf/jsonld
///
/// Returns all entities in the graph serialized as JSON-LD.
pub async fn handle_jsonld(state: web::Data<AppState>) -> HttpResponse {
    let entities = state.graph.read(|g| {
        g.to_grid("")
            .map(|grid| grid.rows.clone())
            .unwrap_or_default()
    });
    let jsonld = haystack_core::codecs::rdf::to_jsonld(&entities);
    HttpResponse::Ok()
        .content_type("application/ld+json")
        .body(jsonld)
}
