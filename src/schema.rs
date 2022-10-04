use crate::data::Data;
pub(crate) use crate::permissions::Permissions;
use failure::{bail, err_msg, Error};
use std::collections::{HashMap, HashSet};

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Config {
    allowed_mailing_lists_domains: HashSet<String>,
    allowed_github_orgs: HashSet<String>,
    permissions_bors_repos: HashSet<String>,
    permissions_bools: HashSet<String>,
    permissions_crates_io_ops_bot_apps: HashSet<String>,
}

impl Config {
    pub(crate) fn allowed_mailing_lists_domains(&self) -> &HashSet<String> {
        &self.allowed_mailing_lists_domains
    }

    pub(crate) fn allowed_github_orgs(&self) -> &HashSet<String> {
        &self.allowed_github_orgs
    }

    pub(crate) fn permissions_bors_repos(&self) -> &HashSet<String> {
        &self.permissions_bors_repos
    }

    pub(crate) fn permissions_bools(&self) -> &HashSet<String> {
        &self.permissions_bools
    }

    pub(crate) fn permissions_crates_io_ops_bot_apps(&self) -> &HashSet<String> {
        &self.permissions_crates_io_ops_bot_apps
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
    discord_id: Option<usize>,
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

    pub(crate) fn discord_id(&self) -> Option<usize> {
        self.discord_id
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

#[derive(serde_derive::Deserialize, Debug, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TeamKind {
    Team,
    WorkingGroup,
    ProjectGroup,
    MarkerTeam,
}

impl std::fmt::Display for TeamKind {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Team => "team",
                Self::WorkingGroup => "working group",
                Self::ProjectGroup => "project group",
                Self::MarkerTeam => "marker team",
            }
        )
    }
}

impl Default for TeamKind {
    fn default() -> Self {
        Self::Team
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Team {
    name: String,
    #[serde(default)]
    kind: TeamKind,
    subteam_of: Option<String>,
    people: TeamPeople,
    #[serde(default)]
    permissions: Permissions,
    #[serde(default)]
    leads_permissions: Permissions,
    #[serde(default)]
    github: Vec<GitHubData>,
    rfcbot: Option<RfcbotData>,
    website: Option<WebsiteData>,
    #[serde(default)]
    lists: Vec<TeamList>,
    #[serde(default)]
    zulip_groups: Vec<RawZulipGroup>,
    discord_roles: Option<Vec<DiscordRole>>,
}

impl Team {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn kind(&self) -> TeamKind {
        self.kind
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

    pub(crate) fn discord_roles(&self) -> Option<&Vec<DiscordRole>> {
        self.discord_roles.as_ref()
    }

    pub(crate) fn members<'a>(&'a self, data: &'a Data) -> Result<HashSet<&'a str>, Error> {
        let mut members: HashSet<_> = self.people.members.iter().map(|s| s.as_str()).collect();

        let mut include_leads = |kind| {
            for team in data.teams() {
                if team.name != self.name && team.kind == kind {
                    for lead in team.leads() {
                        members.insert(lead);
                    }
                }
            }
        };
        if self.people.include_team_leads {
            include_leads(TeamKind::Team);
        }
        if self.people.include_wg_leads {
            include_leads(TeamKind::WorkingGroup);
        }
        if self.people.include_project_group_leads {
            include_leads(TeamKind::ProjectGroup);
        }

        if self.people.include_all_team_members {
            for team in data.teams() {
                if team.kind != TeamKind::Team
                    || team.name == self.name
                    // This matches the special alumni team.
                    || team.is_alumni_team()
                {
                    continue;
                }
                for member in team.members(data)? {
                    members.insert(member);
                }
            }
        }
        if self.is_alumni_team() {
            let active_members = data.active_members()?;
            let alumni = data
                .teams()
                .chain(data.archived_teams())
                .flat_map(|t| t.alumni())
                .map(|a| a.as_str());
            let members_of_archived_teams = data
                .archived_teams()
                .filter_map(|t| t.members(data).ok())
                .flat_map(|members| members);

            members.extend(
                alumni
                    .chain(members_of_archived_teams)
                    .filter(|person| !active_members.contains(person)),
            )
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

    pub(crate) fn raw_zulip_groups(&self) -> &[RawZulipGroup] {
        &self.zulip_groups
    }

    pub(crate) fn zulip_groups(&self, data: &Data) -> Result<Vec<ZulipGroup>, Error> {
        let mut groups = Vec::new();
        let zulip_groups = &self.zulip_groups;

        for raw_group in zulip_groups {
            let mut group = ZulipGroup {
                name: raw_group.name.clone(),
                includes_team_members: raw_group.include_team_members,
                members: Vec::new(),
            };

            let mut members = if raw_group.include_team_members {
                self.members(data)?
            } else {
                HashSet::new()
            };
            for person in &raw_group.extra_people {
                members.insert(person.as_str());
            }
            for team in &raw_group.extra_teams {
                let team = data
                    .team(team)
                    .ok_or_else(|| err_msg(format!("team {} is missing", team)))?;
                for member in team.members(data)? {
                    members.insert(member);
                }
            }
            for excluded in &raw_group.excluded_people {
                if !members.remove(excluded.as_str()) {
                    bail!("'{excluded}' was specifically excluded from the Zulip group '{}' but they were already not included", raw_group.name);
                }
            }

            for member in members.iter() {
                let member = data.person(member).ok_or_else(|| {
                    err_msg(format!("{} does not have a person configuration", member))
                })?;
                let member = match (member.zulip_id, member.email()) {
                    (Some(zulip_id), _) => ZulipGroupMember::Id(zulip_id),
                    (_, Email::Present(email)) => ZulipGroupMember::Email(email.to_string()),
                    _ => ZulipGroupMember::Missing,
                };
                group.members.push(member);
            }
            for &extra in &raw_group.extra_zulip_ids {
                group.members.push(ZulipGroupMember::Id(extra));
            }
            groups.push(group);
        }
        Ok(groups)
    }

    pub(crate) fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    pub(crate) fn leads_permissions(&self) -> &Permissions {
        &self.leads_permissions
    }

    pub(crate) fn github_teams<'a>(&'a self, data: &'a Data) -> Result<Vec<GitHubTeam<'a>>, Error> {
        let mut result = Vec::new();
        for github in &self.github {
            let mut members = self
                .members(data)?
                .iter()
                .filter_map(|name| data.person(name).map(|p| (p.github(), p.github_id())))
                .collect::<Vec<_>>();
            for team in &github.extra_teams {
                members.extend(
                    data.team(team)
                        .ok_or_else(|| failure::err_msg(format!("missing team {}", team)))?
                        .members(data)?
                        .iter()
                        .filter_map(|name| data.person(name).map(|p| (p.github(), p.github_id()))),
                );
            }
            members.sort_unstable();
            let name = github.team_name.as_deref().unwrap_or(&self.name);

            for org in &github.orgs {
                result.push(GitHubTeam {
                    org: org.as_str(),
                    name,
                    members: members.clone(),
                });
            }
        }
        Ok(result)
    }

    pub(crate) fn discord_ids(&self, data: &Data) -> Result<Vec<usize>, Error> {
        Ok(self
            .members(data)?
            .iter()
            .flat_map(|name| data.person(name).map(|p| p.discord_id()))
            .flatten()
            .collect())
    }

    pub(crate) fn is_alumni_team(&self) -> bool {
        self.people.include_all_alumni
    }

    // People explicitly set as members
    pub(crate) fn explicit_members(&self) -> &Vec<String> {
        &self.people.members
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct DiscordRole {
    name: String,
    color: Option<String>,
}

impl DiscordRole {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn color(&self) -> Option<&str> {
        self.color.as_ref().map(|s| &s[..])
    }
}

#[derive(Eq, PartialEq, Debug)]
pub(crate) struct DiscordTeam {
    pub(crate) members: Vec<usize>,
}

#[derive(Eq, PartialEq)]
pub(crate) struct GitHubTeam<'a> {
    pub(crate) org: &'a str,
    pub(crate) name: &'a str,
    pub(crate) members: Vec<(&'a str, usize)>,
}

impl std::cmp::PartialOrd for GitHubTeam<'_> {
    fn partial_cmp(&self, other: &GitHubTeam) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for GitHubTeam<'_> {
    fn cmp(&self, other: &GitHubTeam) -> std::cmp::Ordering {
        self.org.cmp(other.org).then(self.name.cmp(other.name))
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
    include_project_group_leads: bool,
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
    zulip_stream: Option<String>,
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

    pub(crate) fn zulip_stream(&self) -> Option<&str> {
        self.zulip_stream.as_deref()
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

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct RawZulipGroup {
    pub(crate) name: String,
    #[serde(default = "default_true")]
    pub(crate) include_team_members: bool,
    #[serde(default)]
    pub(crate) extra_people: Vec<String>,
    #[serde(default)]
    pub(crate) extra_zulip_ids: Vec<usize>,
    #[serde(default)]
    pub(crate) extra_teams: Vec<String>,
    #[serde(default)]
    pub(crate) excluded_people: Vec<String>,
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

#[derive(Debug)]
pub(crate) struct ZulipGroup {
    name: String,
    includes_team_members: bool,
    members: Vec<ZulipGroupMember>,
}

impl ZulipGroup {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Whether the group includes the members of the team its associated
    pub(crate) fn includes_team_members(&self) -> bool {
        self.includes_team_members
    }

    pub(crate) fn members(&self) -> &[ZulipGroupMember] {
        &self.members
    }
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub(crate) enum ZulipGroupMember {
    Id(usize),
    Email(String),
    Missing,
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Repo {
    pub org: String,
    pub name: String,
    pub description: String,
    pub bots: Vec<Bot>,
    pub access: RepoAccess,
    #[serde(rename = "branch", default)]
    pub branches: Vec<Branch>,
}

impl Repo {
    const VALID_ORGS: &'static [&'static str] = &["rust-lang"];

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if !Self::VALID_ORGS.contains(&self.org.as_str()) {
            bail!("{} is not a valid repo org", self.org);
        }

        Ok(())
    }
}

#[derive(serde_derive::Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Bot {
    Bors,
    Highfive,
    Rustbot,
    RustTimer,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RepoAccess {
    pub teams: HashMap<String, RepoPermission>,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) enum RepoPermission {
    Triage,
    Write,
    Maintain,
    Admin,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Branch {
    pub name: String,
    #[serde(default)]
    pub ci_checks: Vec<String>,
    #[serde(default)]
    pub dismiss_stale_review: bool,
}
