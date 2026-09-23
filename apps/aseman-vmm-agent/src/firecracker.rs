//! Driving one Firecracker microVM through its API socket (A603, ADR 0010).
//!
//! This is the only code in Aseman that talks to Firecracker. Everything it writes is
//! derived from the administrator's profile and the agent's own allocation root; no
//! part of a request reaches Firecracker unchecked.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aseman_domain::agent::{MachineProfile, MachineState};

/// Whether this host can run microVMs at all.
#[must_use]
pub fn kvm_available() -> bool {
    Path::new("/dev/kvm").exists()
}

/// A failure, in words an operator can act on. Firecracker's own fault message is
/// kept: "cannot start without kernel configuration" is worth reading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirecrackerError(pub String);

impl std::fmt::Display for FirecrackerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FirecrackerError {}

type Result<T> = std::result::Result<T, FirecrackerError>;

fn failed(error: impl std::fmt::Display) -> FirecrackerError {
    FirecrackerError(error.to_string())
}

/// One microVM: its process, its API socket, and the directory it owns.
pub struct Machine {
    pub directory: PathBuf,
    socket: PathBuf,
    process: Child,
    state: MachineState,
}

impl Machine {
    /// Start a Firecracker process for `directory` and configure it from `profile`.
    ///
    /// The machine is configured, not booted: [`Self::start`] boots it.
    ///
    /// # Errors
    ///
    /// When the process cannot start or Firecracker refuses the configuration.
    pub fn create(binary: &Path, directory: PathBuf, profile: &MachineProfile) -> Result<Self> {
        std::fs::create_dir_all(&directory).map_err(failed)?;
        let socket = directory.join("firecracker.sock");
        // A stale socket from a previous life would be bound by nothing.
        let _ = std::fs::remove_file(&socket);
        let process = Command::new(binary)
            .arg("--api-sock")
            .arg(&socket)
            .current_dir(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| failed(format!("firecracker did not start: {error}")))?;
        let mut machine = Self {
            directory,
            socket,
            process,
            state: MachineState::Created,
        };
        machine.await_socket(Duration::from_secs(10))?;
        machine.configure(profile)?;
        Ok(machine)
    }

    fn await_socket(&mut self, within: Duration) -> Result<()> {
        let deadline = Instant::now() + within;
        while !self.socket.exists() {
            if let Ok(Some(status)) = self.process.try_wait() {
                return Err(failed(format!("firecracker exited early: {status}")));
            }
            if Instant::now() >= deadline {
                return Err(failed("firecracker did not open its API socket"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Write the profile's machine configuration and boot source.
    fn configure(&mut self, profile: &MachineProfile) -> Result<()> {
        self.put(
            "/machine-config",
            &serde_json::json!({
                "vcpu_count": profile.vcpu_count,
                "mem_size_mib": profile.memory_mib,
            }),
        )?;
        // Only a profile's own images are ever attached. A caller cannot name a path.
        if profile.kernel_image.as_os_str().is_empty() {
            return Ok(());
        }
        self.put(
            "/boot-source",
            &serde_json::json!({
                "kernel_image_path": profile.kernel_image,
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off",
            }),
        )?;
        if !profile.root_image.as_os_str().is_empty() {
            self.put(
                "/drives/rootfs",
                &serde_json::json!({
                    "drive_id": "rootfs",
                    "path_on_host": profile.root_image,
                    "is_root_device": true,
                    "is_read_only": true,
                }),
            )?;
        }
        Ok(())
    }

    /// Boot the machine.
    ///
    /// # Errors
    ///
    /// Firecracker's own reason, unchanged. A host without KVM, or a profile with no
    /// kernel, fails here and says why — it is never reported as running.
    pub fn start(&mut self) -> Result<()> {
        self.action("InstanceStart")?;
        self.state = MachineState::Running;
        Ok(())
    }

    /// The runtime's own pause, not a stop.
    ///
    /// # Errors
    ///
    /// Firecracker's own reason.
    pub fn pause(&mut self) -> Result<()> {
        self.patch("/vm", &serde_json::json!({"state": "Paused"}))?;
        self.state = MachineState::Paused;
        Ok(())
    }

    /// Resume a paused machine.
    ///
    /// # Errors
    ///
    /// Firecracker's own reason.
    pub fn resume(&mut self) -> Result<()> {
        self.patch("/vm", &serde_json::json!({"state": "Resumed"}))?;
        self.state = MachineState::Running;
        Ok(())
    }

    /// What the machine is doing, checked against the process rather than assumed.
    pub fn state(&mut self) -> MachineState {
        match self.process.try_wait() {
            // The process is gone. Stopped is what an operator asked for; anything
            // else is a failure, and is reported as one.
            Ok(Some(_)) if self.state == MachineState::Stopped => MachineState::Stopped,
            Ok(Some(_)) => MachineState::Failed,
            _ => self.state,
        }
    }

    /// Stop the machine. Idempotent.
    pub fn stop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_file(&self.socket);
        self.state = MachineState::Stopped;
    }

    /// Stop it and remove everything it owned.
    pub fn delete(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.directory);
    }

    fn put(&self, path: &str, body: &serde_json::Value) -> Result<()> {
        self.request("PUT", path, Some(body)).map(|_| ())
    }

    fn patch(&self, path: &str, body: &serde_json::Value) -> Result<()> {
        self.request("PATCH", path, Some(body)).map(|_| ())
    }

    fn action(&self, action: &str) -> Result<()> {
        self.request(
            "PUT",
            "/actions",
            Some(&serde_json::json!({"action_type": action})),
        )
        .map(|_| ())
    }

    /// Firecracker's API is HTTP over a Unix socket. One request, one connection.
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|error| failed(format!("the firecracker API socket is gone: {error}")))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(failed)?;
        let payload = body.map(ToString::to_string).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        stream.write_all(request.as_bytes()).map_err(failed)?;
        stream.flush().map_err(failed)?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).map_err(failed)?;
        let code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| failed("the firecracker API answered nothing"))?;
        // Skip the headers; the body is short and the connection closes after it.
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).map_err(failed)? == 0 || header.trim().is_empty() {
                break;
            }
        }
        let mut answer = String::new();
        let _ = reader.read_to_string(&mut answer);
        if (200..300).contains(&code) {
            return Ok(answer);
        }
        // Firecracker's fault message is the useful part; keep it verbatim.
        let reason = serde_json::from_str::<serde_json::Value>(&answer)
            .ok()
            .and_then(|value| value["fault_message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| answer.trim().to_owned());
        Err(failed(format!("firecracker refused {path}: {reason}")))
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        // A dropped machine must not leave a Firecracker process behind.
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
