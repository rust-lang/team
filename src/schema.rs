use crate::data::Data;
pub(crate) use crate::permissions::Permissions;
use failure::{bail, err_msg, Error};
use std::collections::HashSet;

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Config {
    allowed_mailing_lists_domains: HashSet<String>,
    allowed_github_orgs: HashSet<String>,
}

impl Config {
    pub(crate) fn allowed_mailing_lists_domains(&self) -> &HashSet<String> {
        &self.allowed_mailing_lists_domains
    }

    pub(crate) fn allowed_github_orgs(&self) -> &HashSet<String> {
        &self.allowed_github_orgs
    }
}

// This is an enum to allow two kinds of values for the email field:
//   email = false
//   email = "foo@example.com"
#[derive(serde_derive::Deserialize, Debug)]
#[serde(untagged)]
enum EmailField {
    Disabled(bool),
    Explicit(Option<String>),
}

impl Default for EmailField {
    fn default() -> Self {
        EmailField::Explicit(None)
    }
}

pub(crate) enum Email<'a> {
    Missing,
    Disabled,
    Present(&'a str),
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Person {
    name: String,
    github: String,
    github_id: usize,
    zulip_id: Option<usize>,
    irc: Option<String>,
    #[serde(default)]
    email: EmailField,
    discord: Option<String>,
    #[serde(default)]
    permissions: Permissions,
}

impl Person {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn github(&self) -> &str {
        &self.github
    }

    pub(crate) fn github_id(&self) -> usize {
        self.github_id
    }

    pub(crate) fn zulip_id(&self) -> Option<usize> {
        self.zulip_id
    }

    #[allow(unused)]
    pub(crate) fn irc(&self) -> &str {
        if let Some(irc) = &self.irc {
            irc
        } else {
            &self.github
        }
    }

    pub(crate) fn email(&self) -> Email {
        match &self.email {
            EmailField::Disabled(false) => Email::Disabled,
            EmailField::Disabled(true) => Email::Missing,
            EmailField::Explicit(None) => Email::Missing,
            EmailField::Explicit(Some(addr)) => Email::Present(addr.as_str()),
        }
    }

    pub(crate) fn discord(&self) -> Option<&str> {
        self.discord.as_deref()
    }

    pub(crate) fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if let EmailField::Disabled(true) = &self.email {
            bail!("`email = true` is not valid (for person {})", self.github);
        }
        Ok(())
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Team {
    name: String,
    #[serde(default = "default_false")]
    wg: bool,
    #[serde(default = "default_false")]
    marker_team: bool,
    subteam_of: Option<String>,
    people: TeamPeople,
    #[serde(default)]
    permissions: Permissions,
    github: Option<GitHubData>,
    rfcbot: Option<RfcbotData>,
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

    pub(crate) fn is_marker_team(&self) -> bool {
        self.marker_team
    }

    pub(crate) fn subteam_of(&self) -> Option<&str> {
        self.subteam_of.as_deref()
    }

    pub(crate) fn leads(&self) -> HashSet<&str> {
        self.people.leads.iter().map(|s| s.as_str()).collect()
    }

    pub(crate) fn rfcbot_data(&self) -> Option<&RfcbotData> {
        self.rfcbot.as_ref()
    }

    pub(crate) fn website_data(&self) -> Option<&WebsiteData> {
        self.website.as_ref()
    }

    pub(crate) fn members<'a>(&'a self, data: &'a Data) -> Result<HashSet<&'a str>, Error> {
        let mut members: HashSet<_> = self.people.members.iter().map(|s| s.as_str()).collect();
        if self.people.include_team_leads || self.people.include_wg_leads {
            for team in data.teams() {
                let include_wg = team.is_wg() && self.people.include_wg_leads;
                let include_team = !team.is_wg() && self.people.include_team_leads;
                if team.name != self.name && (include_wg || include_team) {
                    for lead in team.leads() {
                        members.insert(lead);
                    }
                }
            }
        }
        if self.people.include_all_team_members {
            for team in data.teams() {
                if team.is_wg() || team.is_marker_team() || team.name == self.name {
                    continue;
                }
                for member in team.members(data)? {
                    members.insert(member);
                }
            }
        }
        if self.people.include_all_alumni {
            for team in data.teams() {
                for person in &team.people.alumni {
                    members.insert(&person);
                }
            }
        }
        Ok(members)
    }

    pub(crate) fn alumni(&self) -> &[String] {
        &self.people.alumni
    }

    pub(crate) fn raw_lists(&self) -> &[TeamList] {
        &self.lists
    }

    pub(crate) fn lists(&self, data: &Data) -> Result<Vec<List>, Error> {
        let mut lists = Vec::new();
        for raw_list in &self.lists {
            let mut list = List {
                address: raw_list.address.clone(),
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
                if let Email::Present(email) = member.email() {
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

    pub(crate) fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    pub(crate) fn github_teams<'a>(&'a self, data: &Data) -> Result<Vec<GitHubTeam<'a>>, Error> {
        if let Some(github) = &self.github {
            let mut members = self
                .members(data)?
                .iter()
                .filter_map(|name| data.person(name).map(|p| p.github_id()))
                .collect::<Vec<_>>();
            for team in &github.extra_teams {
                members.extend(
                    data.team(team)
                        .ok_or_else(|| failure::err_msg(format!("missing team {}", team)))?
                        .members(data)?
                        .iter()
                        .filter_map(|name| data.person(name).map(|p| p.github_id())),
                );
            }
            let name = github.team_name.as_deref().unwrap_or(&self.name);
            Ok(github
                .orgs
                .iter()
                .map(|org| GitHubTeam {
                    org: org.as_str(),
                    name,
                    members: members.clone(),
                })
                .collect())
        } else {
            Ok(Vec::new())
        }
    }
}

#[derive(Eq, PartialEq)]
pub(crate) struct GitHubTeam<'a> {
    pub(crate) org: &'a str,
    pub(crate) name: &'a str,
    pub(crate) members: Vec<usize>,
}

impl std::cmp::PartialOrd for GitHubTeam<'_> {
    fn partial_cmp(&self, other: &GitHubTeam) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for GitHubTeam<'_> {
    fn cmp(&self, other: &GitHubTeam) -> std::cmp::Ordering {
        self.org.cmp(&other.org).then(self.name.cmp(&other.name))
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct TeamPeople {
    leads: Vec<String>,
    members: Vec<String>,
    #[serde(default)]
    alumni: Vec<String>,
    #[serde(default = "default_false")]
    include_team_leads: bool,
    #[serde(default = "default_false")]
    include_wg_leads: bool,
    #[serde(default = "default_false")]
    include_all_team_members: bool,
    #[serde(default = "default_false")]
    include_all_alumni: bool,
}

#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct GitHubData {
    team_name: Option<String>,
    orgs: Vec<String>,
    #[serde(default)]
    extra_teams: Vec<String>,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct RfcbotData {
    pub(crate) label: String,
    pub(crate) name: String,
    pub(crate) ping: String,
    #[serde(default)]
    pub(crate) exclude_members: Vec<String>,
}

pub(crate) struct DiscordInvite<'a> {
    pub(crate) url: &'a str,
    pub(crate) channel: &'a str,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct WebsiteData {
    name: String,
    description: String,
    page: Option<String>,
    email: Option<String>,
    repo: Option<String>,
    discord_invite: Option<String>,
    discord_name: Option<String>,
    #[serde(default)]
    weight: i64,
}

impl WebsiteData {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    pub(crate) fn weight(&self) -> i64 {
        self.weight
    }

    pub(crate) fn page(&self) -> Option<&str> {
        self.page.as_deref()
    }

    pub(crate) fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    pub(crate) fn repo(&self) -> Option<&str> {
        self.repo.as_deref()
    }

    pub(crate) fn discord(&self) -> Option<DiscordInvite> {
        if let (Some(url), Some(channel)) = (&self.discord_invite, &self.discord_name) {
            Some(DiscordInvite {
                url: url.as_ref(),
                channel: channel.as_ref(),
            })
        } else {
            None
        }
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct TeamList {
    pub(crate) address: String,
    #[serde(default = "default_true")]
    pub(crate) include_team_members: bool,
    #[serde(default)]
    pub(crate) extra_people: Vec<String>,
    #[serde(default)]
    pub(crate) extra_emails: Vec<String>,
    #[serde(default)]
    pub(crate) extra_teams: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct List {
    address: String,
    emails: Vec<String>,
}

impl List {
    pub(crate) fn address(&self) -> &str {
        &self.address
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
