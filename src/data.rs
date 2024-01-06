use crate::schema::{Config, List, Person, Repo, Team, ZulipGroup};
use anyhow::{bail, Context as _, Error};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;

#[derive(Debug)]
pub(crate) struct Data {
    people: HashMap<String, Person>,
    teams: HashMap<String, Team>,
    archived_teams: Vec<Team>,
    repos: Vec<Repo>,
    archived_repos: Vec<Repo>,
    config: Config,
}

impl Data {
    pub(crate) fn load() -> Result<Self, Error> {
        let mut data = Data {
            people: HashMap::new(),
            teams: HashMap::new(),
            archived_teams: Vec::new(),
            repos: Vec::new(),
            archived_repos: Vec::new(),
            config: load_file(Path::new("config.toml"))?,
        };

        fn validate_repo(org: &str, repo: &Repo, path: &Path) -> anyhow::Result<()> {
            if repo.org != org {
                bail!(
                    "repo '{}' is located in the '{}' org directory but its org is '{}'",
                    repo.name,
                    org,
                    repo.org
                )
            }
            if repo.name != path.file_stem().unwrap().to_str().unwrap() {
                bail!(
                    "repo '{}' is located in file '{}', please ensure that the name matches",
                    repo.name,
                    path.file_name().unwrap().to_str().unwrap()
                )
            }
            Ok(())
        }

        data.load_dir("repos", true, |this, org, repo: Repo, path: &Path| {
            if org == "archive" {
                return Ok(());
            }

            validate_repo(org, &repo, path)?;
            this.repos.push(repo);
            Ok(())
        })?;

        if Path::new("repos/archive").is_dir() {
            data.load_dir(
                "repos/archive",
                true,
                |this, org, repo: Repo, path: &Path| {
                    validate_repo(org, &repo, path)?;
                    this.archived_repos.push(repo);
                    Ok(())
                },
            )?;
        }

        data.load_dir("people", false, |this, _dir, person: Person, _path| {
            person.validate()?;
            this.people.insert(person.github().to_string(), person);
            Ok(())
        })?;

        data.load_dir("teams", false, |this, _dir, team: Team, _path| {
            this.teams.insert(team.name().to_string(), team);
            Ok(())
        })?;

        data.load_dir("teams/archive", false, |this, _dir, team: Team, _path| {
            this.archived_teams.push(team);
            Ok(())
        })?;

        Ok(data)
    }

    fn load_dir<P, T, F>(&mut self, dir: P, nested: bool, f: F) -> Result<(), Error>
    where
        P: AsRef<Path>,
        T: DeserializeOwned,
        F: Fn(&mut Self, &str, T, &Path) -> Result<(), Error>,
        F: Clone,
    {
        for entry in std::fs::read_dir(&dir).with_context(|| {
            let dir = dir.as_ref().display();
            format!("`load_dir` failed to read directory '{}'", dir)
        })? {
            let path = entry?.path();
            if nested && path.is_dir() {
                self.load_dir(&path, false, f.clone())?;
            } else if !nested && path.is_file() && path.extension() == Some(OsStr::new("toml")) {
                fn dir(path: &Path) -> Option<&str> {
                    path.parent()?.file_name()?.to_str()
                }
                f(self, dir(&path).unwrap(), load_file(&path)?, &path)?;
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

    pub(crate) fn subteams_of<'a>(
        &'a self,
        team_name: &'a str,
    ) -> impl Iterator<Item = &Team> + 'a {
        self.team(team_name).into_iter().flat_map(move |team| {
            self.teams()
                .filter(move |maybe_subteam| team.is_parent_of(self, maybe_subteam))
        })
    }

    pub(crate) fn person(&self, name: &str) -> Option<&Person> {
        self.people.get(name)
    }

    pub(crate) fn people(&self) -> impl Iterator<Item = &Person> {
        self.people.values()
    }

    pub(crate) fn active_members(&self) -> Result<HashSet<&str>, Error> {
        let mut active = HashSet::new();
        for team in self.teams.values().filter(|team| !team.is_alumni_team()) {
            active.extend(team.members(self)?)
        }
        Ok(active)
    }

    pub(crate) fn repos(&self) -> impl Iterator<Item = &Repo> {
        self.repos.iter()
    }

    pub(crate) fn archived_repos(&self) -> impl Iterator<Item = &Repo> {
        self.archived_repos.iter()
    }

    pub(crate) fn all_repos(&self) -> impl Iterator<Item = &Repo> {
        self.repos().chain(self.archived_repos())
    }

    pub(crate) fn archived_teams(&self) -> impl Iterator<Item = &Team> {
        self.archived_teams.iter()
    }

    /// All the configured GitHub teams in the a hashset of (org, team_name) tuples.
    pub(crate) fn github_teams(&self) -> HashSet<(String, String)> {
        let mut result = HashSet::new();
        for team in self.teams() {
            for github_team in team.github_teams(self).unwrap_or_default() {
                result.insert((github_team.org.to_owned(), github_team.name.to_owned()));
            }
        }
        result
    }
}

fn load_file<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let parsed =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(parsed)
}
