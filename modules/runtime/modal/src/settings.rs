//! The plugin's settings, read once from the runtime configuration when the plugin
//! is registered (`ASEMAN_MODAL_*`, `ASEMAN_VM_HTTP_*`).

use std::time::Duration;

use crate::client::ModalCredentials;
use crate::models::sanitize_component;

/// Version reported to Modal in `x-modal-client-version` when none is configured.
///
/// Modal PARSES this and refuses anything it cannot read as a version of a
/// supported client — `FailedPrecondition: Invalid client version` — so it is not
/// a free-form identifier. A name-and-slash string ("aseman-vm-modal/0.1.0") is
/// rejected outright, which is why this is a bare semver: it is a compatibility
/// assertion, and Modal enforces it. Modal raises its minimum supported client over
/// time, so it is configurable.
const DEFAULT_CLIENT_VERSION: &str = "1.0.0";

#[derive(Clone, Debug)]
pub struct ModalSettings {
    /// The API credentials, or why they are missing.
    pub(crate) credentials: Result<ModalCredentials, String>,
    pub(crate) client_version: String,
    /// The Modal app every sandbox on this node belongs to.
    pub(crate) app_name: String,
    /// The prefix of every volume name.
    pub(crate) volume_prefix: String,
    pub(crate) builder_version: String,
    /// The base image used when a packet names none.
    pub(crate) default_image: String,
    pub(crate) image_build_timeout: Duration,
    /// A configured sandbox lifetime, overriding the one derived from limits.
    pub(crate) sandbox_timeout_seconds: Option<u32>,
    pub(crate) task_ready_timeout_seconds: f32,
    /// Where the VM's volume is mounted inside the sandbox.
    pub(crate) volume_mount_path: String,
    /// How long to wait for volume writes to settle; zero turns it off.
    pub(crate) volume_settle: Duration,
    /// The port inside the sandbox the VMM ingress forwards HTTP to; it matches
    /// the docker runtime's so a creature serves on the same port under either.
    pub(crate) vm_http_port: u32,
    /// The timeout of a forwarded HTTP request to the sandbox.
    pub(crate) vm_http_timeout: Duration,
}

impl ModalSettings {
    #[must_use]
    pub fn from_config(config: &aseman_config::RuntimeConfig) -> Self {
        // Modal groups sandboxes and volumes under an app; one per node, named by
        // `MODAL_APP_NAME`, else by `MODAL_APP_PREFIX`.
        let app_name = sanitize_component(
            config
                .modal_app_name
                .as_deref()
                .unwrap_or(&config.modal_app_prefix),
        );
        let client_version = if config.modal_client_version.is_empty() {
            DEFAULT_CLIENT_VERSION.to_owned()
        } else {
            config.modal_client_version.clone()
        };
        let timeout_seconds = Some(config.vm_http_timeout_seconds)
            .filter(|seconds| *seconds > 0)
            .unwrap_or(30);
        Self {
            credentials: ModalCredentials::from_config(config),
            client_version,
            app_name,
            volume_prefix: config.modal_app_prefix.clone(),
            builder_version: config.modal_builder_version.clone(),
            default_image: config.modal_default_image.clone(),
            image_build_timeout: Duration::from_secs(config.modal_image_build_timeout_seconds),
            sandbox_timeout_seconds: config.modal_sandbox_timeout_seconds,
            task_ready_timeout_seconds: config.modal_task_ready_timeout_seconds,
            volume_mount_path: config.modal_volume_mount_path.clone(),
            volume_settle: Duration::from_millis(config.modal_volume_settle_ms),
            vm_http_port: u32::from(config.vm_http_port),
            vm_http_timeout: Duration::from_secs(timeout_seconds),
        }
    }

    /// Whether the node is configured to talk to Modal at all.
    #[must_use]
    pub(crate) fn is_configured(&self) -> bool {
        self.credentials.is_ok()
    }

    /// The node-wide app cache key: one Modal app owns every project sandbox.
    #[must_use]
    pub(crate) fn shared_app_link_key(&self) -> String {
        format!("ModalApp::{}", self.app_name)
    }

    /// The deterministic Modal volume name of one VM instance.
    #[must_use]
    pub(crate) fn volume_name(&self, vm_id: &str) -> String {
        format!("{}-vol-{}", self.volume_prefix, sanitize_component(vm_id))
    }
}
