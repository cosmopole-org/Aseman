//! Translation of `chain/common/trilean.go`.

/// A boolean that can also be undefined.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde_repr::Serialize_repr,
    serde_repr::Deserialize_repr,
)]
#[repr(i32)]
pub enum Trilean {
    /// The value has not been defined yet.
    Undefined = 0,
    /// The value is defined and true.
    True = 1,
    /// The value is defined and false.
    False = 2,
}

impl Default for Trilean {
    fn default() -> Self {
        Trilean::Undefined
    }
}

impl std::fmt::Display for Trilean {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Trilean::Undefined => "Undefined",
            Trilean::True => "True",
            Trilean::False => "False",
        };
        write!(f, "{}", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_undefined() {
        assert_eq!(Trilean::default(), Trilean::Undefined);
    }

    #[test]
    fn display_matches_go_strings() {
        assert_eq!(format!("{}", Trilean::Undefined), "Undefined");
        assert_eq!(format!("{}", Trilean::True), "True");
        assert_eq!(format!("{}", Trilean::False), "False");
    }

    #[test]
    fn serde_uses_repr_integer() {
        // The variants must round-trip through their repr(i32) representation
        // because the wire format is shared with the Go consensus engine.
        assert_eq!(serde_json::to_string(&Trilean::Undefined).unwrap(), "0");
        assert_eq!(serde_json::to_string(&Trilean::True).unwrap(), "1");
        assert_eq!(serde_json::to_string(&Trilean::False).unwrap(), "2");

        let parsed: Trilean = serde_json::from_str("1").unwrap();
        assert_eq!(parsed, Trilean::True);
        let parsed: Trilean = serde_json::from_str("2").unwrap();
        assert_eq!(parsed, Trilean::False);
    }

    #[test]
    fn serde_rejects_out_of_range_repr() {
        let parsed: Result<Trilean, _> = serde_json::from_str("3");
        assert!(parsed.is_err());
    }

    #[test]
    fn copy_and_equality() {
        let a = Trilean::True;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(a, Trilean::False);
    }
}
