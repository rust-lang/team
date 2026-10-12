mod crates_io;
mod github;
mod gws;
mod mailgun;
pub mod team_api;
pub mod utils;
mod zulip;

use anyhow::Context;
use chrono::Utc;
use crates_io::SyncCratesIo;
use github::{GitHubApiRead, GitHubWrite, HttpClient, create_diff};
use log::{info, warn};
use secrecy::SecretString;
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
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
                let client = HttpClient::new(&config).await?;
                let gh_read = Box::new(GitHubApiRead::from_client(client.clone())?);

                let limit_before = client.rate_limit().await?;
                log::info!(
                    "Rate limits before: REST {}, GraphQL {}",
                    RateLimitFmt(limit_before.resources.core.clone()),
                    RateLimitFmt(limit_before.resources.graphql.clone().unwrap_or_default())
                );

                let teams = team_api.get_teams().await?;
                let repos = team_api.get_repos().await?;

                let result = async {
                    let diff = create_diff(gh_read, teams, repos, config.clone()).await?;
                    if !diff.is_empty() {
                        info!("{diff}");
                    }
                    if !only_print_plan {
                        let gh_write = GitHubWrite::new(client.clone(), dry_run)?;
                        diff.apply(&gh_write).await?;
                    }
                    anyhow::Ok(())
                }
                .await;

                match client.rate_limit().await {
                    Ok(limit_after) => {
                        log::info!(
                            "Rate limits after: REST {}, GraphQL {}",
                            RateLimitFmt(limit_after.resources.core.clone()),
                            RateLimitFmt(limit_after.resources.graphql.clone().unwrap_or_default())
                        );
                        log::info!(
                            "Consumed {} REST calls and {} GraphQL calls",
                            limit_after
                                .resources
                                .core
                                .used
                                .saturating_sub(limit_before.resources.core.used),
                            limit_after
                                .resources
                                .graphql
                                .unwrap_or_default()
                                .used
                                .saturating_sub(
                                    limit_before.resources.graphql.unwrap_or_default().used
                                ),
                        )
                    }
                    Err(error) => {
                        log::error!("Cannot fetch rate limit after running GitHub sync: {error:?}");
                    }
                }

                result?;
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
            "google-workspace" => {
                println!("google-workspace: nothing to diff");
            }
            _ => panic!("unknown service: {service}"),
        }
    }

    Ok(())
}

struct RateLimitFmt(octocrab::models::Rate);

impl Display for RateLimitFmt {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let resets_at =
            chrono::DateTime::from_timestamp_secs(self.0.reset as i64).unwrap_or(Utc::now());
        write!(
            f,
            "{}/{} remaining, resets at {resets_at}",
            self.0.remaining, self.0.limit
        )
    }
}

fn get_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("failed to get the {key} environment variable"))
}
