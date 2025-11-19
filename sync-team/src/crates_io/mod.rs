mod api;

use crate::team_api::TeamApi;
use std::cmp::Ordering;

use crate::crates_io::api::{CratesIoApi, TrustedPublishingGitHubConfig};
use anyhow::Context;
use secrecy::SecretString;
use std::collections::HashMap;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
struct CrateName(String);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct CratesIoPublishingConfig {
    krate: CrateName,
    repo_org: String,
    repo_name: String,
    workflow_file: String,
    environment: String,
}

pub(crate) struct SyncCratesIo {
    crates_io_api: CratesIoApi,
    crates: HashMap<CrateName, CratesIoPublishingConfig>,
}

impl SyncCratesIo {
    pub(crate) fn new(
        token: SecretString,
        team_api: &TeamApi,
        dry_run: bool,
    ) -> anyhow::Result<Self> {
        let crates_io_api = CratesIoApi::new(token, dry_run);
        let crates: HashMap<CrateName, CratesIoPublishingConfig> = team_api
            .get_repos()?
            .into_iter()
            .flat_map(|repo| {
                repo.crates
                    .iter()
                    .filter_map(|krate| {
                        let Some(publishing) = &krate.crates_io_publishing else {
                            return None;
                        };
                        Some((
                            CrateName(krate.name.clone()),
                            CratesIoPublishingConfig {
                                krate: CrateName(krate.name.clone()),
                                repo_org: repo.org.clone(),
                                repo_name: repo.name.clone(),
                                workflow_file: publishing.workflow_file.clone(),
                                environment: publishing.environment.clone(),
                            },
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        Ok(Self {
            crates_io_api,
            crates,
        })
    }

    pub(crate) fn diff_all(&self) -> anyhow::Result<Diff> {
        let mut config_diffs: Vec<ConfigDiff> = vec![];

        // Note: we currently only support one trusted publishing configuration per crate
        for (krate, desired) in &self.crates {
            let mut configs = self
                .crates_io_api
                .list_trusted_publishing_github_configs(&krate.0)
                .with_context(|| format!("Failed to list configs for crate '{}'", krate.0))?;

            // Find if there are config(s) that match what we need
            let matching_configs = configs
                .extract_if(.., |config| {
                    let TrustedPublishingGitHubConfig {
                        id: _,
                        repository_owner,
                        repository_name,
                        workflow_filename,
                        environment,
                    } = config;
                    *repository_owner.to_lowercase() == desired.repo_org.to_lowercase()
                        && *repository_name.to_lowercase() == desired.repo_name.to_lowercase()
                        && *workflow_filename == desired.workflow_file
                        && environment.as_deref() == Some(&desired.environment)
                })
                .collect::<Vec<_>>();

            if !matching_configs.is_empty() {
                // If we found a matching config, we don't need to do anything with it
                // It shouldn't be possible to have multiple configs with the same repo, workflow
                // and environment for a single crate.
                assert_eq!(matching_configs.len(), 1);
            } else {
                // If no match was found, we want to create this config
                config_diffs.push(ConfigDiff::Create(desired.clone()));
            }

            // Non-matching configs should be deleted
            config_diffs.extend(configs.into_iter().map(ConfigDiff::Delete));
        }

        // We want to apply deletions first, and only then create new configs, to ensure that we
        // don't try to create a duplicate config where e.g. only the environment differs, which
        // would be an error in crates.io.
        config_diffs.sort_by(|a, b| match &(a, b) {
            (ConfigDiff::Delete(_), ConfigDiff::Create(_)) => Ordering::Less,
            (ConfigDiff::Create(_), ConfigDiff::Delete(_)) => Ordering::Greater,
            (ConfigDiff::Delete(a), ConfigDiff::Delete(b)) => a.id.cmp(&b.id),
            (ConfigDiff::Create(a), ConfigDiff::Create(b)) => a.cmp(b),
        });

        Ok(Diff { config_diffs })
    }
}

pub(crate) struct Diff {
    config_diffs: Vec<ConfigDiff>,
}

impl Diff {
    pub(crate) fn apply(&self, sync: &SyncCratesIo) -> anyhow::Result<()> {
        for diff in &self.config_diffs {
            diff.apply(sync)?;
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.config_diffs.is_empty()
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !&self.config_diffs.is_empty() {
            writeln!(f, "💻 Trusted Publishing Config Diffs:")?;
            for diff in &self.config_diffs {
                write!(f, "{diff}")?;
            }
        }

        Ok(())
    }
}

enum ConfigDiff {
    Create(CratesIoPublishingConfig),
    Delete(TrustedPublishingGitHubConfig),
}
impl ConfigDiff {
    fn apply(&self, sync: &SyncCratesIo) -> anyhow::Result<()> {
        match self {
            ConfigDiff::Create(config) => sync
                .crates_io_api
                .create_trusted_publishing_github_config(config),
            ConfigDiff::Delete(config) => sync
                .crates_io_api
                .delete_trusted_publishing_github_config(config.id),
        }
    }
}

impl std::fmt::Display for ConfigDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigDiff::Create(config) => {
                writeln!(
                    f,
                    "  Creating trusted publishing config for krate `{}`",
                    config.krate.0
                )?;
                writeln!(f, "    Repo: {}/{}", config.repo_org, config.repo_name)?;
                writeln!(f, "    Workflow file: {}", config.workflow_file)?;
                writeln!(f, "    Environment: {}", config.environment)?;
            }
            ConfigDiff::Delete(config) => {
                writeln!(
                    f,
                    "  Deleting trusted publishing config with ID {}",
                    config.id
                )?;
                writeln!(
                    f,
                    "    Repo: {}/{}",
                    config.repository_owner, config.repository_name
                )?;
                writeln!(f, "    Workflow file: {}", config.workflow_filename)?;
                writeln!(
                    f,
                    "    Environment: {}",
                    config.environment.as_deref().unwrap_or("(none)")
                )?;
            }
        }
        Ok(())
    }
}
