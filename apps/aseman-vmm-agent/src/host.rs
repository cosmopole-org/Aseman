//! The agent's machines, and the checks every request passes before one is touched.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use aseman_domain::agent::{
    AgentError, AgentOperation, Grant, MachineProfile, MachineState, allocation_directory,
    authorize,
};

use crate::firecracker::{Machine, kvm_available};

/// What an administrator declared on this host.
pub struct HostConfig {
    /// Where allocations' directories live. Every path the agent writes is under it.
    pub root: PathBuf,
    /// The Firecracker binary.
    pub firecracker: PathBuf,
    /// The machine profiles a grant may name.
    pub profiles: BTreeMap<String, MachineProfile>,
    /// Whether this host offers Firecracker at all. An operator can turn it off
    /// independently of everything else the agent does.
    pub firecracker_enabled: bool,
}

/// The agent's live machines.
pub struct Agent {
    config: HostConfig,
    machines: Mutex<BTreeMap<String, Machine>>,
}

/// Why a request was refused, or what it produced.
#[derive(Debug)]
pub enum Refusal {
    /// A rule in the domain said no.
    Rule(AgentError),
    /// The host said no: Firecracker's own words.
    Host(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rule(error) => write!(formatter, "{error}"),
            Self::Host(reason) => formatter.write_str(reason),
        }
    }
}

impl From<AgentError> for Refusal {
    fn from(error: AgentError) -> Self {
        Self::Rule(error)
    }
}

type Answer<T> = Result<T, Refusal>;

impl Agent {
    #[must_use]
    pub fn new(config: HostConfig) -> Self {
        Self {
            config,
            machines: Mutex::new(BTreeMap::new()),
        }
    }

    /// Whether this host can run microVMs right now, and why not when it cannot.
    ///
    /// # Errors
    ///
    /// [`AgentError::Disabled`] or [`AgentError::NoKvm`].
    pub fn capability(&self) -> Result<(), AgentError> {
        if !self.config.firecracker_enabled {
            return Err(AgentError::Disabled);
        }
        if !kvm_available() {
            return Err(AgentError::NoKvm);
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Machine>> {
        self.machines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Prepare a microVM for `allocation`, from the profile the grant names.
    ///
    /// Repeating this for an allocation that already has a machine is not an error and
    /// does not create a second one.
    ///
    /// # Errors
    ///
    /// When the grant does not cover it, the profile is unknown, the host cannot run
    /// microVMs, or Firecracker refuses the configuration.
    pub fn create(&self, grant: &Grant, allocation: &str, now_millis: i64) -> Answer<MachineState> {
        authorize(grant, allocation, AgentOperation::Create, now_millis)?;
        // The capability is checked before anything is written: a host that cannot
        // run a microVM must refuse the work, not accept it and fail later.
        self.capability()?;
        let profile = self
            .config
            .profiles
            .get(&grant.profile)
            .ok_or(AgentError::UnknownProfile)?
            .clone();
        let directory = allocation_directory(&self.config.root, allocation)?;
        let mut machines = self.lock();
        if let Some(machine) = machines.get_mut(allocation) {
            return Ok(machine.state());
        }
        let machine = Machine::create(&self.config.firecracker, directory, &profile)
            .map_err(|error| Refusal::Host(error.to_string()))?;
        machines.insert(allocation.to_owned(), machine);
        Ok(MachineState::Created)
    }

    /// Run one lifecycle operation on an allocation's machine.
    ///
    /// # Errors
    ///
    /// When the grant does not cover it, the allocation has no machine, the machine is
    /// in a state that does not allow it, or the host refuses.
    pub fn operate(
        &self,
        grant: &Grant,
        allocation: &str,
        operation: AgentOperation,
        now_millis: i64,
    ) -> Answer<MachineState> {
        authorize(grant, allocation, operation, now_millis)?;
        let mut machines = self.lock();
        let Some(machine) = machines.get_mut(allocation) else {
            // Deletion is explicitly idempotent at the A603 boundary. A retry after
            // the first delete must not turn success into an ambiguous failure.
            return if operation == AgentOperation::Delete {
                Ok(MachineState::Stopped)
            } else {
                Err(AgentError::UnknownAllocation.into())
            };
        };
        let state = machine.state();
        if matches!(
            (operation, state),
            (AgentOperation::Start, MachineState::Running)
                | (AgentOperation::Pause, MachineState::Paused)
                | (AgentOperation::Resume, MachineState::Running)
                | (AgentOperation::Stop, MachineState::Stopped)
        ) {
            return Ok(state);
        }
        if !state.allows(operation) {
            // A pause that is not a pause would be worse than a refusal (A604).
            return Err(AgentError::WrongState.into());
        }
        let host = |result: Result<(), crate::firecracker::FirecrackerError>| {
            result.map_err(|error| Refusal::Host(error.to_string()))
        };
        match operation {
            AgentOperation::State => Ok(state),
            AgentOperation::Start => {
                host(machine.start())?;
                Ok(machine.state())
            }
            AgentOperation::Pause => {
                host(machine.pause())?;
                Ok(machine.state())
            }
            AgentOperation::Resume => {
                host(machine.resume())?;
                Ok(machine.state())
            }
            AgentOperation::Stop => {
                machine.stop();
                Ok(MachineState::Stopped)
            }
            AgentOperation::Delete => {
                machine.delete();
                machines.remove(allocation);
                Ok(MachineState::Stopped)
            }
            AgentOperation::Create => Err(AgentError::WrongState.into()),
        }
    }

    /// Whether this allocation has a machine.
    #[must_use]
    pub fn holds(&self, allocation: &str) -> bool {
        self.lock().contains_key(allocation)
    }
}
