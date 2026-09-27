//! Bounded property/fuzz coverage for untrusted contract parsers (A1002).

use std::panic::{AssertUnwindSafe, catch_unwind};

use proptest::prelude::*;

use crate::capsule::CapsuleEnvelope;
use crate::guest_api::{WorkloadCredential, parse_proof_header};
use crate::migration::CanonicalCapsuleExport;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    })]

    #[test]
    fn proof_header_parser_never_panics_and_is_deterministic(input in ".{0,4096}") {
        let first = catch_unwind(AssertUnwindSafe(|| parse_proof_header(&input)));
        let second = catch_unwind(AssertUnwindSafe(|| parse_proof_header(&input)));
        prop_assert!(first.is_ok());
        prop_assert!(second.is_ok());
        prop_assert_eq!(first.unwrap().ok(), second.unwrap().ok());
    }

    #[test]
    fn workload_credential_parser_never_panics(input in ".{0,4096}") {
        prop_assert!(catch_unwind(AssertUnwindSafe(|| WorkloadCredential::decode(&input))).is_ok());
    }

    #[test]
    fn capsule_envelope_parser_never_panics(input in prop::collection::vec(any::<u8>(), 0..8192)) {
        prop_assert!(catch_unwind(AssertUnwindSafe(|| CapsuleEnvelope::from_canonical_bytes(&input))).is_ok());
    }

    #[test]
    fn migration_json_parser_and_validator_never_panic(input in prop::collection::vec(any::<u8>(), 0..16384)) {
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            serde_json::from_slice::<CanonicalCapsuleExport>(&input)
                .ok()
                .map(|export| export.validate())
        }));
        prop_assert!(parsed.is_ok());
    }
}
