//! Generic response payload for the `command` action namespace.

#[derive(Debug, Clone, Default)]
pub struct Command {
    pub value: String,
    pub data: String,
}
