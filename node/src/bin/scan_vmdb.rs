//! Diagnostic: list the `vmEntityType` links in a legacy application store, read
//! through the legacy provider's read-only snapshot source.

use aseman_storage_legacy::{LegacyRecordSource, RocksDbLegacySource};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/caspar/node1/db".to_string());
    let source = RocksDbLegacySource::open_read_only(std::path::Path::new(&path), "scan-vmdb")
        .expect("open db");
    let snapshot = source
        .read_snapshot(
            aseman_storage_legacy::DEFAULT_MAX_EXPORT_RECORDS,
            aseman_storage_legacy::DEFAULT_MAX_EXPORT_BYTES,
        )
        .expect("read db");
    for record in snapshot.records {
        if record.key.starts_with(b"link::vmEntityType::") {
            println!("{}", String::from_utf8_lossy(&record.key));
        }
    }
}
