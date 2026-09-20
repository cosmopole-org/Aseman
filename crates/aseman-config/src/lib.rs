//! Typed Aseman configuration with explicit source precedence and bounded legacy aliases.
#![forbid(unsafe_code)]

use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::OnceLock;
use thiserror::Error;

const LEGACY_ALIASES_JSON: &str = include_str!("../../../contracts/config/legacy-aliases.json");

#[derive(Clone, Debug, PartialEq)]
pub struct AsemanConfig {
    pub node: NodeIdentityConfig,
    pub network: NetworkConfig,
    pub storage: LegacyStorageConfig,
    pub allocator: AllocatorConfig,
    pub telemetry: TelemetryConfig,
    pub cluster: ClusterBootstrapConfig,
    pub core: CoreConfig,
    pub rate_limit: RateLimitConfig,
    pub legacy_adapters: LegacyAdapterConfig,
    pub runtime: RuntimeConfig,
    pub vmm_endpoint: String,
    pub database_url_secret: Option<String>,
    pub legacy_aliases_used: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeIdentityConfig {
    pub id: String,
    pub private_key_secret: String,
    pub origin: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkConfig {
    pub public_http_port: u16,
    pub public_storage_port: u16,
    pub docker_gateway_port: u16,
    pub vm_http_ingress_port: u16,
    pub legacy_tcp_port: u16,
    pub legacy_ws_port: u16,
    pub legacy_federation_port: u16,
    pub legacy_consensus_port: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyStorageConfig {
    pub root_path: String,
    pub base_db_path: String,
    pub applet_db_path: String,
    pub store_logs_db: String,
    pub search_index_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocatorConfig {
    pub arena_max: i32,
    pub trim_interval_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetryConfig {
    pub database_path: String,
    pub api_port: u16,
    pub pprof_port: u16,
    pub entity_port: u16,
    pub vm_port: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClusterBootstrapConfig {
    pub config_path: Option<String>,
    pub enabled: Option<bool>,
    pub bootstrap: Option<bool>,
    pub node_id: Option<u64>,
    pub node_name: Option<String>,
    pub region: Option<String>,
    pub listen_addr: Option<String>,
    pub advertise_addr: Option<String>,
    pub auth_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoreConfig {
    pub root_node: Option<String>,
    pub tls_certificate_path: Option<String>,
    pub tls_private_key_path: Option<String>,
    pub execution_cost_per_second: i64,
    pub ram_cost_per_mb_minute: i64,
    pub cpu_core_cost_per_minute: i64,
    pub disk_cost_per_gb_minute: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub authenticated_rps: f64,
    pub authenticated_burst: f64,
    pub anonymous_rps: f64,
    pub anonymous_burst: f64,
    pub global_rps: f64,
    pub global_burst: f64,
    pub idle_evict_seconds: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyAdapterConfig {
    pub main_port: String,
    pub login_grant_required: bool,
    pub public_storage_max_bytes: usize,
    pub questdb_port: u16,
    pub rocksdb_max_open_files: i32,
    pub rocksdb_block_cache_mb: usize,
    pub rocksdb_write_buffer_mb: usize,
    pub babble_data_dir: Option<String>,
    pub babble_frame_cache: usize,
    pub babble_frame_retention: i64,
    pub is_head: bool,
    pub shardchain_script: String,
    pub blockchain_api_port: u16,
    pub ip_address: String,
    pub home_dir: Option<String>,
    pub user_profile_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeConfig {
    pub storage_root: Option<String>,
    pub vm_http_port: u16,
    pub vm_http_timeout_seconds: u64,
    pub docker_gateway_network: String,
    pub docker_runtime: Option<String>,
    pub docker_disk_quota: bool,
    pub docker_gateway_host: String,
    pub docker_gateway_port: u16,
    pub firecracker_binary: String,
    pub firecracker_kernel_image: Option<String>,
    pub firecracker_rootfs_image: Option<String>,
    pub firecracker_boot_args: String,
    pub wasm_aot: bool,
    pub wasm_vm_cache: bool,
    pub modal_api_key: String,
    pub modal_token_id: String,
    pub modal_token_secret: String,
    pub modal_environment: String,
    pub modal_server_url: String,
    pub modal_client_version: String,
    pub modal_app_name: Option<String>,
    pub modal_app_prefix: String,
    pub modal_builder_version: String,
    pub modal_default_image: String,
    pub modal_image_build_timeout_seconds: u64,
    pub modal_sandbox_timeout_seconds: Option<u32>,
    pub modal_task_ready_timeout_seconds: f32,
    pub modal_volume_mount_path: String,
    pub modal_volume_settle_ms: u64,
    pub probe_machine_id: String,
    pub probe_vm_id: String,
    pub probe_command: String,
}

impl RuntimeConfig {
    fn from_canonical(values: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        Ok(Self {
            storage_root: nonempty(values, "ASEMAN_LEGACY_STORAGE_ROOT_PATH"),
            vm_http_port: parse_or(values, "ASEMAN_VM_HTTP_PORT", 8080)?,
            vm_http_timeout_seconds: parse_or(values, "ASEMAN_VM_HTTP_TIMEOUT_SECS", 30)?,
            docker_gateway_network: value_or(values, "ASEMAN_GATEWAY_NETWORK", "kasper"),
            docker_runtime: match values.get("ASEMAN_DOCKER_RUNTIME") {
                None => Some("runsc".to_owned()),
                Some(value)
                    if value.trim().is_empty()
                        || value.trim().eq_ignore_ascii_case("default")
                        || value.trim().eq_ignore_ascii_case("runc") =>
                {
                    None
                }
                Some(value) => Some(value.trim().to_owned()),
            },
            docker_disk_quota: !falsey(values.get("ASEMAN_DOCKER_DISK_QUOTA")),
            docker_gateway_host: value_or(
                values,
                "ASEMAN_LEGACY_DOCKER_HOST_GATEWAY_ADVERTISE_HOST",
                "host.docker.internal",
            ),
            docker_gateway_port: parse_or(values, "ASEMAN_LEGACY_DOCKER_HOST_GATEWAY_PORT", 8079)?,
            firecracker_binary: value_or(
                values,
                "ASEMAN_LEGACY_FIRECRACKER_BIN",
                "/usr/local/bin/firecracker",
            ),
            firecracker_kernel_image: nonempty(values, "ASEMAN_LEGACY_FIRECRACKER_KERNEL_IMAGE"),
            firecracker_rootfs_image: nonempty(values, "ASEMAN_LEGACY_FIRECRACKER_ROOTFS_IMAGE"),
            firecracker_boot_args: value_or(
                values,
                "ASEMAN_LEGACY_FIRECRACKER_BOOT_ARGS",
                "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw",
            ),
            wasm_aot: !falsey(values.get("ASEMAN_WASM_AOT")),
            wasm_vm_cache: !falsey(values.get("ASEMAN_WASM_VM_CACHE")),
            modal_api_key: optional(values, "ASEMAN_LEGACY_MODAL_API_KEY"),
            modal_token_id: optional(values, "ASEMAN_LEGACY_MODAL_TOKEN_ID"),
            modal_token_secret: optional(values, "ASEMAN_LEGACY_MODAL_TOKEN_SECRET"),
            modal_environment: optional(values, "ASEMAN_LEGACY_MODAL_ENVIRONMENT"),
            modal_server_url: value_or(
                values,
                "ASEMAN_LEGACY_MODAL_SERVER_URL",
                "https://api.modal.com:443",
            ),
            modal_client_version: value_or(values, "ASEMAN_LEGACY_MODAL_CLIENT_VERSION", "1.0.0"),
            modal_app_name: nonempty(values, "ASEMAN_LEGACY_MODAL_APP_NAME"),
            modal_app_prefix: value_or(values, "ASEMAN_LEGACY_MODAL_APP_PREFIX", "caspar"),
            modal_builder_version: optional(values, "ASEMAN_LEGACY_MODAL_BUILDER_VERSION"),
            modal_default_image: value_or(
                values,
                "ASEMAN_LEGACY_MODAL_DEFAULT_IMAGE",
                "ubuntu:24.04",
            ),
            modal_image_build_timeout_seconds: parse_or(
                values,
                "ASEMAN_LEGACY_MODAL_IMAGE_BUILD_TIMEOUT_SECS",
                900,
            )?,
            modal_sandbox_timeout_seconds: parse_optional(
                values,
                "ASEMAN_LEGACY_MODAL_SANDBOX_TIMEOUT_SECS",
            )?,
            modal_task_ready_timeout_seconds: parse_or(
                values,
                "ASEMAN_LEGACY_MODAL_TASK_READY_TIMEOUT_SECS",
                60.0,
            )?,
            modal_volume_mount_path: value_or(
                values,
                "ASEMAN_LEGACY_MODAL_VOLUME_MOUNT_PATH",
                "/data",
            ),
            modal_volume_settle_ms: parse_or(values, "ASEMAN_LEGACY_MODAL_VOLUME_SETTLE_MS", 3000)?,
            probe_machine_id: optional(values, "ASEMAN_LEGACY_PROBE_MACHINE_ID"),
            probe_vm_id: optional(values, "ASEMAN_LEGACY_PROBE_VM_ID"),
            probe_command: value_or(
                values,
                "ASEMAN_LEGACY_PROBE_CMD",
                "tail -60 /var/log/decillion/bridge.log 2>&1",
            ),
        })
    }

    pub fn from_process() -> Result<Self, ConfigError> {
        let values: BTreeMap<String, String> = std::env::vars().collect();
        let (values, _) = canonicalize(&values)?;
        Self::from_canonical(&values)
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::from_canonical(&BTreeMap::new()).expect("runtime defaults are valid")
    }
}

pub fn runtime_config() -> RuntimeConfig {
    ACTIVE_CONFIG
        .get()
        .map(|config| config.runtime.clone())
        .unwrap_or_else(|| RuntimeConfig::from_process().unwrap_or_default())
}

static ACTIVE_CONFIG: OnceLock<AsemanConfig> = OnceLock::new();
static ACTIVE_CLI_CONFIG: OnceLock<CliConfig> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliConfig {
    pub cluster_endpoint: String,
    pub cluster_token: String,
    pub telemetry_url: String,
    pub pprof_url: String,
    pub library_path: Option<String>,
    pub executable_path: Option<OsString>,
    pub owner_username: Option<String>,
    pub owner_email: Option<String>,
    pub vms_dir: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrationTestConfig {
    pub postgres_url: Option<String>,
}

impl IntegrationTestConfig {
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            postgres_url: std::env::var("ASEMAN_TEST_POSTGRES_URL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
        }
    }
}

impl CliConfig {
    pub fn from_process() -> Result<Self, ConfigError> {
        let values: BTreeMap<String, String> = std::env::vars().collect();
        let (values, _) = canonicalize(&values)?;
        Ok(Self {
            cluster_endpoint: values
                .get("ASEMAN_LEGACY_CASPARCTL_CLUSTER_ENDPOINT")
                .cloned()
                .unwrap_or_else(|| "http://127.0.0.1:7440".to_owned()),
            cluster_token: optional(&values, "ASEMAN_CLUSTER_TOKEN"),
            telemetry_url: values
                .get("ASEMAN_LEGACY_CASPARCTL_TELEMETRY")
                .cloned()
                .unwrap_or_else(|| "http://127.0.0.1:9099/telemetry/snapshot".to_owned()),
            pprof_url: values
                .get("ASEMAN_LEGACY_CASPARCTL_PPROF")
                .cloned()
                .unwrap_or_else(|| "http://127.0.0.1:9999".to_owned()),
            library_path: nonempty(&values, "ASEMAN_LEGACY_LD_LIBRARY_PATH"),
            executable_path: std::env::var_os("PATH"),
            owner_username: nonempty(&values, "ASEMAN_OWNER_USERNAME"),
            owner_email: nonempty(&values, "ASEMAN_OWNER_EMAIL"),
            vms_dir: nonempty(&values, "ASEMAN_VMS_DIR"),
        })
    }
}

pub fn install_cli_process_config() -> Result<(), ConfigError> {
    ACTIVE_CLI_CONFIG
        .set(CliConfig::from_process()?)
        .map_err(|_| ConfigError::AlreadyInstalled)
}

pub fn cli_config() -> Option<&'static CliConfig> {
    ACTIVE_CLI_CONFIG.get()
}

/// Install the validated process snapshot for legacy leaf adapters that cannot yet
/// accept constructor injection. New code should receive the narrow typed sub-config.
pub fn install_legacy_adapter_snapshot(config: &AsemanConfig) -> Result<(), ConfigError> {
    ACTIVE_CONFIG
        .set(config.clone())
        .map_err(|_| ConfigError::AlreadyInstalled)
}

pub fn legacy_adapter_snapshot() -> Option<&'static LegacyAdapterConfig> {
    ACTIVE_CONFIG.get().map(|config| &config.legacy_adapters)
}

/// Process home lookup for executables that do not load the full node configuration.
pub fn process_home_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|value| !value.is_empty())
        })
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ConfigError {
    #[error("missing configuration key {0}")]
    Missing(&'static str),
    #[error("conflicting canonical and legacy values for {canonical}/{legacy}")]
    AliasConflict { canonical: String, legacy: String },
    #[error("invalid value for {key}: {reason}")]
    Invalid {
        key: &'static str,
        reason: &'static str,
    },
    #[error("could not read dotenv file: {0}")]
    DotenvIo(String),
    #[error("invalid dotenv line {line}")]
    InvalidDotenv { line: usize },
    #[error("the process configuration snapshot is already installed")]
    AlreadyInstalled,
    #[error("embedded configuration alias catalog is invalid")]
    InvalidAliasCatalog,
    #[error("could not read JSON configuration: {0}")]
    JsonIo(String),
    #[error("invalid JSON configuration: {0}")]
    InvalidJson(String),
    #[error("could not read secret file: {0}")]
    SecretIo(String),
    #[error("secret file is empty or exceeds its byte limit")]
    InvalidSecret,
}

pub fn read_json_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, ConfigError> {
    let bytes = fs::read(path).map_err(|error| ConfigError::JsonIo(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| ConfigError::InvalidJson(error.to_string()))
}

pub fn read_secret_file(path: impl AsRef<Path>, max_bytes: usize) -> Result<String, ConfigError> {
    let bytes = fs::read(path).map_err(|error| ConfigError::SecretIo(error.to_string()))?;
    if bytes.is_empty() || bytes.len() > max_bytes || bytes.contains(&0) {
        return Err(ConfigError::InvalidSecret);
    }
    let secret = String::from_utf8(bytes).map_err(|_| ConfigError::InvalidSecret)?;
    let secret = secret.trim_end_matches(['\r', '\n']);
    if secret.is_empty() {
        return Err(ConfigError::InvalidSecret);
    }
    Ok(secret.to_owned())
}

#[derive(Deserialize)]
struct AliasCatalog {
    aliases: Vec<AliasRow>,
}

#[derive(Deserialize)]
struct AliasRow {
    legacy: String,
    canonical: String,
}

impl AsemanConfig {
    /// Load the process environment once, then apply the legacy `.env` precedence.
    ///
    /// The old node loaded `.env` after process startup and overwrote matching process
    /// variables. This deliberately preserves that behavior during the compatibility
    /// window without mutating the process environment. A missing file is allowed.
    pub fn from_process_with_dotenv(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let mut values: BTreeMap<String, String> = std::env::vars().collect();
        match fs::read_to_string(path) {
            Ok(contents) => values.extend(parse_dotenv(&contents)?),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(ConfigError::DotenvIo(error.to_string())),
        }
        Self::from_map(&values)
    }

    pub fn from_map(values: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let (values, legacy_aliases_used) = canonicalize(values)?;
        Ok(Self {
            node: NodeIdentityConfig {
                id: required(&values, "ASEMAN_NODE_ID")?,
                private_key_secret: required(&values, "ASEMAN_NODE_PRIVATE_KEY_SECRET")?,
                origin: optional(&values, "ASEMAN_LEGACY_ORIGIN"),
            },
            network: NetworkConfig {
                public_http_port: parse_or(&values, "ASEMAN_PUBLIC_HTTP_PORT", 8080)?,
                public_storage_port: parse_or(&values, "ASEMAN_PUBLIC_STORAGE_PORT", 8091)?,
                docker_gateway_port: parse_or(
                    &values,
                    "ASEMAN_LEGACY_DOCKER_HOST_GATEWAY_PORT",
                    8079,
                )?,
                vm_http_ingress_port: parse_or(
                    &values,
                    "ASEMAN_LEGACY_VM_HTTP_INGRESS_PORT",
                    8090,
                )?,
                legacy_tcp_port: parse_or(&values, "ASEMAN_LEGACY_TCP_PORT", 0)?,
                legacy_ws_port: parse_or(&values, "ASEMAN_LEGACY_WS_PORT", 0)?,
                legacy_federation_port: parse_or(&values, "ASEMAN_LEGACY_FEDERATION_PORT", 0)?,
                legacy_consensus_port: parse_or(&values, "ASEMAN_LEGACY_CONSENSUS_PORT", 0)?,
            },
            storage: LegacyStorageConfig {
                root_path: optional(&values, "ASEMAN_LEGACY_STORAGE_ROOT_PATH"),
                base_db_path: optional(&values, "ASEMAN_LEGACY_BASE_DB_PATH"),
                applet_db_path: optional(&values, "ASEMAN_LEGACY_APPLET_DB_PATH"),
                store_logs_db: optional(&values, "ASEMAN_LEGACY_STORE_LOGS_DB"),
                search_index_path: optional(&values, "ASEMAN_LEGACY_SEARCH_INDEX_PATH"),
            },
            allocator: AllocatorConfig {
                arena_max: parse_or(&values, "ASEMAN_MALLOC_ARENA_MAX", 2)?,
                trim_interval_seconds: parse_or(&values, "ASEMAN_MALLOC_TRIM_SECS", 30)?,
            },
            telemetry: TelemetryConfig {
                database_path: optional(&values, "ASEMAN_LEGACY_TELEMETRY_DB_PATH"),
                api_port: parse_or(&values, "ASEMAN_LEGACY_TELEMETRY_API_PORT", 9099)?,
                pprof_port: parse_or(&values, "ASEMAN_LEGACY_PPROF_PORT", 9999)?,
                entity_port: parse_or(&values, "ASEMAN_LEGACY_ENTITY_API_PORT", 0)?,
                vm_port: parse_or(&values, "ASEMAN_LEGACY_VM_API_PORT", 0)?,
            },
            cluster: ClusterBootstrapConfig {
                config_path: nonempty(&values, "ASEMAN_LEGACY_CLUSTER_CONFIG_PATH"),
                enabled: legacy_bool(&values, "ASEMAN_LEGACY_CLUSTER_ENABLED"),
                bootstrap: legacy_bool(&values, "ASEMAN_LEGACY_CLUSTER_BOOTSTRAP"),
                node_id: parse_optional(&values, "ASEMAN_LEGACY_CLUSTER_NODE_ID")?,
                node_name: nonempty(&values, "ASEMAN_LEGACY_CLUSTER_NODE_NAME"),
                region: nonempty(&values, "ASEMAN_LEGACY_CLUSTER_REGION"),
                listen_addr: nonempty(&values, "ASEMAN_LEGACY_CLUSTER_LISTEN_ADDR"),
                advertise_addr: nonempty(&values, "ASEMAN_LEGACY_CLUSTER_ADVERTISE_ADDR"),
                auth_token: nonempty(&values, "ASEMAN_LEGACY_CLUSTER_AUTH_TOKEN"),
            },
            core: CoreConfig {
                root_node: nonempty(&values, "ASEMAN_LEGACY_ROOT_NODE"),
                tls_certificate_path: nonempty(&values, "ASEMAN_LEGACY_TLS_CERT_PATH"),
                tls_private_key_path: nonempty(&values, "ASEMAN_LEGACY_TLS_KEY_PATH"),
                execution_cost_per_second: parse_or(
                    &values,
                    "ASEMAN_LEGACY_VM_EXEC_COST_PER_SECOND",
                    0_i64,
                )?
                .max(0),
                ram_cost_per_mb_minute: parse_or(
                    &values,
                    "ASEMAN_LEGACY_VM_RAM_COST_PER_MB_PER_MINUTE",
                    0_i64,
                )?
                .max(0),
                cpu_core_cost_per_minute: parse_or(
                    &values,
                    "ASEMAN_LEGACY_VM_CPU_CORE_COST_PER_MINUTE",
                    0_i64,
                )?
                .max(0),
                disk_cost_per_gb_minute: parse_or(
                    &values,
                    "ASEMAN_LEGACY_VM_DISK_COST_PER_GB_PER_MINUTE",
                    0_i64,
                )?
                .max(0),
            },
            rate_limit: RateLimitConfig {
                enabled: rate_limit_enabled(&values),
                authenticated_rps: parse_or(&values, "ASEMAN_LEGACY_RATE_LIMIT_AUTH_RPS", 50.0)?,
                authenticated_burst: parse_or(
                    &values,
                    "ASEMAN_LEGACY_RATE_LIMIT_AUTH_BURST",
                    100.0,
                )?,
                anonymous_rps: parse_or(&values, "ASEMAN_LEGACY_RATE_LIMIT_ANON_RPS", 10.0)?,
                anonymous_burst: parse_or(&values, "ASEMAN_LEGACY_RATE_LIMIT_ANON_BURST", 20.0)?,
                global_rps: parse_or(&values, "ASEMAN_LEGACY_RATE_LIMIT_GLOBAL_RPS", 5000.0)?,
                global_burst: parse_or(&values, "ASEMAN_LEGACY_RATE_LIMIT_GLOBAL_BURST", 10000.0)?,
                idle_evict_seconds: parse_or(
                    &values,
                    "ASEMAN_LEGACY_RATE_LIMIT_IDLE_EVICT_SECS",
                    300.0,
                )?,
            },
            legacy_adapters: LegacyAdapterConfig {
                main_port: optional(&values, "ASEMAN_LEGACY_MAIN_PORT"),
                login_grant_required: values
                    .get("ASEMAN_LOGIN_MODE")
                    .map(|value| value.trim().eq_ignore_ascii_case("grant"))
                    .unwrap_or(false),
                public_storage_max_bytes: parse_or(
                    &values,
                    "ASEMAN_STORAGE_MAX_BYTES",
                    10 * 1024 * 1024,
                )?,
                questdb_port: parse_or(&values, "ASEMAN_LEGACY_QUESTDB_PORT", 8812)?,
                rocksdb_max_open_files: parse_or(&values, "ASEMAN_ROCKSDB_MAX_OPEN_FILES", 512)?,
                rocksdb_block_cache_mb: parse_or(&values, "ASEMAN_ROCKSDB_BLOCK_CACHE_MB", 128)?,
                rocksdb_write_buffer_mb: parse_or(&values, "ASEMAN_ROCKSDB_WRITE_BUFFER_MB", 32)?,
                babble_data_dir: nonempty(&values, "ASEMAN_LEGACY_BABBLE_DATA_DIR"),
                babble_frame_cache: parse_or(&values, "ASEMAN_BABBLE_FRAME_CACHE", 25)?,
                babble_frame_retention: parse_or(&values, "ASEMAN_BABBLE_FRAME_RETENTION", 25)?,
                is_head: values
                    .get("ASEMAN_LEGACY_IS_HEAD")
                    .map(|value| value == "true")
                    .unwrap_or(false),
                shardchain_script: values
                    .get("ASEMAN_LEGACY_SHARDCHAIN_SCRIPT")
                    .cloned()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "/app/scripts/shardchain.sh".to_owned()),
                blockchain_api_port: parse_or(&values, "ASEMAN_LEGACY_CONSENSUS_PORT", 1337)?,
                ip_address: optional(&values, "ASEMAN_LEGACY_IPADDR"),
                home_dir: nonempty(&values, "ASEMAN_LEGACY_HOME"),
                user_profile_dir: nonempty(&values, "ASEMAN_LEGACY_USERPROFILE"),
            },
            runtime: RuntimeConfig::from_canonical(&values)?,
            vmm_endpoint: values
                .get("ASEMAN_VMM_ENDPOINT")
                .cloned()
                .unwrap_or_else(|| "http://127.0.0.1:8081".to_owned()),
            database_url_secret: values.get("ASEMAN_DATABASE_URL_SECRET").cloned(),
            legacy_aliases_used,
        })
    }
}

fn required(values: &BTreeMap<String, String>, key: &'static str) -> Result<String, ConfigError> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(ConfigError::Missing(key))
}

fn optional(values: &BTreeMap<String, String>, key: &str) -> String {
    values.get(key).cloned().unwrap_or_default()
}

fn value_or(values: &BTreeMap<String, String>, key: &str, default: &str) -> String {
    nonempty(values, key).unwrap_or_else(|| default.to_owned())
}

fn falsey(value: Option<&String>) -> bool {
    value
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(false)
}

fn nonempty(values: &BTreeMap<String, String>, key: &str) -> Option<String> {
    values
        .get(key)
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

fn legacy_bool(values: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    values
        .get(key)
        .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
}

fn rate_limit_enabled(values: &BTreeMap<String, String>) -> bool {
    values
        .get("ASEMAN_LEGACY_RATE_LIMIT_ENABLED")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

fn parse_optional<T>(
    values: &BTreeMap<String, String>,
    key: &'static str,
) -> Result<Option<T>, ConfigError>
where
    T: std::str::FromStr,
{
    values
        .get(key)
        .map(|value| {
            value.parse().map_err(|_| ConfigError::Invalid {
                key,
                reason: "value has the wrong type or range",
            })
        })
        .transpose()
}

fn parse_or<T>(
    values: &BTreeMap<String, String>,
    key: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    match values.get(key) {
        Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
            key,
            reason: "value has the wrong type or range",
        }),
        None => Ok(default),
    }
}

/// Parse the subset of dotenv syntax historically accepted by the node, including
/// multiline double-quoted PEM values.
pub fn parse_dotenv(contents: &str) -> Result<BTreeMap<String, String>, ConfigError> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut output = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line_number = index + 1;
        let line = lines[index].trim();
        index += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line
            .split_once('=')
            .ok_or(ConfigError::InvalidDotenv { line: line_number })?;
        let key = key.trim();
        if key.is_empty() {
            return Err(ConfigError::InvalidDotenv { line: line_number });
        }
        let raw = raw.trim();
        let value = if raw.starts_with('"') && !raw[1..].contains('"') {
            let mut value = raw[1..].to_owned();
            let mut closed = false;
            while index < lines.len() {
                let next = lines[index];
                index += 1;
                value.push('\n');
                value.push_str(next);
                if next.ends_with('"') {
                    value.pop();
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err(ConfigError::InvalidDotenv { line: line_number });
            }
            value
        } else {
            raw.trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .to_owned()
        };
        output.insert(key.to_owned(), value);
    }
    Ok(output)
}

/// Translate the complete generated A003 legacy-key catalog to canonical names.
/// Canonical/legacy collisions fail even when their values match.
pub fn canonicalize(
    values: &BTreeMap<String, String>,
) -> Result<(BTreeMap<String, String>, Vec<String>), ConfigError> {
    let catalog: AliasCatalog =
        serde_json::from_str(LEGACY_ALIASES_JSON).map_err(|_| ConfigError::InvalidAliasCatalog)?;
    let mut output = values.clone();
    let mut used = Vec::new();
    for row in catalog.aliases {
        if row.legacy == row.canonical {
            continue;
        }
        match (values.get(&row.canonical), values.get(&row.legacy)) {
            (Some(_), Some(_)) => {
                return Err(ConfigError::AliasConflict {
                    canonical: row.canonical,
                    legacy: row.legacy,
                });
            }
            (None, Some(value)) => {
                output.insert(row.canonical, value.clone());
                used.push(row.legacy);
            }
            _ => {}
        }
    }
    used.sort();
    Ok((output, used))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ASEMAN_NODE_ID".into(), "node-1".into()),
            (
                "ASEMAN_NODE_PRIVATE_KEY_SECRET".into(),
                "secret://node/private-key".into(),
            ),
        ])
    }

    #[test]
    fn canonical_defaults_are_typed() {
        let config = AsemanConfig::from_map(&base()).unwrap();
        assert_eq!(config.network.public_http_port, 8080);
        assert_eq!(config.network.docker_gateway_port, 8079);
        assert_eq!(config.allocator.trim_interval_seconds, 30);
        assert!(config.legacy_aliases_used.is_empty());
    }

    #[test]
    fn conflicting_aliases_fail_closed() {
        let mut values = base();
        values.insert("OWNER_ID".into(), "legacy".into());
        assert!(matches!(
            AsemanConfig::from_map(&values),
            Err(ConfigError::AliasConflict { .. })
        ));
    }

    #[test]
    fn generated_catalog_translates_non_core_legacy_keys() {
        let values = BTreeMap::from([("CASPAR_STORAGE_PORT".into(), "8091".into())]);
        let (canonical, used) = canonicalize(&values).unwrap();
        assert_eq!(canonical["ASEMAN_PUBLIC_STORAGE_PORT"], "8091");
        assert_eq!(used, vec!["CASPAR_STORAGE_PORT"]);
    }

    #[test]
    fn legacy_node_map_preserves_ports_paths_and_allocator_defaults() {
        let values = BTreeMap::from([
            ("OWNER_ID".into(), "legacy-node".into()),
            ("OWNER_PRIVATE_KEY".into(), "legacy-pem".into()),
            ("CLIENT_TCP_API_PORT".into(), "7001".into()),
            ("STORAGE_ROOT_PATH".into(), "/srv/aseman".into()),
            ("CASPAR_MALLOC_ARENA_MAX".into(), "4".into()),
        ]);
        let config = AsemanConfig::from_map(&values).unwrap();
        assert_eq!(config.node.id, "legacy-node");
        assert_eq!(config.network.legacy_tcp_port, 7001);
        assert_eq!(config.storage.root_path, "/srv/aseman");
        assert_eq!(config.allocator.arena_max, 4);
    }

    #[test]
    fn dotenv_parser_preserves_multiline_secret_and_last_value() {
        let values =
            parse_dotenv("OWNER_ID=first\nOWNER_ID=last\nOWNER_PRIVATE_KEY=\"line-1\nline-2\"\n")
                .unwrap();
        assert_eq!(values["OWNER_ID"], "last");
        assert_eq!(values["OWNER_PRIVATE_KEY"], "line-1\nline-2");
    }

    #[test]
    fn invalid_typed_port_is_rejected() {
        let mut values = base();
        values.insert("ASEMAN_PUBLIC_STORAGE_PORT".into(), "70000".into());
        assert!(matches!(
            AsemanConfig::from_map(&values),
            Err(ConfigError::Invalid {
                key: "ASEMAN_PUBLIC_STORAGE_PORT",
                ..
            })
        ));
    }

    #[test]
    fn module_json_and_secret_files_are_bounded_and_typed() {
        #[derive(Debug, Deserialize, Eq, PartialEq)]
        #[serde(deny_unknown_fields)]
        struct Example {
            enabled: bool,
        }

        let root = std::env::temp_dir().join(format!(
            "aseman-config-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        fs::create_dir_all(&root).unwrap();
        let json = root.join("configuration.json");
        let secret = root.join("secret");
        fs::write(&json, br#"{"enabled":true}"#).unwrap();
        fs::write(&secret, b"postgres://opaque\n").unwrap();
        assert_eq!(
            read_json_file::<Example>(&json).unwrap(),
            Example { enabled: true }
        );
        assert_eq!(read_secret_file(&secret, 64).unwrap(), "postgres://opaque");
        assert!(read_secret_file(&secret, 4).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
