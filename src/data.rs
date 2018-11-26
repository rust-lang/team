use crate::schema::{Person, Team, List};
use failure::{Error, ResultExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::OsStr;

#[derive(Debug)]
pub(crate) struct Data {
    people: HashMap<String, Person>,
    teams: HashMap<String, Team>,
}

impl Data {
    pub(crate) fn load() -> Result<Self, Error> {
        let mut data = Data {
            people: HashMap::new(),
            teams: HashMap::new(),
        };

        data.load_dir("people", |this, person: Person| {
            this.people.insert(person.github().to_string(), person);
        })?;

        data.load_dir("teams", |this, team: Team| {
            this.teams.insert(team.name().to_string(), team);
        })?;

        Ok(data)
    }

    fn load_dir<T, F>(&mut self, dir: &str, f: F) -> Result<(), Error>
    where
        T: for<'de> Deserialize<'de>,
        F: Fn(&mut Self, T),
    {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_file() && path.extension() == Some(OsStr::new("toml")) {
                let content = std::fs::read(&path)
                    .with_context(|_| format!("failed to read {}", path.display()))?;
                let parsed: T = toml::from_slice(&content)
                    .with_context(|_| format!("failed to parse {}", path.display()))?;
                f(self, parsed);
            }
        }

        Ok(())
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
}
