use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "confidential-agent")]
#[command(about = "Confidential Agent host CLI")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,

    #[arg(
        long,
        env = "CA_SHELTER_BIN",
        default_value = "shelter",
        global = true,
        hide = true
    )]
    pub(crate) shelter_bin: PathBuf,

    #[arg(long, default_value = ".confidential-agent", global = true)]
    pub(crate) state_dir: PathBuf,

    #[arg(
        long,
        env = "CA_TOOLS_IMAGE",
        default_value = "confidential-agent-tools:latest",
        global = true
    )]
    pub(crate) tools_image: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    Build(BuildArgs),
    Deploy(DeployArgs),
    #[command(hide = true)]
    Inject(InjectArgs),
    #[command(hide = true)]
    Mesh(MeshArgs),
    Connect(ConnectArgs),
    Status(StatusArgs),
    Destroy(DestroyArgs),
}

#[derive(Debug, Args)]
pub(crate) struct BuildArgs {
    #[arg(long)]
    pub(crate) spec: PathBuf,
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DeployArgs {
    #[arg(long)]
    pub(crate) spec: PathBuf,
    #[arg(long, hide = true)]
    pub(crate) image_source: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub(crate) skip_inject: bool,
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InjectArgs {
    #[arg(long)]
    pub(crate) spec: PathBuf,
    #[arg(long)]
    pub(crate) target_ip: String,
}

#[derive(Debug, Args)]
pub(crate) struct MeshArgs {
    #[command(subcommand)]
    pub(crate) command: MeshCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MeshCommands {
    Sync {
        #[arg(long)]
        service: Option<String>,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ConnectArgs {
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long, help = "Query read-only status from live guest daemons")]
    pub(crate) live: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DestroyArgs {
    pub(crate) service: String,
}
