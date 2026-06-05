use anyhow::{bail, Context, Result};
use cai_gateway::{run_gateway, GatewayConfig};
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        usage();
        bail!("missing command");
    }
    let command = args.remove(0);
    match command.as_str() {
        "serve" => {
            let config_path = parse_config_path(&args)?;
            let content = fs::read_to_string(&config_path)
                .with_context(|| format!("failed to read '{}'", config_path.display()))?;
            let config: GatewayConfig =
                serde_json::from_str(&content).context("invalid gateway config JSON")?;
            let shutdown = Arc::new(AtomicBool::new(false));
            let signal = shutdown.clone();
            ctrlc::set_handler(move || {
                signal.store(true, Ordering::SeqCst);
            })
            .context("failed to install signal handler")?;
            run_gateway(config, shutdown)
        }
        "audit-verify" => {
            let audit_path = parse_audit_path(&args)?;
            let report = cai_gateway::AuditStore::verify_path(&audit_path)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.valid {
                bail!("audit chain verification failed");
            }
            Ok(())
        }
        _ => {
            usage();
            bail!("unknown command '{command}'");
        }
    }
}

fn parse_config_path(args: &[String]) -> Result<PathBuf> {
    let mut config = None;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--config" => {
                idx += 1;
                config = Some(PathBuf::from(
                    args.get(idx).context("--config requires a value")?,
                ));
            }
            other => bail!("unknown serve argument '{other}'"),
        }
        idx += 1;
    }
    config.context("--config is required")
}

fn parse_audit_path(args: &[String]) -> Result<PathBuf> {
    let mut audit = None;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--audit" => {
                idx += 1;
                audit = Some(PathBuf::from(
                    args.get(idx).context("--audit requires a value")?,
                ));
            }
            other => bail!("unknown audit-verify argument '{other}'"),
        }
        idx += 1;
    }
    audit.context("--audit is required")
}

fn usage() {
    eprintln!("usage:");
    eprintln!("  cai-gateway serve --config /etc/cai/gateway.json");
    eprintln!("  cai-gateway audit-verify --audit /var/lib/cai-gateway/audit-8000.jsonl");
}
