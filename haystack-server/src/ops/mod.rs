//! Haystack HTTP API op handlers.
//!
//! Each sub-module implements one or more Haystack ops as async Axum
//! handler functions. Routes are registered in [`app::HaystackServer`].
//!
//! Standard ops: `about`, `ops`, `formats`, `read`, `nav`, `defs`, `libs`,
//! `watchSub`/`watchPoll`/`watchUnsub`, `hisRead`/`hisWrite`,
//! `pointWrite`, `invokeAction`, `close`.
//!
//! Extended ops: `changes`, `export`/`import`, `specs`/`spec`/`loadLib`/
//! `unloadLib`/`exportLib`/`validate`.

pub mod about;
pub mod changes;
pub mod data;
pub mod defs;
pub mod formats;
pub mod his;
pub mod invoke;
pub mod libs;
pub mod nav;
pub mod ops_handler;
pub mod point_write;
pub mod read;
pub mod watch;
