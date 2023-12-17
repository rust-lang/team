use crate::data::Data;
use crate::schema::{Config, Person};
use anyhow::{bail, Error};
use std::collections::{HashMap, HashSet};

#[derive(serde_derive::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct BorsAcl {
    #[serde(default)]
    review: bool,
    #[serde(rename = "try", default)]
    try_: bool,
}

impl BorsAcl {
    pub(crate) fn review(&self) -> bool {
        self.review
    }

    pub(crate) fn try_(&self) -> bool {
        self.try_
    }
}

#[derive(serde_derive::Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Permissions {
    #[serde(default)]
    bors: HashMap<String, BorsAcl>,
    #[serde(flatten)]
    booleans: HashMap<String, bool>,
}

impl Permissions {
    pub(crate) fn bors(&self) -> &HashMap<String, BorsAcl> {
        &self.bors
    }

    pub(crate) fn booleans(&self) -> &HashMap<String, bool> {
        &self.booleans
    }

    pub(crate) fn available(config: &Config) -> Vec<String> {
        let mut result = Vec::new();

        for boolean in config.permissions_bools() {
            result.push(boolean.to_string());
        }
        for repo in config.permissions_bors_repos() {
            result.push(format!("bors.{}.review", repo));
            result.push(format!("bors.{}.try", repo));
        }

        result
    }

    pub(crate) fn has(&self, permission: &str) -> bool {
        self.has_directly(permission) || self.has_indirectly(permission)
    }

    pub(crate) fn has_directly(&self, permission: &str) -> bool {
        match permission.split('.').collect::<Vec<_>>().as_slice() {
            [boolean] => self.booleans.get(*boolean).cloned(),
            ["bors", repo, "review"] => self.bors.get(*repo).map(|repo| repo.review),
            ["bors", repo, "try"] => self.bors.get(*repo).map(|repo| repo.try_),
            _ => None,
        }
        .unwrap_or(false)
    }

    pub fn has_indirectly(&self, permission: &str) -> bool {
        match permission.split('.').collect::<Vec<_>>().as_slice() {
            ["bors", repo, "try"] => self.bors.get(*repo).map(|repo| repo.review),
            _ => None,
        }
        .unwrap_or(false)
    }

    pub(crate) fn has_any(&self) -> bool {
        for permission in self.booleans.values() {
            if *permission {
                return true;
            }
        }
        for repo in self.bors.values() {
            if repo.review || repo.try_ {
                return true;
            }
        }
        false
    }

    pub(crate) fn validate(&self, what: String, config: &Config) -> Result<(), Error> {
        for boolean in self.booleans.keys() {
            if !config.permissions_bools().contains(boolean) {
                bail!(
                    "unknown permission: {} (maybe add it to config.toml?)",
                    boolean
                );
            }
        }
        for (repo, perms) in self.bors.iter() {
            if !config.permissions_bors_repos().contains(repo) {
                bail!(
                    "unknown bors repository: {} (maybe add it to config.toml?)",
                    repo
                );
            }
            if perms.try_ && perms.review {
                bail!(
                    "{} has both the `bors.{}.review` and `bors.{}.try` permissions",
                    what,
                    repo,
                    repo,
                );
            }
        }
        Ok(())
    }
}

pub(crate) fn allowed_people<'a>(
    data: &'a Data,
    permission: &str,
) -> Result<Vec<&'a Person>, Error> {
    let mut members_with_perms = HashSet::new();
    for team in data.teams() {
        if team.permissions().has(permission) {
            members_with_perms.extend(team.members(data)?);
        }
        if team.leads_permissions().has(permission) {
            members_with_perms.extend(team.leads());
        }
    }
    Ok(data
        .people()
        .filter(|p| members_with_perms.contains(p.github()) || p.permissions().has(permission))
        .collect())
}
