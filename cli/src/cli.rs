use clap::{Args, Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

fn default_state_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".confidential-agent")
}

#[derive(Debug, Parser)]
#[command(name = "confidential-agent")]
#[command(about = "Confidential Agent host CLI")]
#[command(version)]
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

    #[arg(long, default_value_os_t = default_state_dir(), global = true)]
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
    Docs(DocsArgs),
    Spec(SpecArgs),
    Key(KeyArgs),
    #[command(hide = true)]
    Inject(InjectArgs),
    #[command(hide = true)]
    Mesh(MeshArgs),
    Connect(ConnectArgs),
    Peering(PeeringArgs),
    A2a(A2aArgs),
    Migrate(MigrateArgs),
    Image(ImageArgs),
    Ssh(SshArgs),
    Status(StatusArgs),
    Report(ReportArgs),
    Destroy(DestroyArgs),
    Version,
}

#[derive(Debug, Args)]
pub(crate) struct BuildArgs {
    #[arg(long, default_value = "confidential-agent.yaml")]
    pub(crate) spec: PathBuf,
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DeployArgs {
    #[arg(long, default_value = "confidential-agent.yaml")]
    pub(crate) spec: PathBuf,
    #[arg(long, hide = true)]
    pub(crate) skip_inject: bool,
    #[arg(long, hide = true)]
    pub(crate) render_only: bool,
    #[arg(long, env = "CA_SKIP_PEERING_CHECK")]
    pub(crate) skip_peering_check: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum OutputFormat {
    Markdown,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DocsTopic {
    Overview,
    Workflow,
    Appspec,
    Ops,
}

#[derive(Debug, Args)]
pub(crate) struct DocsArgs {
    #[arg(value_enum)]
    pub(crate) topic: DocsTopic,
    #[arg(long, value_enum, default_value = "markdown")]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct SpecArgs {
    #[command(subcommand)]
    pub(crate) command: SpecCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SpecCommands {
    Schema {
        #[arg(long, value_enum, default_value = "markdown")]
        format: OutputFormat,
    },
    Validate {
        #[arg(long, default_value = "confidential-agent.yaml")]
        spec: PathBuf,
        #[arg(long, value_enum, default_value = "markdown")]
        format: OutputFormat,
    },
}

#[derive(Debug, Args)]
pub(crate) struct KeyArgs {
    #[command(subcommand)]
    pub(crate) command: KeyCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyCommands {
    #[command(name = "generate-cosign")]
    GenerateCosign {
        #[arg(long, default_value = "./cosign")]
        output_key_prefix: PathBuf,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct InjectArgs {
    #[arg(long)]
    pub(crate) spec: PathBuf,
    #[arg(long)]
    pub(crate) target_ip: String,
    #[arg(long, env = "CA_SKIP_PEERING_CHECK")]
    pub(crate) skip_peering_check: bool,
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
    #[arg(long)]
    pub(crate) from_card: Option<String>,
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[command(subcommand)]
    pub(crate) command: Option<ConnectCommands>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConnectCommands {
    Start(ConnectStartArgs),
    Stop(ConnectStopArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ConnectStartArgs {
    #[arg(long)]
    pub(crate) from_card: Option<String>,
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[arg(long, default_value = "connect-ready.json")]
    pub(crate) ready_json: PathBuf,
    #[arg(long, default_value_t = 120)]
    pub(crate) wait_ready: u64,
    #[arg(long)]
    pub(crate) log_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ConnectStopArgs {
    #[arg(long, default_value = "connect-ready.json")]
    pub(crate) ready_json: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct PeeringArgs {
    #[command(subcommand)]
    pub(crate) command: PeeringCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PeeringCommands {
    Add {
        #[arg(long)]
        role: String,
        #[arg(long)]
        cidr: String,
        #[arg(long)]
        label: String,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },
    List,
    Show {
        label: String,
    },
    Remove {
        label: String,
    },
    Apply {
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct A2aArgs {
    #[command(subcommand)]
    pub(crate) command: A2aCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum A2aCommands {
    Add {
        agent_card_url: String,
        #[arg(long)]
        alias: Option<String>,
        #[arg(long, value_delimiter = ',')]
        service: Vec<String>,
        #[arg(
            long,
            help = "Expected OIDC issuer for the peer AgentCard Sigstore keyless signature"
        )]
        signer_issuer: Option<String>,
        #[arg(
            long,
            help = "Expected certificate identity/subject for the peer AgentCard Sigstore keyless signature"
        )]
        signer_subject: Option<String>,
    },
    Remove {
        alias_or_url: String,
    },
    List,
    Show {
        alias_or_url: String,
    },
    Sync {
        #[arg(long)]
        alias: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct MigrateArgs {
    pub(crate) spec: PathBuf,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    #[arg(long)]
    pub(crate) peerings_out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ImageArgs {
    #[command(subcommand)]
    pub(crate) command: ImageCommands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ImageCommands {
    List {
        #[arg(long)]
        json: bool,
    },
    Rm {
        service: String,
        #[arg(long, short)]
        force: bool,
    },
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
pub(crate) struct SshArgs {
    pub(crate) service: String,
    #[arg(last = true)]
    pub(crate) ssh_args: Vec<OsString>,
}

#[derive(Debug, Args)]
pub(crate) struct ReportArgs {
    #[arg(long)]
    pub(crate) service: Option<String>,
    #[arg(long)]
    pub(crate) include_a2a: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct DestroyArgs {
    pub(crate) service: String,
}
