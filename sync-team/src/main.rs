mod github;
mod mailgun;
mod team_api;
mod utils;
mod zulip;

use crate::github::{create_diff, GitHubApiRead, GitHubWrite, HttpClient};
use crate::team_api::TeamApi;
use crate::zulip::SyncZulip;
use anyhow::Context;
use log::{error, info, warn};

const AVAILABLE_SERVICES: &[&str] = &["github", "mailgun", "zulip"];
const USER_AGENT: &str = "rust-lang teams sync (https://github.com/rust-lang/sync-team)";

fn usage() {
    eprintln!("available services:");
    for service in AVAILABLE_SERVICES {
        eprintln!("  {service}");
    }
    eprintln!("available flags:");
    eprintln!("  --help              Show this help message");
    eprintln!("  --live              Apply the proposed changes to the services");
    eprintln!("  --team-repo <path>  Path to the local team repo to use");
    eprintln!("  --only-print-plan   Print the execution plan without executing it");
    eprintln!("environment variables:");
    eprintln!("  GITHUB_TOKEN          Authentication token with GitHub");
    eprintln!("  MAILGUN_API_TOKEN     Authentication token with Mailgun");
    eprintln!("  EMAIL_ENCRYPTION_KEY  Key used to decrypt encrypted emails in the team repo");
    eprintln!("  ZULIP_USERNAME        Username of the Zulip bot");
    eprintln!("  ZULIP_API_TOKEN       Autnentication token of the Zulip bot");
}

fn app() -> anyhow::Result<()> {
    let mut dry_run = true;
    let mut next_team_repo = false;
    let mut only_print_plan = false;
    let mut team_repo = None;
    let mut services = Vec::new();
    for arg in std::env::args().skip(1) {
        if next_team_repo {
            team_repo = Some(arg);
            next_team_repo = false;
            continue;
        }
        match arg.as_str() {
            "--live" => dry_run = false,
            "--team-repo" => next_team_repo = true,
            "--help" => {
                usage();
                return Ok(());
            }
            "--only-print-plan" => only_print_plan = true,
            service if AVAILABLE_SERVICES.contains(&service) => services.push(service.to_string()),
            _ => {
                eprintln!("unknown argument: {arg}");
                usage();
                std::process::exit(1);
            }
        }
    }

    let team_api = team_repo
        .map(|p| TeamApi::Local(p.into()))
        .unwrap_or(TeamApi::Production);

    if services.is_empty() {
        info!("no service to synchronize specified, defaulting to all services");
        services = AVAILABLE_SERVICES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    }

    if dry_run {
        warn!("sync-team is running in dry mode, no changes will be applied.");
        warn!("run the binary with the --live flag to apply the changes.");
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
