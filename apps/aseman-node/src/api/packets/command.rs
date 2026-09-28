//! Generic response payload for the `command` action namespace.

#[derive(Debug, Clone, Default)]
#[expect(
    dead_code,
    reason = "RL-004: characterized legacy action surface (A008) kept until its deletion gate"
)]
pub struct Command {
    pub value: String,
    pub data: String,
}
