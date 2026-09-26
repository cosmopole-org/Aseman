//! Shell-API surface — action handlers, persisted models, and the packet
//! schemas (request / response / federation-broadcast payloads) each
//! action namespace consumes and produces.

pub mod actions;
#[path = "main.rs"]
pub mod main_api;
pub mod model;
pub mod packets;
