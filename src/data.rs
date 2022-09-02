use crate::schema::{Config, List, Person, Repo, Team, ZulipGroup};
use failure::{bail, Error, ResultExt};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Data {
    people: HashMap<String, Person>,
    teams: HashMap<String, Team>,
    repos: HashMap<(String, String), Repo>,
    config: Config,
}

impl Data {
    pub(crate) fn load() -> Result<Self, Error> {
        let mut data = Data {
            people: HashMap::new(),
            teams: HashMap::new(),
            repos: HashMap::new(),
            config: load_file(Path::new("config.toml"))?,
        };

        data.load_dir("repos", true, |this, org, repo: Repo| {
            repo.validate()?;
            if &repo.org != org {
                bail!(
                    "repo '{}' is located in the '{}' org directory but its org is '{}'",
                    repo.name,
                    org,
                    repo.org
                )
            }

            this.repos
                .insert((repo.org.clone(), repo.name.clone()), repo);
            Ok(())
        })?;

        data.load_dir("people", false, |this, _dir, person: Person| {
            person.validate()?;
            this.people.insert(person.github().to_string(), person);
            Ok(())
        })?;

        data.load_dir("teams", false, |this, _dir, team: Team| {
            this.teams.insert(team.name().to_string(), team);
            Ok(())
        })?;

        Ok(data)
    }

    fn load_dir<P, T, F>(&mut self, dir: P, nested: bool, f: F) -> Result<(), Error>
    where
        P: AsRef<Path>,
        T: for<'de> Deserialize<'de>,
        F: Fn(&mut Self, &str, T) -> Result<(), Error>,
        F: Clone,
    {
        for entry in std::fs::read_dir(&dir).with_context(|e| {
            let dir = dir.as_ref().display();
            format!("`load_dir` failed to read directory '{}': {}", dir, e)
        })? {
            let path = entry?.path();
            if nested && path.is_dir() {
                self.load_dir(&path, false, f.clone())?;
            } else if !nested && path.is_file() && path.extension() == Some(OsStr::new("toml")) {
                fn dir(path: &PathBuf) -> Option<&str> {
                    Some(path.parent()?.file_name()?.to_str()?)
                }
                f(self, dir(&path).unwrap(), load_file(&path)?)?;
            }
        }

        Ok(())
    }

    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn lists(&self) -> Result<HashMap<String, List>, Error> {
        let mut lists = HashMap::new();
        for team in self.teams.values() {
            for list in team.lists(self)? {
                lists.insert(list.address().to_string(), list);
            }
        }
        Ok(lists)
    }

    pub(crate) fn list(&self, name: &str) -> Result<Option<List>, Error> {
        let mut lists = self.lists()?;
        Ok(lists.remove(name))
    }

    pub(crate) fn zulip_groups(&self) -> Result<HashMap<String, ZulipGroup>, Error> {
        let mut groups = HashMap::new();
        for team in self.teams() {
            for group in team.zulip_groups(self)? {
                groups.insert(group.name().to_string(), group);
            }
        }
        Ok(groups)
    }

    pub(crate) fn team(&self, name: &str) -> Option<&Team> {
        self.teams.get(name)
    }

    pub(crate) fn teams(&self) -> impl Iterator<Item = &Team> {
        self.teams.values()
    }

    pub(crate) fn person(&self, name: &str) -> Option<&Person> {
        self.people.get(name)
    }

    pub(crate) fn people(&self) -> impl Iterator<Item = &Person> {
        self.people.values()
    }

    pub(crate) fn active_members(&self) -> Result<HashSet<&str>, Error> {
        let mut result = HashSet::new();
        for team in self.teams.values() {
            if team.is_alumni_team() {
                continue;
            }
            for member in team.members(self)? {
                result.insert(member);
            }
        }
        Ok(result)
    }

    pub(crate) fn repos(&self) -> impl Iterator<Item = &Repo> {
        self.repos.iter().map(|(_, repo)| repo)
    }
}

fn load_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Error> {
    let content =
        std::fs::read(&path).with_context(|_| format!("failed to read {}", path.display()))?;
    let parsed = toml::from_slice(&content)
        .with_context(|_| format!("failed to parse {}", path.display()))?;
    Ok(parsed)
}
