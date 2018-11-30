use crate::data::Data;
use failure::{err_msg, Error};
use std::collections::HashSet;

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Config {
    allowed_mailing_lists_domains: HashSet<String>,
}

impl Config {
    pub(crate) fn allowed_mailing_lists_domains(&self) -> &HashSet<String> {
        &self.allowed_mailing_lists_domains
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Person {
    name: String,
    github: String,
    irc: Option<String>,
    email: Option<String>,
    discord: Option<String>,
}

impl Person {
    #[allow(unused)]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn github(&self) -> &str {
        &self.github
    }

    #[allow(unused)]
    pub(crate) fn irc(&self) -> &str {
        if let Some(irc) = &self.irc {
            irc
        } else {
            &self.github
        }
    }

    pub(crate) fn email(&self) -> Option<&str> {
        self.email.as_ref().map(|e| e.as_str())
    }

    pub(crate) fn discord(&self) -> Option<&str> {
        self.discord.as_ref().map(|e| e.as_str())
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Team {
    name: String,
    #[serde(default = "default_false")]
    wg: bool,
    #[serde(default)]
    children: Vec<String>,
    people: TeamPeople,
    website: Option<WebsiteData>,
    #[serde(default)]
    lists: Vec<TeamList>,
}

impl Team {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn is_wg(&self) -> bool {
        self.wg
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
        if self.people.include_team_leads || self.people.include_wg_leads {
            for team in data.teams() {
                let include_wg = team.is_wg() && self.people.include_wg_leads;
                let include_team = !team.is_wg() && self.people.include_team_leads;
                if include_wg || include_team {
                    for lead in team.leads() {
                        members.insert(lead);
                    }
                }
            }
        }
        Ok(members)
    }

    pub(crate) fn raw_lists(&self) -> &[TeamList] {
        &self.lists
    }

    pub(crate) fn lists(&self, data: &Data) -> Result<Vec<List>, Error> {
        let mut lists = Vec::new();
        for raw_list in &self.lists {
            let mut list = List {
                address: raw_list.address.clone(),
                access_level: raw_list.access_level,
                emails: Vec::new(),
            };

            let mut members = if raw_list.include_team_members {
                self.members(data)?
            } else {
                HashSet::new()
            };
            for person in &raw_list.extra_people {
                members.insert(person.as_str());
            }
            for team in &raw_list.extra_teams {
                let team = data
                    .team(team)
                    .ok_or_else(|| err_msg(format!("team {} is missing", team)))?;
                for member in team.members(data)? {
                    members.insert(member);
                }
            }

            for member in members.iter() {
                let member = data
                    .person(member)
                    .ok_or_else(|| err_msg(format!("member {} is missing", member)))?;
                if let Some(email) = member.email() {
                    list.emails.push(email.to_string());
                }
            }
            for extra in &raw_list.extra_emails {
                list.emails.push(extra.to_string());
            }
            lists.push(list);
        }
        Ok(lists)
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct TeamPeople {
    leads: Vec<String>,
    members: Vec<String>,
    #[serde(default = "default_false")]
    include_team_leads: bool,
    #[serde(default = "default_false")]
    include_wg_leads: bool,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct WebsiteData {
    name: String,
    description: String,
    email: Option<String>,
    repo: Option<String>,
    discord_invite: Option<String>,
    discord_name: Option<String>,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct TeamList {
    pub(crate) address: String,
    pub(crate) access_level: ListAccessLevel,
    #[serde(default = "default_true")]
    pub(crate) include_team_members: bool,
    #[serde(default)]
    pub(crate) extra_people: Vec<String>,
    #[serde(default)]
    pub(crate) extra_emails: Vec<String>,
    #[serde(default)]
    pub(crate) extra_teams: Vec<String>,
}

#[derive(serde_derive::Deserialize, Debug, Copy, Clone)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ListAccessLevel {
    Everyone,
    Members,
    ReadOnly,
}

#[derive(Debug)]
pub(crate) struct List {
    address: String,
    access_level: ListAccessLevel,
    emails: Vec<String>,
}

impl List {
    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    pub(crate) fn access_level_str(&self) -> &str {
        match self.access_level {
            ListAccessLevel::Everyone => "everyone",
            ListAccessLevel::Members => "members",
            ListAccessLevel::ReadOnly => "readonly",
        }
    }

    pub(crate) fn emails(&self) -> &[String] {
        &self.emails
    }
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}
