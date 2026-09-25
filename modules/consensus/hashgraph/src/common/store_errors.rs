//! Translation of `chain/common/store_errors.go`.

/// Encodes the nature of a [`StoreErr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum StoreErrType {
    /// An item is not found.
    KeyNotFound = 0,
    /// An item is no longer in the store because it was evicted from the cache.
    TooLate = 1,
    /// An attempt was made to insert a non-sequential item in a cache.
    SkippedIndex = 2,
    /// An attempt was made to retrieve objects associated to a non-existent
    /// participant.
    UnknownParticipant = 3,
    /// A cache is empty.
    Empty = 4,
    /// An attempt was made to insert an item already present in a cache.
    KeyAlreadyExists = 5,
}

/// A generic error type that encodes errors when accessing objects in the
/// hashgraph store.
#[derive(Debug, Clone)]
pub struct StoreErr {
    pub data_type: String,
    pub err_type: StoreErrType,
    pub key: String,
}

/// Creates a [`StoreErr`] pertaining to an object identified by its `data_type`
/// and `key`. The `err_type` determines the nature of the error.
pub fn new_store_err(data_type: &str, err_type: StoreErrType, key: &str) -> StoreErr {
    StoreErr {
        data_type: data_type.to_string(),
        err_type,
        key: key.to_string(),
    }
}

impl StoreErr {
    pub fn new(data_type: &str, err_type: StoreErrType, key: &str) -> StoreErr {
        new_store_err(data_type, err_type, key)
    }
}

impl std::fmt::Display for StoreErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = match self.err_type {
            StoreErrType::KeyNotFound => "Not Found",
            StoreErrType::TooLate => "Too Late",
            StoreErrType::SkippedIndex => "Skipped Index",
            StoreErrType::UnknownParticipant => "Unknown Participant",
            StoreErrType::Empty => "Empty",
            StoreErrType::KeyAlreadyExists => "Key Already Exists",
        };
        write!(f, "{}, {}, {}", self.data_type, self.key, m)
    }
}

impl std::error::Error for StoreErr {}

/// Checks that an error is a [`StoreErr`] and that its code matches the
/// provided [`StoreErrType`].
pub fn is_store(err: &anyhow::Error, t: StoreErrType) -> bool {
    err.downcast_ref::<StoreErr>()
        .map(|store_err| store_err.err_type == t)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format_matches_data_key_message() {
        let err = new_store_err("Event", StoreErrType::KeyNotFound, "abc");
        assert_eq!(format!("{}", err), "Event, abc, Not Found");

        let err = new_store_err("Round", StoreErrType::TooLate, "9");
        assert_eq!(format!("{}", err), "Round, 9, Too Late");

        let err = new_store_err("Event", StoreErrType::SkippedIndex, "42");
        assert_eq!(format!("{}", err), "Event, 42, Skipped Index");

        let err = new_store_err("Peer", StoreErrType::UnknownParticipant, "p1");
        assert_eq!(format!("{}", err), "Peer, p1, Unknown Participant");

        let err = new_store_err("Cache", StoreErrType::Empty, "");
        assert_eq!(format!("{}", err), "Cache, , Empty");

        let err = new_store_err("Map", StoreErrType::KeyAlreadyExists, "k");
        assert_eq!(format!("{}", err), "Map, k, Key Already Exists");
    }

    #[test]
    fn is_store_matches_only_same_type() {
        let raw = new_store_err("Event", StoreErrType::TooLate, "9");
        let err: anyhow::Error = raw.into();
        assert!(is_store(&err, StoreErrType::TooLate));
        assert!(!is_store(&err, StoreErrType::KeyNotFound));
    }

    #[test]
    fn is_store_returns_false_for_unrelated_error() {
        let err: anyhow::Error = anyhow::anyhow!("some other error");
        assert!(!is_store(&err, StoreErrType::TooLate));
    }

    #[test]
    fn store_err_is_clonable_and_carries_fields() {
        let err = StoreErr::new("D", StoreErrType::Empty, "K");
        let cloned = err.clone();
        assert_eq!(cloned.data_type, "D");
        assert_eq!(cloned.key, "K");
        assert_eq!(cloned.err_type, StoreErrType::Empty);
    }
}
