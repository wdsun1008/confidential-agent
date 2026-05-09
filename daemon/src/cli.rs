use clap::{Args, Parser, Subcommand};
use confidential_agent_core::schema::DAEMON_STATUS_PORT;
use std::path::PathBuf;

const DEFAULT_CDH_ROOT: &str = "/run/confidential-containers/cdh";
const DEFAULT_BOOTSTRAP_RESOURCE: &str = "default/local-resources/cagent_bootstrap_config";
const DEFAULT_MESH_RESOURCE: &str = "default/local-resources/cagent_mesh_bundle";

#[derive(Debug, Parser)]
#[command(name = "confidential-agentd")]
#[command(about = "Confidential Agent guest daemon")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    Run(RunArgs),
    ApplyOnce(RunArgs),
    InitrdFetch(InitrdFetchArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RunArgs {
    #[arg(long, env = "CA_CDH_RESOURCE_ROOT", default_value = DEFAULT_CDH_ROOT)]
    pub(crate) cdh_root: PathBuf,

    #[arg(
        long,
        env = "CA_BOOTSTRAP_RESOURCE_PATH",
        default_value = DEFAULT_BOOTSTRAP_RESOURCE
    )]
    pub(crate) bootstrap_resource: String,

    #[arg(long, env = "CA_MESH_RESOURCE_PATH", default_value = DEFAULT_MESH_RESOURCE)]
    pub(crate) mesh_resource: String,

    #[arg(long, env = "CA_POLL_INTERVAL_SEC", default_value_t = 5)]
    pub(crate) poll_interval_sec: u64,

    #[arg(
        long,
        env = "CA_STATUS_LISTEN",
        default_value_t = default_status_listen()
    )]
    pub(crate) status_listen: String,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct InitrdFetchArgs {
    #[arg(long, env = "CA_CDH_RESOURCE_ROOT", default_value = DEFAULT_CDH_ROOT)]
    pub(crate) cdh_root: PathBuf,

    #[arg(
        long,
        env = "CA_BOOTSTRAP_RESOURCE_PATH",
        default_value = DEFAULT_BOOTSTRAP_RESOURCE
    )]
    pub(crate) bootstrap_resource: String,

    #[arg(
        long,
        env = "CA_DISK_KEY_RESOURCE_PATH",
        default_value = "default/local-resources/disk_passphrase"
    )]
    pub(crate) disk_key_resource: String,

    #[arg(
        long,
        env = "CA_INITRD_SECRET_STAGE_DIR",
        default_value = "/run/cai/secrets"
    )]
    pub(crate) stage_dir: PathBuf,

    #[arg(
        long,
        env = "CA_SECRET_WAIT_TIMEOUT_SEC",
        default_value_t = 600,
        help = "Seconds to wait for required initrd secrets; 0 waits forever"
    )]
    pub(crate) wait_timeout_sec: u64,

    #[arg(long, env = "CA_SECRET_RETRY_INTERVAL_SEC", default_value_t = 5)]
    pub(crate) retry_interval_sec: u64,
}

fn default_status_listen() -> String {
    format!("0.0.0.0:{DAEMON_STATUS_PORT}")
}
