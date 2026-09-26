//! Wire-level packet types exchanged across the node boundary.
//!
//! Every concrete type is re-exported here so callers can `use
//! crate::models::packet::*` without reaching into the submodules.

pub mod command;
pub mod consume;
pub mod error;
pub mod origin_file;
pub mod origin_file_response;
pub mod origin_packet;
pub mod packet;
pub mod signal_tags;
pub mod simple;

pub use command::Command;
pub use consume::ConsumeTokenInput;
pub use error::{Error, build_error_json};
pub use origin_file::OriginFile;
pub use origin_file_response::OriginFileRes;
pub use origin_packet::OriginPacket;
pub use packet::{BuildPacket, LogPacket, Packet};
pub use signal_tags::{LogQuery, decode_tags, encode_tags, validate_tag, validate_tags};
pub use simple::ResponseSimpleMessage;
