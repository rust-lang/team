use crate::data::Data;
use failure::{Error, err_msg};
use std::collections::HashSet;

#[derive(serde_derive::Deserialize, Debug)]
pub(crate) struct Person {
    name: String,
    github: String,
    irc: Option<String>,
}

impl Person {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn github(&self) -> &str {
        &self.github
    }

    pub(crate) fn irc(&self) -> &str {
        if let Some(irc) = &self.irc {
            irc
        } else {
            &self.github
        }
    }
}

#[derive(serde_derive::Deserialize, Debug)]
pub(crate) struct Team {
    name: String,
    #[serde(default)]
    children: Vec<String>,
    people: TeamPeople,
}

impl Team {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn leads(&self) -> HashSet<&str> {
        self.people.leads.iter().map(|s| s.as_str()).collect()
    }

    pub(crate) fn members<'a>(&'a self, data: &'a Data) -> Result<HashSet<&'a str>, Error> {
        let mut members: HashSet<_> = self.people.members.iter().map(|s| s.as_str()).collect();
        for subteam in &self.children {
            let submembers = data
                .team(&subteam)
                .ok_or_else(|| err_msg(format!("missing team {}", subteam)))?;
            for person in submembers.members(data)? {
                members.insert(person);
            }
        }
        Ok(members)
    }
}

#[derive(serde_derive::Deserialize, Debug)]
struct TeamPeople {
    leads: Vec<String>,
    members: Vec<String>,
}
