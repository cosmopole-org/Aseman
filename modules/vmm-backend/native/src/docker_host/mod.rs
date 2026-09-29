//! Docker-host bridge gateway.
//!
//! This is the unified ingress/egress for **docker-based creature programs**.
//! Such containers run sandboxed on the gateway docker network with no other
//! route to the outside world; their *only* channel is a single TCP connection
//! to the gateway served here. Over that one connection a container:
//!
//! * performs every host interaction — DB ops, storage ops, outbound HTTP,
//!   signalling other creatures, spawning sibling VMs — by issuing host-call
//!   `REQUEST`s that the node executes through its VM host functions, and
//! * receives `SIGNAL` pushes when other creatures signal it.
//!
//! ## Ownership & access
//!
//! The gateway is **not** a process-wide static: the native backend owns one
//! [`DockerHostGateway`] (moved out of the node in P5-06).
//!
//! ## Security
//!
//! A connection's identity is derived from its docker-network **source IP**: the
//! node asks docker which container owns that IP and maps the container name to
//! the identity it recorded at launch (the gateway's `identify`). The
//! container declares nothing and holds no secret, and it cannot forge its
//! bridge IP — so it can never impersonate another VM.
//!
//! The module is structured into:
//! * [`protocol`]   — the chunked wire framing,
//! * [`connection`] — per-connection state and the live-connection registry,
//! * [`dispatch`]   — mapping requests onto the unified host-function surface,
//! * [`gateway`]    — the owning `DockerHostGateway` instance,
//! * [`server`]     — the per-connection I/O loop.

pub(crate) mod connection;
pub(crate) mod dispatch;
pub(crate) mod gateway;
pub(crate) mod prelude;
pub(crate) mod protocol;
pub(crate) mod server;

pub(crate) use connection::ContainerIdentity;
pub(crate) use gateway::DockerHostGateway;
