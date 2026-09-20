//! Store membership permissions. Absence and unknown legacy values deny by default.

use serde::{Deserialize, Serialize};

pub const PERM_READ: &str = "read";
pub const PERM_SIGNAL: &str = "signal";
pub const PERM_MANAGE: &str = "manage";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorePermissions {
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub signal: bool,
    #[serde(default)]
    pub manage: bool,
}

impl StorePermissions {
    #[must_use]
    pub const fn owner() -> Self {
        Self {
            read: true,
            signal: true,
            manage: true,
        }
    }
    #[must_use]
    pub const fn member() -> Self {
        Self {
            read: true,
            signal: true,
            manage: false,
        }
    }
    #[must_use]
    pub const fn viewer() -> Self {
        Self {
            read: true,
            signal: false,
            manage: false,
        }
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.read && !self.signal && !self.manage
    }

    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let mut permissions = Self::default();
        for part in raw.split(',') {
            match part.trim() {
                PERM_READ => permissions.read = true,
                PERM_SIGNAL => permissions.signal = true,
                PERM_MANAGE => permissions.manage = true,
                _ => {}
            }
        }
        permissions
    }

    #[must_use]
    pub fn encode(&self) -> String {
        let mut parts = Vec::with_capacity(3);
        if self.read {
            parts.push(PERM_READ);
        }
        if self.signal {
            parts.push(PERM_SIGNAL);
        }
        if self.manage {
            parts.push(PERM_MANAGE);
        }
        parts.join(",")
    }

    #[must_use]
    pub fn from_list(flags: &[String]) -> Self {
        Self::parse(&flags.join(","))
    }
}

#[must_use]
pub fn legacy_access_link_key(store_id: &str, member_id: &str) -> String {
    format!("onaccess::{store_id}::{member_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_round_trip_and_viewer_cannot_signal() {
        for permissions in [
            StorePermissions::owner(),
            StorePermissions::member(),
            StorePermissions::viewer(),
        ] {
            assert_eq!(StorePermissions::parse(&permissions.encode()), permissions);
        }
        assert!(!StorePermissions::viewer().signal);
    }

    #[test]
    fn absent_legacy_and_unknown_values_grant_nothing_extra() {
        assert!(StorePermissions::parse("").is_empty());
        assert!(StorePermissions::parse("true").is_empty());
        assert_eq!(
            StorePermissions::parse("read,teleport,signal"),
            StorePermissions::member()
        );
    }
}
