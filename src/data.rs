use crate::schema::{Config, List, Person, Team, ZulipGroup};
use failure::{Error, ResultExt};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;

#[derive(Debug)]
pub(crate) struct Data {
    people: HashMap<String, Person>,
    teams: HashMap<String, Team>,
    config: Config,
}

impl Data {
    pub(crate) fn load() -> Result<Self, Error> {
        let mut data = Data {
            people: HashMap::new(),
            teams: HashMap::new(),
            config: load_file(&Path::new("config.toml"))?,
        };

        data.load_dir("people", |this, person: Person| {
            person.validate()?;
            this.people.insert(person.github().to_string(), person);
            Ok(())
        })?;

        data.load_dir("teams", |this, team: Team| {
            this.teams.insert(team.name().to_string(), team);
            Ok(())
        })?;

        Ok(data)
    }

    fn load_dir<T, F>(&mut self, dir: &str, f: F) -> Result<(), Error>
    where
        T: for<'de> Deserialize<'de>,
        F: Fn(&mut Self, T) -> Result<(), Error>,
    {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_file() && path.extension() == Some(OsStr::new("toml")) {
                f(self, load_file(&path)?)?;
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
            for list in team.zulip_groups(self)? {
                groups.insert(list.name().to_string(), list);
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
}

fn load_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Error> {
    let content =
        std::fs::read(&path).with_context(|_| format!("failed to read {}", path.display()))?;
    let parsed = toml::from_slice(&content)
        .with_context(|_| format!("failed to parse {}", path.display()))?;
    Ok(parsed)
}
