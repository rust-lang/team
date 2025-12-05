mod api;

use crate::team_api::TeamApi;
use std::cmp::Ordering;

use crate::crates_io::api::{CratesIoApi, CratesIoOwner, OwnerKind, TrustedPublishingGitHubConfig};
use anyhow::Context;
use secrecy::SecretString;
use std::collections::{BTreeMap, HashSet};
use std::fmt::{Display, Formatter};

/// Special account that should own our managed crates.
const RUST_LANG_OWNER_ACCOUNT: &str = "rust-lang-owner";

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
struct CrateName(String);

impl Display for CrateName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct TeamOwner {
    org: String,
    name: String,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct CrateConfig {
    krate: CrateName,
    repo_org: String,
    repo_name: String,
    workflow_file: String,
    environment: String,
    trusted_publishing_only: bool,
    teams: Vec<TeamOwner>,
}

pub(crate) struct SyncCratesIo {
    crates_io_api: CratesIoApi,
    crates: BTreeMap<CrateName, CrateConfig>,
}

impl SyncCratesIo {
    pub(crate) fn new(
        token: SecretString,
        team_api: &TeamApi,
        dry_run: bool,
    ) -> anyhow::Result<Self> {
        let crates_io_api = CratesIoApi::new(token, dry_run);
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
                                teams: krate
                                    .teams
                                    .clone()
                                    .into_iter()
                                    .map(|owner| TeamOwner {
                                        org: owner.org,
                                        name: owner.name,
                                    })
                                    .collect(),
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
        let mut crate_diffs: Vec<CrateDiff> = vec![];

        let is_ci_dry_run = std::env::var("CI").is_ok() && self.crates_io_api.is_dry_run();

        // Note: we currently only support one trusted publishing configuration per crate
        for (krate, desired) in &self.crates {
            // Reading trusted publishing configs requires an authenticated token
            // We skip generating a diff for publishing configs on CI when dry-run is enabled,
            // to enable doing a crates.io dry-run without a privileged token.
            // Because crates.io does not currently support read-only token
            if !is_ci_dry_run {
                // Sync trusted publishing configs
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

            // Sync "trusted publishing only" crate option
            let trusted_publish_only_expected = desired.trusted_publishing_only;
            let crates_io_crate = self
                .crates_io_api
                .get_crate(&krate.0)
                .with_context(|| anyhow::anyhow!("Cannot load crate {krate}"))?;
            if crates_io_crate.trusted_publishing_only != trusted_publish_only_expected {
                crate_diffs.push(CrateDiff::SetTrustedPublishingOnly {
                    krate: krate.to_string(),
                    value: trusted_publish_only_expected,
                });
            }

            // Sync crate owners
            let owners = self
                .crates_io_api
                .list_crate_owners(&krate.0)
                .with_context(|| anyhow::anyhow!("Cannot list crate owners of {krate}"))?;

            let mut owners_to_add = vec![];

            let rust_lang_owner = CratesIoOwner::user(RUST_LANG_OWNER_ACCOUNT.to_string());
            // Make sure that `rust-lang-owner` is an owner of each managed crate
            if !owners.contains(&rust_lang_owner) {
                owners_to_add.push(rust_lang_owner);
            }

            // Sync team owners
            let existing_teams: HashSet<CratesIoOwner> = owners
                .iter()
                .filter(|owner| match owner.kind() {
                    OwnerKind::User => false,
                    OwnerKind::Team => true,
                })
                .cloned()
                .collect();
            let target_teams: HashSet<CratesIoOwner> = desired
                .teams
                .iter()
                .map(|team| CratesIoOwner::team(team.org.clone(), team.name.clone()))
                .collect();
            let teams_to_add = target_teams.difference(&existing_teams).cloned();
            owners_to_add.extend(teams_to_add);

            if !owners_to_add.is_empty() {
                crate_diffs.push(CrateDiff::AddOwners {
                    krate: krate.to_string(),
                    owners: owners_to_add,
                });
            }

            let teams_to_remove = existing_teams
                .difference(&target_teams)
                .cloned()
                .collect::<Vec<_>>();
            if !teams_to_remove.is_empty() {
                crate_diffs.push(CrateDiff::RemoveOwners {
                    krate: krate.to_string(),
                    owners: teams_to_remove,
                });
            }
        }

        // We want to apply deletions first, and only then create new configs, to ensure that we
        // don't try to create a duplicate config where e.g. only the environment differs.
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

enum CrateDiff {
    SetTrustedPublishingOnly {
        krate: String,
        value: bool,
    },
    AddOwners {
        krate: String,
        owners: Vec<CratesIoOwner>,
    },
    RemoveOwners {
        krate: String,
        owners: Vec<CratesIoOwner>,
    },
}

impl CrateDiff {
    fn apply(&self, sync: &SyncCratesIo) -> anyhow::Result<()> {
        match self {
            Self::SetTrustedPublishingOnly { krate, value } => sync
                .crates_io_api
                .set_trusted_publishing_only(krate, *value),
            CrateDiff::AddOwners { krate, owners } => {
                sync.crates_io_api.invite_crate_owners(krate, owners)
            }
            CrateDiff::RemoveOwners { krate, owners } => {
                sync.crates_io_api.delete_crate_owners(krate, owners)
            }
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
            CrateDiff::AddOwners { krate, owners } => {
                for owner in owners {
                    let kind = match owner.kind() {
                        OwnerKind::User => "user",
                        OwnerKind::Team => "team",
                    };
                    writeln!(
                        f,
                        "  Adding `{kind}` owner `{}` to krate `{krate}`",
                        owner.login()
                    )?;
                }
            }
            CrateDiff::RemoveOwners { krate, owners } => {
                for owner in owners {
                    let kind = match owner.kind() {
                        OwnerKind::User => "user",
                        OwnerKind::Team => "team",
                    };
                    writeln!(
                        f,
                        "  Removing `{kind}` owner `{}` from krate `{krate}`",
                        owner.login()
                    )?;
                }
            }
        }
        Ok(())
    }
}
