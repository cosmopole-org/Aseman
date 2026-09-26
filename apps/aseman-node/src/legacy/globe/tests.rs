//! Real-code reproduction of the `message_callbacks` leak, driven through
//! the actual `Globe::send_typed_message_on_chain`. The message-callback
//! sink here stands in for `CoreOrchestrator::message_callbacks` (the map
//! the real `set_message_cb_fn` closure writes into); process RSS is read
//! the same way the node's pprof `/debug/pprof/heap` endpoint reads it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::legacy::globe::{
    Globe, SetChainCallbackFn, SetMessageCbFn, SignPacketFn, SubmitChainPacketFn,
};
use crate::models::chain::MessageCallback;
use crate::models::globe::{IGlobe, TypedMessageCallback};

/// VmRSS in KiB from /proc/self/status — the number pprof's heap endpoint
/// surfaces. Returns 0 on platforms without procfs.
fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                let rest = l.strip_prefix("VmRSS:")?;
                rest.split_whitespace().next()?.parse::<u64>().ok()
            })
        })
        .unwrap_or(0)
}

/// Build a `Globe` whose `set_message_cb_fn` records into `sink` — exactly
/// what the orchestrator's real closure does into `message_callbacks`.
fn globe_with_sink(sink: Arc<Mutex<HashMap<String, MessageCallback>>>) -> Arc<Globe> {
    let sign_fn: SignPacketFn = Arc::new(|_| String::new());
    let submit_fn: SubmitChainPacketFn = Arc::new(|_, _| {});
    let set_chain_cb: SetChainCallbackFn = Arc::new(|_, _| {});
    let set_msg_cb: SetMessageCbFn = {
        let sink = sink.clone();
        Arc::new(move |id: &str, cb: MessageCallback| {
            sink.lock().unwrap().insert(id.to_string(), cb);
        })
    };
    Globe::new("node1".into(), sign_fn, submit_fn, set_chain_cb, set_msg_cb)
}

fn fire(globe: &Globe, callback: Option<TypedMessageCallback>) {
    let mut receivers: HashMap<String, HashMap<String, bool>> = HashMap::new();
    receivers.insert("*".to_string(), HashMap::new());
    globe.send_typed_message_on_chain(
        "main",
        "chains/vm/request",
        "vm.chain",
        b"{}".to_vec(),
        "",
        "owner",
        receivers,
        "",
        "store1",
        None,
        callback,
    );
}

// The fix: fire-and-forget sends pass `None`, so nothing is parked in the
// callback sink no matter how many messages are sent.
#[test]
fn fire_and_forget_none_does_not_grow_callback_map() {
    let sink: Arc<Mutex<HashMap<String, MessageCallback>>> = Arc::new(Mutex::new(HashMap::new()));
    let globe = globe_with_sink(sink.clone());

    let n = 200_000;
    let rss_before = rss_kib();
    for _ in 0..n {
        fire(&globe, None);
    }
    let rss_after = rss_kib();

    let len = sink.lock().unwrap().len();
    eprintln!(
        "[none] sent={} parked_callbacks={} rss_kib {} -> {} (delta {})",
        n,
        len,
        rss_before,
        rss_after,
        rss_after as i64 - rss_before as i64,
    );
    assert_eq!(len, 0, "fire-and-forget must not park any callback");
}

// Counterpart proving the sink genuinely reflects registrations: a bounded
// batch of `Some` sends parks exactly that many callbacks. Before the fix
// every send (all fire-and-forget in the tree) took this branch and nothing
// ever removed the entries — measured at ~309 B/entry, i.e. ~1.2 GB per ~4M
// messages, the observed production growth. `Some` callbacks are now
// one-shot (removed on reply delivery in the orchestrator).
#[test]
fn registered_some_parks_one_callback_each() {
    let sink: Arc<Mutex<HashMap<String, MessageCallback>>> = Arc::new(Mutex::new(HashMap::new()));
    let globe = globe_with_sink(sink.clone());

    let n = 1_000;
    for _ in 0..n {
        fire(&globe, Some(Box::new(|_, _| {})));
    }
    assert_eq!(
        sink.lock().unwrap().len(),
        n,
        "each registered send parks exactly one callback (the pre-fix leak)"
    );
}
