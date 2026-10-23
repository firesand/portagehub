//! Shared pieces between the GUI (`portagehub`) and the privileged helper
//! (`portagehub-helper`): the JSON-lines protocol, process streaming and the
//! request handler.

pub mod protocol;
pub mod rootops;
pub mod server;
pub mod stream;
