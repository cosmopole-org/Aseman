//! Administrative client for the authenticated module supervisor API.

use std::collections::HashMap;
use std::fs;
use std::process::Command;

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde_json::{json, Value};

pub fn run_modules(args: &[String]) -> Result<()> {
    if args.is_empty() {
        print_usage();
        return Ok(());
    }
    let parsed = ModuleArgs::parse(&args[1..]);
    match args[0].as_str() {
        "list" => show(request(&parsed, "GET", "/modules", None)?),
        "inspect" | "status" => {
            let name = parsed.required_name()?;
            show(request(&parsed, "GET", &format!("/modules/{name}"), None)?)
        }
        "install" => {
            let artifact = parsed
                .positional
                .first()
                .ok_or_else(|| anyhow!("module install requires an artifact path"))?;
            let artifact_bytes = fs::read(artifact)?;
            show(request(
                &parsed,
                "POST",
                "/modules/install",
                Some(&json!({
                    "artifactBase64": base64::engine::general_purpose::STANDARD.encode(artifact_bytes),
                    "scope": parsed.scope()?
                })),
            )?)
        }
        "configure" => {
            let name = parsed.required_name()?;
            let path = parsed
                .values
                .get("file")
                .ok_or_else(|| anyhow!("module configure requires --file <path>"))?;
            let configuration = fs::read_to_string(path)?;
            show(request(
                &parsed,
                "POST",
                &format!("/modules/{name}/configure"),
                Some(&json!({"configuration": configuration})),
            )?)
        }
        "validate" | "stage" | "activate" | "drain" | "rollback" => {
            let operation = args[0].as_str();
            let name = parsed.required_name()?;
            show(request(
                &parsed,
                "POST",
                &format!("/modules/{name}/{operation}"),
                Some(&json!({"scope": parsed.scope()?})),
            )?)
        }
        "trust" if parsed.positional.first().map(String::as_str) == Some("add") => {
            let path = parsed
                .positional
                .get(1)
                .ok_or_else(|| anyhow!("module trust add requires a public-key file"))?;
            let public_key = fs::read_to_string(path)?;
            show(request(
                &parsed,
                "POST",
                "/modules/trust",
                Some(&json!({"publicKey": public_key})),
            )?)
        }
        "help" | "-h" | "--help" => print_usage(),
        other => bail!("unknown module subcommand: {other}"),
    }
    Ok(())
}

fn print_usage() {
    println!(
        "casparctl module - manage signed Aseman provider modules\n\n\
         Usage: casparctl module <command> [name|artifact] [flags]\n\n\
         Commands:\n  \
         list | inspect <name> | status <name>\n  \
         trust add <publisher-public-key>\n  \
         install <artifact> [--scope node|cluster]\n  \
         configure <name> --file <configuration>\n  \
         validate <name> | stage <name> | activate <name>\n  \
         drain <name> | rollback <name>\n\n\
         Global flags: --endpoint URL --token SECRET --scope node|cluster"
    );
}

struct ModuleArgs {
    endpoint: String,
    token: String,
    values: HashMap<String, String>,
    positional: Vec<String>,
}

impl ModuleArgs {
    fn parse(args: &[String]) -> Self {
        let config = aseman_config::cli_config();
        let mut endpoint = config
            .map(|value| value.cluster_endpoint.clone())
            .unwrap_or_else(|| "http://127.0.0.1:7440".to_owned());
        let mut token = config
            .map(|value| value.cluster_token.clone())
            .unwrap_or_default();
        let mut values = HashMap::new();
        let mut positional = Vec::new();
        let mut index = 0;
        while index < args.len() {
            if let Some(raw) = args[index].strip_prefix("--") {
                let (key, inline) = raw
                    .split_once('=')
                    .map_or((raw, None), |(key, value)| (key, Some(value)));
                let value = inline.map(str::to_owned).unwrap_or_else(|| {
                    index += 1;
                    args.get(index).cloned().unwrap_or_default()
                });
                match key {
                    "endpoint" => endpoint = value,
                    "token" => token = value,
                    _ => {
                        values.insert(key.to_owned(), value);
                    }
                }
            } else {
                positional.push(args[index].clone());
            }
            index += 1;
        }
        Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            token,
            values,
            positional,
        }
    }

    fn required_name(&self) -> Result<&str> {
        let value = self
            .positional
            .first()
            .map(String::as_str)
            .ok_or_else(|| anyhow!("a module name is required"))?;
        if !valid_name(value) {
            bail!("invalid module name");
        }
        Ok(value)
    }

    fn scope(&self) -> Result<&str> {
        match self.values.get("scope").map(String::as_str) {
            Some("cluster") => Ok("cluster"),
            Some("node") | None => Ok("node"),
            Some(_) => bail!("--scope must be node or cluster"),
        }
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
}

fn request(args: &ModuleArgs, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
    let url = format!("{}/v1/admin{}", args.endpoint, path);
    let mut command = Command::new("curl");
    command
        .arg("-sS")
        .arg("--fail-with-body")
        .arg("--max-time")
        .arg("35")
        .arg("-X")
        .arg(method)
        .arg("-H")
        .arg("content-type: application/json");
    if !args.token.is_empty() {
        command
            .arg("-H")
            .arg(format!("authorization: Bearer {}", args.token))
            .arg("-H")
            .arg(format!("x-caspar-cluster-token: {}", args.token));
    }
    if let Some(body) = body {
        command.arg("-d").arg(serde_json::to_string(body)?);
    }
    let output = command.arg(&url).output()?;
    if !output.status.success() {
        bail!(
            "module administration request failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| anyhow!("invalid module administration response: {error}"))
}

fn show(value: Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_cannot_escape_admin_paths() {
        assert!(valid_name("storage-postgres@1.0.0"));
        for value in ["", "../trust", "name/activate", "name?x=y", "has space"] {
            assert!(!valid_name(value), "accepted unsafe name: {value}");
        }
    }

    #[test]
    fn cluster_scope_is_explicit_and_fail_closed() {
        let cluster = ModuleArgs::parse(&["sample".into(), "--scope".into(), "cluster".into()]);
        assert_eq!(cluster.scope().unwrap(), "cluster");
        let invalid = ModuleArgs::parse(&["sample".into(), "--scope=world".into()]);
        assert!(invalid.scope().is_err());
    }
}
