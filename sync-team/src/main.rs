mod github;
mod mailgun;
mod team_api;
mod utils;
mod zulip;

use crate::github::{GitHubApiRead, GitHubWrite, HttpClient, create_diff};
use crate::team_api::TeamApi;
use crate::zulip::SyncZulip;
use anyhow::Context;
use clap::Parser;
use log::{error, info, warn};
use std::path::PathBuf;

const AVAILABLE_SERVICES: &[&str] = &["github", "mailgun", "zulip"];
const USER_AGENT: &str = "rust-lang teams sync (https://github.com/rust-lang/sync-team)";

/// Tooling that performs changes on GitHub, MailGun and Zulip.
///
/// Environment variables:
/// - GITHUB_TOKEN          Authentication token with GitHub
/// - MAILGUN_API_TOKEN     Authentication token with Mailgun
/// - EMAIL_ENCRYPTION_KEY  Key used to decrypt encrypted emails in the team repo
/// - ZULIP_USERNAME        Username of the Zulip bot
/// - ZULIP_API_TOKEN       Authentication token of the Zulip bot
#[derive(clap::Parser, Debug)]
#[clap(verbatim_doc_comment)]
struct Args {
    /// Comma-separated list of available services
    #[clap(long, global(true), value_parser = clap::builder::PossibleValuesParser::new(
        AVAILABLE_SERVICES
    ), value_delimiter = ',')]
    services: Vec<String>,

    /// Path to a checkout of `rust-lang/team`.
    #[clap(long, global(true), conflicts_with = "team_json")]
    team_repo: Option<PathBuf>,

    /// Path to a directory with prebuilt JSON data from the `team` repository.
    #[clap(long, global(true))]
    team_json: Option<PathBuf>,

    #[clap(subcommand)]
    command: Option<SubCommand>,
}

#[derive(clap::Parser, Debug)]
enum SubCommand {
    /// Try to apply changes, but do not send any outgoing API requests.
    DryRun,
    /// Only print a diff of what would be changed.
    PrintPlan,
    /// Apply the changes to the specified services.
    Apply,
}

fn app() -> anyhow::Result<()> {
    let args = Args::parse();

    let team_api = if let Some(path) = args.team_repo {
        TeamApi::Checkout(path)
    } else if let Some(path) = args.team_json {
        TeamApi::Prebuilt(path)
    } else {
        TeamApi::Production
    };

    let mut services = args.services;
    if services.is_empty() {
        info!("no service to synchronize specified, defaulting to all services");
        services = AVAILABLE_SERVICES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    }

    let subcmd = args.command.unwrap_or(SubCommand::DryRun);
    let only_print_plan = matches!(subcmd, SubCommand::PrintPlan);
    let dry_run = only_print_plan || matches!(subcmd, SubCommand::DryRun);

    if dry_run {
        warn!("sync-team is running in dry mode, no changes will be applied.");
    }

    for service in services {
        info!("synchronizing {}", service);
        match service.as_str() {
            "github" => {
                let client = HttpClient::new()?;
                let gh_read = Box::new(GitHubApiRead::from_client(client.clone())?);
                let teams = team_api.get_teams()?;
                let repos = team_api.get_repos()?;
                let diff = create_diff(gh_read, teams, repos)?;
                info!("{}", diff);
                if !only_print_plan {
                    let gh_write = GitHubWrite::new(client, dry_run)?;
                    diff.apply(&gh_write)?;
                }
            }
            "mailgun" => {
                let token = get_env("MAILGUN_API_TOKEN")?;
                let encryption_key = get_env("EMAIL_ENCRYPTION_KEY")?;
                mailgun::run(&token, &encryption_key, &team_api, dry_run)?;
            }
            "zulip" => {
                let username = get_env("ZULIP_USERNAME")?;
                let token = get_env("ZULIP_API_TOKEN")?;
                let sync = SyncZulip::new(username, token, &team_api, dry_run)?;
                let diff = sync.diff_all()?;
                info!("{}", diff);
                if !only_print_plan {
                    diff.apply(&sync)?;
                }
            }
            _ => panic!("unknown service: {service}"),
        }
    }

    Ok(())
}

fn get_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("failed to get the {key} environment variable"))
}

fn main() {
    init_log();
    if let Err(err) = app() {
        // Display shows just the first element of the chain.
        error!("failed: {}", err);
        for cause in err.chain().skip(1) {
            error!("caused by: {}", cause);
        }
        std::process::exit(1);
    }
}

fn init_log() {
    let mut env = env_logger::Builder::new();
    env.filter_module("sync_team", log::LevelFilter::Info);
    if let Ok(content) = std::env::var("RUST_LOG") {
        env.parse_filters(&content);
    }
    env.init();
}
