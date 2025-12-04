mod api;

use crate::team_api::TeamApi;
use std::cmp::Ordering;

use crate::crates_io::api::{CratesIoApi, CratesIoCrate, TrustedPublishingGitHubConfig, UserId};
use anyhow::Context;
use secrecy::SecretString;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
struct CrateName(String);

impl Display for CrateName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct CrateConfig {
    krate: CrateName,
    repo_org: String,
    repo_name: String,
    workflow_file: String,
    environment: String,
    trusted_publishing_only: bool,
}

pub(crate) struct SyncCratesIo {
    crates_io_api: CratesIoApi,
    crates: BTreeMap<CrateName, CrateConfig>,
    user_id: UserId,
    username: String,
}

impl SyncCratesIo {
    pub(crate) fn new(
        token: SecretString,
        username: String,
        team_api: &TeamApi,
        dry_run: bool,
    ) -> anyhow::Result<Self> {
        let crates_io_api = CratesIoApi::new(token, dry_run);
        let user_id = crates_io_api.get_user_id(&username)?;

        let crates: BTreeMap<CrateName, CrateConfig> = team_api
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
                            CrateConfig {
                                krate: CrateName(krate.name.clone()),
                                repo_org: repo.org.clone(),
                                repo_name: repo.name.clone(),
                                workflow_file: publishing.workflow_file.clone(),
                                environment: publishing.environment.clone(),
                                trusted_publishing_only: krate.trusted_publishing_only,
                            },
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        Ok(Self {
            crates_io_api,
            crates,
            user_id,
            username,
        })
    }

    pub(crate) fn diff_all(&self) -> anyhow::Result<Diff> {
        let mut config_diffs: Vec<ConfigDiff> = vec![];
        let mut crate_diffs: Vec<CrateDiff> = vec![];

        let is_ci_dry_run = std::env::var("CI").is_ok() && self.crates_io_api.is_dry_run();
        let mut tp_configs = if is_ci_dry_run {
            HashMap::new()
        } else {
            let tp_configs = self
                .crates_io_api
                .list_trusted_publishing_github_configs(self.user_id)
                .with_context(|| {
                    format!("Failed to list configs for user_id `{:?}`", self.user_id)
                })?;
            let tp_configs: HashMap<String, Vec<TrustedPublishingGitHubConfig>> = tp_configs
                .into_iter()
                .fold(HashMap::new(), |mut map, config| {
                    map.entry(config.krate.clone()).or_default().push(config);
                    map
                });
            tp_configs
        };

        // Batch load all crates owned by the current user
        let crates: HashMap<String, CratesIoCrate> = self
            .crates_io_api
            .get_crates_owned_by(self.user_id)?
            .into_iter()
            .map(|krate| (krate.name.clone(), krate))
            .collect();

        // Note: we currently only support one trusted publishing configuration per crate
        for (krate, desired) in &self.crates {
            // Reading trusted publishing configs requires an authenticated token
            // We skip generating a diff for publishing configs on CI when dry-run is enabled,
            // to enable doing a crates.io dry-run without a privileged token.
            // Because crates.io does not currently support read-only token
            if !is_ci_dry_run {
                let mut empty_vec = vec![];
                let configs = tp_configs.get_mut(&krate.0).unwrap_or(&mut empty_vec);

                // Find if there are config(s) that match what we need and remove them from the list
                // of found configs.
                let matching_configs = configs
                    .extract_if(.., |config| {
                        let TrustedPublishingGitHubConfig {
                            krate: _,
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
                config_diffs.extend(configs.iter_mut().map(|c| ConfigDiff::Delete(c.clone())));
            }

            let Some(crates_io_crate) = crates.get(&krate.0) else {
                return Err(anyhow::anyhow!(
                    "Crate `{krate}` is not owned by user `{0}`. Please invite `{0}` to be its owner.",
                    self.username
                ));
            };
            if crates_io_crate.trusted_publishing_only != desired.trusted_publishing_only {
                crate_diffs.push(CrateDiff::SetTrustedPublishingOnly {
                    krate: krate.to_string(),
                    value: desired.trusted_publishing_only,
                });
            }
        }

        // If any trusted publishing configs remained in the hashmap, they are leftover and should
        // be removed.
        for config in tp_configs.into_values().flatten() {
            config_diffs.push(ConfigDiff::Delete(config));
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

        Ok(Diff {
            config_diffs,
            crate_diffs,
        })
    }
}

pub(crate) struct Diff {
    config_diffs: Vec<ConfigDiff>,
    crate_diffs: Vec<CrateDiff>,
}

impl Diff {
    pub(crate) fn apply(&self, sync: &SyncCratesIo) -> anyhow::Result<()> {
        let Diff {
            config_diffs,
            crate_diffs,
        } = self;

        for diff in config_diffs {
            diff.apply(sync)?;
        }
        for diff in crate_diffs {
            diff.apply(sync)?;
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        // Destructure struct to get compiler errors when new fields are added
        let Diff {
            config_diffs,
            crate_diffs,
        } = self;

        config_diffs.is_empty() && crate_diffs.is_empty()
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Destructure struct to get compiler errors when new fields are added
        let Diff {
            config_diffs,
            crate_diffs,
        } = self;

        if !config_diffs.is_empty() {
            writeln!(f, "💻 Trusted Publishing Config Diffs:")?;
            for diff in config_diffs {
                write!(f, "{diff}")?;
            }
        }

        if !crate_diffs.is_empty() {
            writeln!(f, "💻 Trusted Publishing Crate Diffs:")?;
            for diff in crate_diffs {
                write!(f, "{diff}")?;
            }
        }
        Ok(())
    }
}

enum ConfigDiff {
    Create(CrateConfig),
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
                    "  Creating trusted publishing config for crate `{}`",
                    config.krate.0
                )?;
                writeln!(f, "    Repo: {}/{}", config.repo_org, config.repo_name)?;
                writeln!(f, "    Workflow file: {}", config.workflow_file)?;
                writeln!(f, "    Environment: {}", config.environment)?;
            }
            ConfigDiff::Delete(config) => {
                writeln!(
                    f,
                    "  Deleting trusted publishing config for crate `{}` (ID {})",
                    config.krate, config.id
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

enum CrateDiff {
    SetTrustedPublishingOnly { krate: String, value: bool },
}

impl CrateDiff {
    fn apply(&self, sync: &SyncCratesIo) -> anyhow::Result<()> {
        match self {
            Self::SetTrustedPublishingOnly { krate, value } => sync
                .crates_io_api
                .set_trusted_publishing_only(krate, *value),
        }
    }
}

impl std::fmt::Display for CrateDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SetTrustedPublishingOnly { krate, value } => {
                writeln!(
                    f,
                    "  Setting trusted publishing only option for krate `{krate}` to `{value}`",
                )?;
            }
        }
        Ok(())
    }
}
