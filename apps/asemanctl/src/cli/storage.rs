//! `asemanctl storage` — the node's storage module (ADR 0036).
//!
//! `storage migrate` converts a store from before ADR 0036 into models and copies
//! every model and consensus log into another storage provider. It runs while the
//! node is stopped: for a host node in this process, over the node's configuration;
//! for the compact deployment inside a one-off node container
//! (`aseman-node storage migrate`), where the node's volumes and secrets are, so
//! secret paths name files inside the container.

use anyhow::{Result, anyhow, bail};

use super::args;
use super::compact::Compact;
use aseman_config::CliConfig;

/// Flags `asemanctl` itself reads; the rest pass through to the migration.
const OWN_FLAGS: [&str; 4] = ["config-dir", "compose-file", "state-dir", "host"];

pub fn run_storage(config: &CliConfig, arguments: &[String]) -> Result<()> {
    match arguments.first().map(String::as_str) {
        Some("migrate") => migrate(config, &arguments[1..]),
        None | Some("help" | "-h" | "--help") => {
            print_usage();
            Ok(())
        }
        Some(other) => {
            print_usage();
            bail!("unknown storage subcommand \"{other}\"")
        }
    }
}

fn print_usage() {
    println!(
        "asemanctl storage - the node's storage module (ADR 0036)\n\n\
         Subcommands:\n  \
         migrate   Convert a pre-ADR-0036 store into models, or copy every model and\n            \
         consensus log into another provider (--to rocksdb|postgres)\n\n\
         Flags: [--to PROVIDER] [--database-url-secret FILE] [--shards-secret FILE]\n       \
         [--file-artifact ID=PATH]... [--dry-run] [--host]\n\n\
         Run it while the node is stopped. --host migrates the host node even when a\n\
         compact deployment exists."
    );
}

/// The arguments the migration reads (without `asemanctl`'s own flags).
fn passed_through(arguments: &[String]) -> Vec<String> {
    let mut passed = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let own = OWN_FLAGS.iter().find(|flag| {
            argument == &format!("--{flag}") || argument.starts_with(&format!("--{flag}="))
        });
        match own {
            // `--host` is bare; the others take a value unless given as `--flag=value`.
            Some(&"host") => {}
            Some(_) if !argument.contains('=') => index += 1,
            Some(_) => {}
            None => passed.push(argument.clone()),
        }
        index += 1;
    }
    passed
}

fn migrate(cli: &CliConfig, arguments: &[String]) -> Result<()> {
    let migration = passed_through(arguments);
    let compact = (!args::has_flag(arguments, "host"))
        .then(|| Compact::locate(arguments, cli))
        .flatten();
    let Some(compact) = compact else {
        let config = aseman_config::AsemanConfig::from_process_with_dotenv("")
            .map_err(|error| anyhow!("node configuration could not be loaded: {error}"))?;
        let report = aseman_storage_providers::migrate::command(&config, &migration)
            .map_err(|error| anyhow!("{error}"))?;
        print!("{report}");
        return Ok(());
    };
    if compact.node_healthy() && !args::has_flag(arguments, "dry-run") {
        bail!("the node is running; stop it first (`asemanctl stop node`)");
    }
    let mut command = vec![
        "run",
        "--rm",
        "--no-deps",
        "--entrypoint",
        "/usr/local/bin/aseman-node",
        "node",
        "storage",
        "migrate",
    ];
    command.extend(migration.iter().map(String::as_str));
    let output = compact.compose(&command)?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn only_the_migration_flags_pass_through() {
        assert_eq!(
            passed_through(&strings(&[
                "--to",
                "postgres",
                "--config-dir",
                "/c",
                "--host",
                "--state-dir=/s",
                "--file-artifact",
                "f1=/x",
                "--dry-run",
            ])),
            strings(&["--to", "postgres", "--file-artifact", "f1=/x", "--dry-run"])
        );
    }
}
