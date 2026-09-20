fn main() {
    use rocksdb::{IteratorMode, Options, DB};
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/caspar/node1/db".to_string());
    let mut opts = Options::default();
    opts.set_max_open_files(64);
    let db = DB::open_for_read_only(&opts, &path, false).expect("open db");
    let prefix = b"link::vmEntityType::";
    let iter = db.iterator(IteratorMode::Start);
    for item in iter {
        let (k, _v) = item.unwrap();
        if k.starts_with(prefix) {
            println!("{}", String::from_utf8_lossy(&k));
        }
    }
}
