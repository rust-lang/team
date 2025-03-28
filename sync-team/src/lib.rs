mod github;
mod mailgun;
pub mod team_api;
mod utils;
mod zulip;

use crate::github::{GitHubApiRead, GitHubWrite, HttpClient, create_diff};
use crate::team_api::TeamApi;
use crate::zulip::SyncZulip;
use anyhow::Context;
use log::{info, warn};
use secrecy::SecretString;

const USER_AGENT: &str = "rust-lang teams sync (https://github.com/rust-lang/sync-team)";

pub fn run_sync_team(
    team_api: TeamApi,
    services: &[String],
    dry_run: bool,
    only_print_plan: bool,
) -> anyhow::Result<()> {
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
                if !diff.is_empty() {
                    info!("{}", diff);
                }
                if !only_print_plan {
                    let gh_write = GitHubWrite::new(client, dry_run)?;
                    diff.apply(&gh_write)?;
                }
            }
            "mailgun" => {
                let token = SecretString::from(get_env("MAILGUN_API_TOKEN")?);
                let encryption_key = get_env("EMAIL_ENCRYPTION_KEY")?;
                mailgun::run(token, &encryption_key, &team_api, dry_run)?;
            }
            "zulip" => {
                let username = get_env("ZULIP_USERNAME")?;
                let token = SecretString::from(get_env("ZULIP_API_TOKEN")?);
                let sync = SyncZulip::new(username, token, &team_api, dry_run)?;
                let diff = sync.diff_all()?;
                if !diff.is_empty() {
                    info!("{}", diff);
                }
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
