//! Haystack HTTP API op handlers and route registration.

pub mod about;
pub mod data;
pub mod defs;
pub mod federation;
pub mod formats;
pub mod his;
pub mod invoke;
pub mod libs;
pub mod nav;
pub mod ops_handler;
pub mod point_write;
pub mod read;
pub mod rdf;
pub mod system;
pub mod watch;

use actix_web::web;

/// Configure all Haystack API routes under `/api`.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api")
            .route("/about", web::get().to(about::handle))
            .route("/ops", web::get().to(ops_handler::handle))
            .route("/formats", web::get().to(formats::handle))
            .route("/read", web::post().to(read::handle))
            .route("/nav", web::post().to(nav::handle))
            .route("/defs", web::post().to(defs::handle))
            .route("/libs", web::post().to(defs::handle_libs))
            .route("/watchSub", web::post().to(watch::handle_sub))
            .route("/watchPoll", web::post().to(watch::handle_poll))
            .route("/watchUnsub", web::post().to(watch::handle_unsub))
            .route("/pointWrite", web::post().to(point_write::handle))
            .route("/hisRead", web::post().to(his::handle_read))
            .route("/hisWrite", web::post().to(his::handle_write))
            .route("/invokeAction", web::post().to(invoke::handle))
            .route("/specs", web::post().to(libs::handle_specs))
            .route("/spec", web::post().to(libs::handle_spec))
            .route("/loadLib", web::post().to(libs::handle_load_lib))
            .route("/unloadLib", web::post().to(libs::handle_unload_lib))
            .route("/exportLib", web::post().to(libs::handle_export_lib))
            .route("/validate", web::post().to(libs::handle_validate))
            .route("/export", web::post().to(data::handle_export))
            .route("/import", web::post().to(data::handle_import))
            .route("/rdf/turtle", web::get().to(rdf::handle_turtle))
            .route("/rdf/jsonld", web::get().to(rdf::handle_jsonld))
            .route("/close", web::post().to(about::handle_close))
            .service(
                web::scope("/system")
                    .route("/status", web::get().to(system::handle_status))
                    .route("/backup", web::post().to(system::handle_backup))
                    .route("/restore", web::post().to(system::handle_restore)),
            )
            .service(
                web::scope("/federation")
                    .route("/status", web::get().to(federation::handle_status))
                    .route("/sync", web::post().to(federation::handle_sync)),
            ),
    );
}
