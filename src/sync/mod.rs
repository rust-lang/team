mod crates_io;
mod github;
mod mailgun;
pub mod team_api;
pub mod utils;
mod zulip;

use std::collections::BTreeSet;

use anyhow::Context;
use crates_io::SyncCratesIo;
use github::{GitHubApiRead, GitHubWrite, HttpClient, create_diff};
use log::{info, warn};
use secrecy::SecretString;
use team_api::TeamApi;
use zulip::SyncZulip;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub special_org_members: BTreeSet<String>,
    pub independent_github_orgs: BTreeSet<String>,
}

pub async fn run_sync_team(
    team_api: TeamApi,
    services: &[String],
    dry_run: bool,
    only_print_plan: bool,
    config: Config,
) -> anyhow::Result<()> {
    if dry_run {
        warn!("sync-team is running in dry mode, no changes will be applied.");
    }

    for service in services {
        info!("synchronizing {service}");
        match service.as_str() {
            "github" => {
                let client = HttpClient::new()?;
                let gh_read = Box::new(GitHubApiRead::from_client(client.clone())?);
                let teams = team_api.get_teams().await?;
                let repos = team_api.get_repos().await?;
                let diff = create_diff(gh_read, teams, repos, config.clone()).await?;
                if !diff.is_empty() {
                    info!("{diff}");
                }
                if !only_print_plan {
                    let gh_write = GitHubWrite::new(client, dry_run)?;
                    diff.apply(&gh_write).await?;
                }
            }
            "mailgun" => {
                let token = SecretString::from(get_env("MAILGUN_API_TOKEN")?);
                let private_key = get_env("EMAIL_PRIVATE_KEY")?;
                mailgun::run(token, &private_key, &team_api, dry_run).await?;
            }
            "zulip" => {
                let username = get_env("ZULIP_USERNAME")?;
                let token = SecretString::from(get_env("ZULIP_API_TOKEN")?);
                let sync = SyncZulip::new(username, token, &team_api, dry_run).await?;
                let diff = sync.diff_all().await?;
                if !diff.is_empty() {
                    info!("{diff}");
                }
                if !only_print_plan {
                    diff.apply(&sync).await?;
                }
            }
            "crates-io" => {
                let token = SecretString::from(get_env("CRATES_IO_TOKEN")?);
                let username = get_env("CRATES_IO_USERNAME")?;
                let sync = SyncCratesIo::new(token, username, &team_api, dry_run).await?;
                let diff = sync.diff_all().await?;
                if !diff.is_empty() {
                    info!("{diff}");
                }
                if !only_print_plan {
                    diff.apply(&sync).await?;
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
