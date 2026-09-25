# Executable examples

This directory contains deployable creature source projects, not runtime
implementations. The examples were moved here from the former SDK sample tree
so the client can upload them without mixing application code into an SDK.

`creatures/` currently demonstrates Docker, Elpian, Elpify, JavaScript, and
WebAssembly artifacts. Use the client in `apps/aseman-client` to create a
program and deploy the matching directory. Runtime implementations themselves
live under `modules/runtime/`.

Examples must use public Aseman contracts, contain no credentials, and remain
runnable against a disposable deployment.
