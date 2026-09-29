//! Canonical Aseman administrative CLI.
//!
//! * `bootstrap` provisions (or resumes) the compact deployment; `status`, `start`, and
//!   `stop` operate the deployment it created (`compact`).
//! * `doctor`, `backup`, `restore`, `upgrade`, and `support-bundle` drive the resumable
//!   A902 journals (`ops`).
//! * `module` manages signed provider modules; `vms` manages the native VMM backend's
//!   compiled-in runtime plugins.
//!
//! Every command accepts `--json` for one A903 result/error envelope.

use std::process::Command;

use serde_json::{Value, json};

mod args;
mod bootstrap;
mod cluster;
mod compact;
mod modules;
mod ops;
mod storage;
mod vms;

pub fn main() {
    if let Err(error) = aseman_config::install_cli_process_config() {
        eprintln!("invalid Aseman CLI configuration: {error}");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().collect();
    if !aseman_config::cli_config().is_some_and(|config| config.structured_child)
        && args.iter().skip(1).any(|argument| argument == "--json")
    {
        run_structured(&args);
    }
    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }
    let rest = &args[2..];
    let result = match args[1].as_str() {
        "bootstrap" => bootstrap::run_bootstrap(rest),
        "status" => compact::run_status(rest),
        "start" => compact::run_start(rest),
        "stop" => compact::run_stop(rest),
        "module" | "modules" => modules::run_modules(rest),
        "vms" => vms::run_vms(rest),
        "cluster" => cluster::run_cluster(rest),
        "storage" => storage::run_storage(rest),
        "doctor" => ops::run_doctor(rest),
        "backup" => ops::run_backup(rest),
        "restore" => ops::run_restore(rest),
        "upgrade" => ops::run_upgrade(rest),
        "support-bundle" => ops::run_support_bundle(rest),
        "help" | "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown command \"{other}\"\n");
            print_usage();
            std::process::exit(1);
        }
    };
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

/// Run any command as a child and turn its complete result into exactly one JSON
/// document. This keeps the compatibility implementations free to print their human
/// progress while giving automation one stable A903 envelope. The child marker is
/// parsed by `aseman-config`, so this module performs no direct environment read.
fn run_structured(args: &[String]) -> ! {
    let command = args
        .iter()
        .skip(1)
        .find(|argument| argument.as_str() != "--json")
        .cloned()
        .unwrap_or_else(|| "help".to_owned());
    let preserve_native_json = command == "doctor";
    let mut child_args = args
        .iter()
        .skip(1)
        .filter(|argument| argument.as_str() != "--json")
        .cloned()
        .collect::<Vec<_>>();
    if preserve_native_json {
        child_args.push("--json".to_owned());
    }
    let output = std::env::current_exe().and_then(|executable| {
        Command::new(executable)
            .args(child_args)
            .env("ASEMAN_CLI_STRUCTURED_CHILD", "1")
            .output()
    });
    let (exit_code, stdout, stderr) = match output {
        Ok(output) => (
            output.status.code().unwrap_or(1),
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ),
        Err(error) => (1, String::new(), format!("cannot execute command: {error}")),
    };
    let mut document = structured_result(&command, exit_code, &stdout, &stderr);
    let _ = crate::redact_support_bundle(&mut document);
    println!(
        "{}",
        serde_json::to_string(&document).unwrap_or_else(|_| {
            r#"{"schema":"aseman.cli.error.v1","ok":false,"command":"unknown","error":"could not encode structured result"}"#.to_owned()
        })
    );
    std::process::exit(exit_code);
}

fn structured_result(command: &str, exit_code: i32, stdout: &str, stderr: &str) -> Value {
    let data = serde_json::from_str(stdout).unwrap_or_else(|_| Value::String(stdout.to_owned()));
    let warnings = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if exit_code == 0 {
        json!({
            "schema": "aseman.cli.result.v1",
            "ok": true,
            "command": command,
            "exit_code": exit_code,
            "data": data,
            "warnings": warnings,
        })
    } else {
        let error = if stderr.is_empty() {
            stdout.to_owned()
        } else {
            stderr.to_owned()
        };
        json!({
            "schema": "aseman.cli.error.v1",
            "ok": false,
            "command": command,
            "exit_code": exit_code,
            "error": error,
            "warnings": warnings,
        })
    }
}

fn print_usage() {
    println!(
        "asemanctl - administer an Aseman deployment\n\n\
         Usage:\n  asemanctl <command> [flags] [--json]\n\n\
         Deployment:\n  \
         bootstrap       Provision or resume the compact deployment\n  \
         status          Show every service and the node's health\n  \
         start [SVC...]  Start the deployment or the named services\n  \
         stop [SVC...]   Stop the deployment or the named services (volumes are kept)\n\n\
         Operations:\n  \
         doctor          Run the ordered health checks\n  \
         backup          Snapshot storage into a signed backup\n  \
         restore         Restore a backup onto an empty target\n  \
         upgrade         Back up, then move the deployment to new images\n  \
         support-bundle  Collect, redact, and package diagnostics\n\n\
         Extensions:\n  \
         module          Manage signed provider modules\n  \
         vms             Manage the native backend's runtime plugins\n  \
         cluster         Operate the RocksDB storage module's OpenRaft cluster\n  \
         storage         Migrate the node's storage (legacy layouts, provider switch)\n\n\
         Run \"asemanctl <command> --help\" for command-specific flags."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_success_preserves_native_json_as_data() {
        let result = structured_result("status", 0, r#"{"healthy":true}"#, "warning: old alias");
        assert_eq!(result["schema"], "aseman.cli.result.v1");
        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["healthy"], true);
        assert_eq!(result["warnings"][0], "warning: old alias");
    }

    #[test]
    fn structured_failure_uses_the_stable_error_envelope() {
        let result = structured_result("start", 1, "", "could not start");
        assert_eq!(result["schema"], "aseman.cli.error.v1");
        assert_eq!(result["ok"], false);
        assert_eq!(result["command"], "start");
        assert_eq!(result["error"], "could not start");
    }
}
