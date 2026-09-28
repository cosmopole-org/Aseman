//! The native backend links WasmEdge dynamically (`libwasmedge.so.0`). The installer
//! places it in `<prefix>/lib/aseman` beside `<prefix>/bin`, so the binaries look there
//! first and run without `LD_LIBRARY_PATH` (contracts/release/runtime-dependencies.json).
fn main() {
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN/../lib/aseman");
}
