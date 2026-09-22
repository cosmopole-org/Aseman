//! Conversions between the port values and the A504 messages.

use aseman_contracts::module_control_v1::{ErrorCode, ModuleError, RequestMetadata};
use aseman_contracts::vmm_backend_v1 as wire;
use aseman_domain::vmm::{
    DeployConventions, Endpoint, LogRecord, LogStream, Observation, PortProtocol, ReconcileAction,
    RuntimeCapabilities, Usage,
};
use aseman_domain::{Generation, ObservedWorkloadState};
use aseman_ports::PortError;

/// The A504 contract version.
pub const CONTRACT: &str = "1";

#[must_use]
pub fn error(error: &PortError) -> ModuleError {
    let (code, retryable) = match error {
        PortError::NotFound => (ErrorCode::NotFound, false),
        PortError::Conflict => (ErrorCode::Conflict, false),
        PortError::Denied(_) => (ErrorCode::PermissionDenied, false),
        PortError::Unavailable(_) => (ErrorCode::Unavailable, true),
        PortError::Deadline => (ErrorCode::DeadlineExceeded, true),
        PortError::Unsupported(_) => (ErrorCode::Unsupported, false),
        PortError::Failed(_) => (ErrorCode::Internal, false),
    };
    ModuleError {
        code: code as i32,
        message: error.to_string(),
        retryable,
        request_id: String::new(),
        details: Default::default(),
    }
}

/// The port error a response's `ModuleError` stands for.
#[must_use]
pub fn port_error(error: &ModuleError) -> PortError {
    match ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified) {
        ErrorCode::NotFound => PortError::NotFound,
        ErrorCode::Conflict => PortError::Conflict,
        ErrorCode::PermissionDenied | ErrorCode::Unauthenticated => PortError::Denied("backend"),
        ErrorCode::Unavailable | ErrorCode::Cancelled => PortError::Unavailable("backend"),
        ErrorCode::DeadlineExceeded => PortError::Deadline,
        ErrorCode::Unsupported => PortError::Unsupported("backend"),
        _ => PortError::Failed(error.message.clone()),
    }
}

#[must_use]
pub fn metadata(request_id: &str, deadline_millis: i64) -> RequestMetadata {
    RequestMetadata {
        request_id: request_id.to_owned(),
        trace_id: String::new(),
        deadline_unix_millis: deadline_millis,
        cancellation_id: String::new(),
        idempotency_key: String::new(),
    }
}

#[must_use]
pub fn observed_state(state: ObservedWorkloadState) -> wire::ObservedState {
    match state {
        ObservedWorkloadState::Unknown => wire::ObservedState::Unknown,
        ObservedWorkloadState::Pending => wire::ObservedState::Pending,
        ObservedWorkloadState::Running => wire::ObservedState::Running,
        ObservedWorkloadState::Paused => wire::ObservedState::Paused,
        ObservedWorkloadState::Stopped => wire::ObservedState::Stopped,
        ObservedWorkloadState::Failed => wire::ObservedState::Failed,
        ObservedWorkloadState::Lost => wire::ObservedState::Lost,
    }
}

/// # Errors
///
/// `Failed` for an unspecified or unknown state.
pub fn domain_state(state: i32) -> Result<ObservedWorkloadState, PortError> {
    Ok(match wire::ObservedState::try_from(state) {
        Ok(wire::ObservedState::Unknown) => ObservedWorkloadState::Unknown,
        Ok(wire::ObservedState::Pending) => ObservedWorkloadState::Pending,
        Ok(wire::ObservedState::Running) => ObservedWorkloadState::Running,
        Ok(wire::ObservedState::Paused) => ObservedWorkloadState::Paused,
        Ok(wire::ObservedState::Stopped) => ObservedWorkloadState::Stopped,
        Ok(wire::ObservedState::Failed) => ObservedWorkloadState::Failed,
        Ok(wire::ObservedState::Lost) => ObservedWorkloadState::Lost,
        _ => return Err(PortError::Failed("unknown observed state".to_owned())),
    })
}

#[must_use]
pub fn observation(observation: &Observation) -> wire::Observation {
    wire::Observation {
        state: observed_state(observation.state) as i32,
        generation: observation.generation.get(),
        sequence: observation.sequence,
        reason: observation.reason.clone().unwrap_or_default(),
        observed_at_millis: observation.observed_at_millis,
    }
}

/// # Errors
///
/// `Failed` for a missing or invalid observation.
pub fn domain_observation(
    observation: Option<wire::Observation>,
) -> Result<Observation, PortError> {
    let observation = observation
        .ok_or_else(|| PortError::Failed("the backend sent no observation".to_owned()))?;
    Ok(Observation {
        state: domain_state(observation.state)?,
        generation: Generation::from_stored(observation.generation)
            .map_err(|_| PortError::Failed("invalid observation generation".to_owned()))?,
        sequence: observation.sequence,
        reason: (!observation.reason.is_empty()).then_some(observation.reason),
        observed_at_millis: observation.observed_at_millis,
    })
}

#[must_use]
pub fn action(action: ReconcileAction) -> wire::ReconcileAction {
    match action {
        ReconcileAction::Start => wire::ReconcileAction::Start,
        ReconcileAction::Stop => wire::ReconcileAction::Stop,
        ReconcileAction::Pause => wire::ReconcileAction::Pause,
        ReconcileAction::Resume => wire::ReconcileAction::Resume,
        ReconcileAction::Delete => wire::ReconcileAction::Delete,
        ReconcileAction::Restart => wire::ReconcileAction::Restart,
        ReconcileAction::None | ReconcileAction::Adopt => wire::ReconcileAction::Unspecified,
    }
}

/// # Errors
///
/// `Failed` for an unspecified or unknown action: a backend never adopts or idles on
/// request.
pub fn domain_action(action: i32) -> Result<ReconcileAction, PortError> {
    Ok(match wire::ReconcileAction::try_from(action) {
        Ok(wire::ReconcileAction::Start) => ReconcileAction::Start,
        Ok(wire::ReconcileAction::Stop) => ReconcileAction::Stop,
        Ok(wire::ReconcileAction::Pause) => ReconcileAction::Pause,
        Ok(wire::ReconcileAction::Resume) => ReconcileAction::Resume,
        Ok(wire::ReconcileAction::Delete) => ReconcileAction::Delete,
        Ok(wire::ReconcileAction::Restart) => ReconcileAction::Restart,
        _ => return Err(PortError::Failed("unknown reconcile action".to_owned())),
    })
}

#[must_use]
pub fn capabilities(runtime: &RuntimeCapabilities) -> wire::RuntimeCapabilities {
    wire::RuntimeCapabilities {
        runtime: runtime.runtime.clone(),
        invocation: runtime.invocation,
        long_running: runtime.long_running,
        pause: runtime.pause,
        snapshot: runtime.snapshot,
        exec: runtime.exec,
        terminal: runtime.terminal,
        http_ingress: runtime.http_ingress,
        files: runtime.files,
        build: runtime.build,
        chain_transactions: runtime.chain_transactions,
        execution_proofs: runtime.execution_proofs,
        deploy: Some(wire::DeployConventions {
            entity_file_name: runtime.deploy.entity_file_name.clone(),
            accepts_extra_files: runtime.deploy.accepts_extra_files,
            build_on_deploy: runtime.deploy.build_on_deploy,
        }),
    }
}

#[must_use]
pub fn domain_capabilities(runtime: wire::RuntimeCapabilities) -> RuntimeCapabilities {
    let deploy = runtime.deploy.unwrap_or_default();
    RuntimeCapabilities {
        runtime: runtime.runtime,
        invocation: runtime.invocation,
        long_running: runtime.long_running,
        pause: runtime.pause,
        snapshot: runtime.snapshot,
        exec: runtime.exec,
        terminal: runtime.terminal,
        http_ingress: runtime.http_ingress,
        files: runtime.files,
        build: runtime.build,
        chain_transactions: runtime.chain_transactions,
        execution_proofs: runtime.execution_proofs,
        deploy: DeployConventions {
            entity_file_name: deploy.entity_file_name,
            accepts_extra_files: deploy.accepts_extra_files,
            build_on_deploy: deploy.build_on_deploy,
        },
    }
}

fn protocol_text(protocol: PortProtocol) -> &'static str {
    match protocol {
        PortProtocol::Http => "http",
        PortProtocol::Tcp => "tcp",
    }
}

#[must_use]
pub fn endpoint(endpoint: &Endpoint) -> wire::Endpoint {
    wire::Endpoint {
        name: endpoint.name.clone(),
        protocol: protocol_text(endpoint.protocol).to_owned(),
        address: endpoint.address.clone(),
        port: u32::from(endpoint.port),
    }
}

/// # Errors
///
/// `Failed` for an unknown protocol or port.
pub fn domain_endpoint(endpoint: wire::Endpoint) -> Result<Endpoint, PortError> {
    Ok(Endpoint {
        name: endpoint.name,
        protocol: match endpoint.protocol.as_str() {
            "http" => PortProtocol::Http,
            "tcp" => PortProtocol::Tcp,
            _ => return Err(PortError::Failed("unknown endpoint protocol".to_owned())),
        },
        address: endpoint.address,
        port: u16::try_from(endpoint.port)
            .map_err(|_| PortError::Failed("invalid endpoint port".to_owned()))?,
    })
}

#[must_use]
pub fn usage(usage: &Usage) -> wire::Usage {
    wire::Usage {
        window_start_millis: usage.window_start_millis,
        window_end_millis: usage.window_end_millis,
        sequence: usage.sequence,
        cpu_millis: usage.cpu_millis,
        memory_peak_bytes: usage.memory_peak_bytes,
        network_rx_bytes: usage.network_rx_bytes,
        network_tx_bytes: usage.network_tx_bytes,
        storage_bytes: usage.storage_bytes,
        invocations: usage.invocations,
    }
}

#[must_use]
pub fn domain_usage(usage: wire::Usage) -> Usage {
    Usage {
        window_start_millis: usage.window_start_millis,
        window_end_millis: usage.window_end_millis,
        sequence: usage.sequence,
        cpu_millis: usage.cpu_millis,
        memory_peak_bytes: usage.memory_peak_bytes,
        network_rx_bytes: usage.network_rx_bytes,
        network_tx_bytes: usage.network_tx_bytes,
        storage_bytes: usage.storage_bytes,
        invocations: usage.invocations,
    }
}

fn stream_text(stream: LogStream) -> &'static str {
    match stream {
        LogStream::Stdout => "stdout",
        LogStream::Stderr => "stderr",
        LogStream::System => "system",
        LogStream::Build => "build",
    }
}

#[must_use]
pub fn log(record: &LogRecord) -> wire::LogRecord {
    wire::LogRecord {
        sequence: record.sequence,
        at_millis: record.at_millis,
        stream: stream_text(record.stream).to_owned(),
        line: record.line.clone(),
    }
}

/// # Errors
///
/// `Failed` for an unknown stream.
pub fn domain_log(record: wire::LogRecord) -> Result<LogRecord, PortError> {
    Ok(LogRecord {
        sequence: record.sequence,
        at_millis: record.at_millis,
        stream: match record.stream.as_str() {
            "stdout" => LogStream::Stdout,
            "stderr" => LogStream::Stderr,
            "system" => LogStream::System,
            "build" => LogStream::Build,
            _ => return Err(PortError::Failed("unknown log stream".to_owned())),
        },
        line: record.line,
    })
}
