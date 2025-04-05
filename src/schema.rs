use crate::data::Data;
pub(crate) use crate::permissions::Permissions;
use anyhow::{bail, format_err, Error};
use serde::de::{Deserialize, Deserializer};
use serde_untagged::UntaggedEnumVisitor;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Config {
    allowed_mailing_lists_domains: HashSet<String>,
    allowed_github_orgs: HashSet<String>,
    permissions_bors_repos: HashSet<String>,
    permissions_bools: HashSet<String>,
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
    github_id: u64,
    zulip_id: Option<u64>,
    irc: Option<String>,
    #[serde(default)]
    email: EmailField,
    discord_id: Option<u64>,
    matrix: Option<String>,
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

    pub(crate) fn github_id(&self) -> u64 {
        self.github_id
    }

    pub(crate) fn zulip_id(&self) -> Option<u64> {
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

    pub(crate) fn discord_id(&self) -> Option<u64> {
        self.discord_id
    }

    #[allow(unused)]
    pub(crate) fn matrix(&self) -> Option<&str> {
        self.matrix.as_deref()
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
    top_level: Option<bool>,
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
    roles: Vec<MemberRole>,
    #[serde(default)]
    lists: Vec<TeamList>,
    #[serde(default)]
    zulip_groups: Vec<RawZulipGroup>,
    #[serde(default)]
    zulip_streams: Vec<RawZulipStream>,
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

    pub(crate) fn top_level(&self) -> Option<bool> {
        self.top_level
    }

    // Return's whether the provided team is a subteam of this team
    pub(crate) fn is_parent_of<'a>(&'a self, data: &'a Data, subteam: &Team) -> bool {
        let mut visited = Vec::new();
        let mut subteam = Some(subteam);
        while let Some(team) = subteam {
            // Get subteam's parent
            let Some(parent) = team.subteam_of() else {
                // The current subteam is a top level team.
                // Therefore this team cannot be its parent.
                return false;
            };
            // If the parent is this team, return true
            if parent == self.name {
                return true;
            }

            visited.push(team.name.as_str());

            // Otherwise try the test again with the parent
            // unless we have already visited it.

            if visited.contains(&parent) {
                // We have found a cycle, give up.
                return false;
            }
            subteam = data.team(parent);
        }
        false
    }

    pub(crate) fn leads(&self) -> BTreeSet<&str> {
        self.people.leads.iter().map(|s| s.as_str()).collect()
    }

    pub(crate) fn rfcbot_data(&self) -> Option<&RfcbotData> {
        self.rfcbot.as_ref()
    }

    pub(crate) fn website_data(&self) -> Option<&WebsiteData> {
        self.website.as_ref()
    }

    pub(crate) fn roles(&self) -> &[MemberRole] {
        &self.roles
    }

    pub(crate) fn discord_roles(&self) -> Option<&Vec<DiscordRole>> {
        self.discord_roles.as_ref()
    }

    /// Exposed only for validation.
    pub(crate) fn raw_people(&self) -> &TeamPeople {
        &self.people
    }

    pub(crate) fn members<'a>(&'a self, data: &'a Data) -> Result<HashSet<&'a str>, Error> {
        let mut members: HashSet<_> = self
            .people
            .members
            .iter()
            .map(|s| s.github.as_str())
            .collect();

        for team in &self.people.included_teams {
            let team = data.team(team).ok_or_else(|| {
                format_err!(
                    "team '{}' includes members from non-existent team '{}'",
                    self.name(),
                    team
                )
            })?;
            members.extend(team.members(data)?);
        }
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
                members.extend(team.members(data)?);
            }
        }
        if self.is_alumni_team() {
            let active_members = data.active_members()?;
            let alumni = data
                .teams()
                .chain(data.archived_teams())
                .flat_map(|t| t.explicit_alumni())
                .map(|a| a.github.as_str())
                .filter(|person| !active_members.contains(person));
            members.extend(alumni);
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
                emails: Vec::new(),
            };

            let mut members = if raw_list.include_team_members {
                self.members(data)?
            } else {
                HashSet::new()
            };
            if raw_list.include_subteam_members {
                for subteam in data.subteams_of(&self.name) {
                    members.extend(subteam.members(data)?);
                }
            }
            for person in &raw_list.extra_people {
                members.insert(person.as_str());
            }
            for team in &raw_list.extra_teams {
                let team = data
                    .team(team)
                    .ok_or_else(|| format_err!("team {} is missing", team))?;
                members.extend(team.members(data)?);
            }

            for member in members.iter() {
                let member = data
                    .person(member)
                    .ok_or_else(|| format_err!("member {} is missing", member))?;
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

    /// `on_exclude_not_included` is a function that is returned when an excluded member
    /// wasn't included.
    fn expand_zulip_membership(
        &self,
        data: &Data,
        common: &RawZulipCommon,
        on_exclude_not_included: impl Fn(&str) -> Error,
    ) -> Result<Vec<ZulipMember>, Error> {
        let mut members = if common.include_team_members {
            self.members(data)?
        } else {
            HashSet::new()
        };
        for person in &common.extra_people {
            members.insert(person.as_str());
        }
        for team in &common.extra_teams {
            let team = data
                .team(team)
                .ok_or_else(|| format_err!("team {} is missing", team))?;
            members.extend(team.members(data)?);
        }
        for excluded in &common.excluded_people {
            if !members.remove(excluded.as_str()) {
                return Err(on_exclude_not_included(excluded));
            }
        }

        let mut final_members = Vec::new();
        for member in members.iter() {
            let member = data
                .person(member)
                .ok_or_else(|| format_err!("{} does not have a person configuration", member))?;
            let member = match (member.github.clone(), member.zulip_id) {
                (github, Some(zulip_id)) => ZulipMember::MemberWithId { github, zulip_id },
                (github, _) => ZulipMember::MemberWithoutId { github },
            };
            final_members.push(member);
        }
        for &extra in &common.extra_zulip_ids {
            final_members.push(ZulipMember::JustId(extra));
        }
        Ok(final_members)
    }

    pub(crate) fn raw_zulip_groups(&self) -> &[RawZulipGroup] {
        &self.zulip_groups
    }

    pub(crate) fn zulip_groups(&self, data: &Data) -> Result<Vec<ZulipGroup>, Error> {
        let mut groups = Vec::new();
        let zulip_groups = &self.zulip_groups;

        for raw_group in zulip_groups {
            groups.push(ZulipGroup(ZulipCommon {
                name: raw_group.common.name.clone(),
                includes_team_members: raw_group.common.include_team_members,
                members: self.expand_zulip_membership(
                    data,
                    &raw_group.common,
                    |excluded| {
                        format_err!("'{excluded}' was specifically excluded from the Zulip group '{}' but they were already not included", raw_group.common.name)
                    },
                )?,
            }));
        }
        Ok(groups)
    }

    pub(crate) fn raw_zulip_streams(&self) -> &[RawZulipStream] {
        &self.zulip_streams
    }

    pub(crate) fn zulip_streams(&self, data: &Data) -> Result<Vec<ZulipStream>, Error> {
        let mut streams = Vec::new();
        let zulip_streams = self.raw_zulip_streams();

        for raw_stream in zulip_streams {
            streams.push(ZulipStream(ZulipCommon {
                name: raw_stream.common.name.clone(),
                includes_team_members: raw_stream.common.include_team_members,
                members: self.expand_zulip_membership(
                    data,
                    &raw_stream.common,
                    |excluded| {
                        format_err!("'{excluded}' was specifically excluded from the Zulip stream '{}' but they were already not included", raw_stream.common.name)
                    },
                )?,
            }));
        }
        Ok(streams)
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
                        .ok_or_else(|| format_err!("missing team {}", team))?
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

    pub(crate) fn discord_ids(&self, data: &Data) -> Result<Vec<u64>, Error> {
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
    pub(crate) fn explicit_members(&self) -> &[TeamMember] {
        &self.people.members
    }

    pub(crate) fn explicit_alumni(&self) -> &[TeamMember] {
        self.people.alumni.as_ref().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn contains_person(&self, data: &Data, person: &Person) -> Result<bool, Error> {
        let members = self.members(data)?;
        Ok(members.contains(person.github()))
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

#[derive(Eq, PartialEq)]
pub(crate) struct GitHubTeam<'a> {
    pub(crate) org: &'a str,
    pub(crate) name: &'a str,
    pub(crate) members: Vec<(&'a str, u64)>,
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
pub(crate) struct TeamPeople {
    pub leads: Vec<String>,
    pub members: Vec<TeamMember>,
    pub alumni: Option<Vec<TeamMember>>,
    #[serde(default)]
    pub included_teams: Vec<String>,
    #[serde(default = "default_false")]
    pub include_team_leads: bool,
    #[serde(default = "default_false")]
    pub include_wg_leads: bool,
    #[serde(default = "default_false")]
    pub include_project_group_leads: bool,
    #[serde(default = "default_false")]
    pub include_all_team_members: bool,
    #[serde(default = "default_false")]
    pub include_all_alumni: bool,
}

#[derive(serde::Deserialize, Clone, Debug)]
#[serde(remote = "Self", deny_unknown_fields)]
pub(crate) struct TeamMember {
    pub github: String,
    pub roles: Vec<String>,
}

impl<'de> Deserialize<'de> for TeamMember {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        UntaggedEnumVisitor::new()
            .string(|github| {
                Ok(TeamMember {
                    github: github.to_owned(),
                    roles: Vec::new(),
                })
            })
            .map(|map| {
                let deserializer = serde::de::value::MapAccessDeserializer::new(map);
                TeamMember::deserialize(deserializer)
            })
            .deserialize(deserializer)
    }
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
    matrix_room: Option<String>,
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

    pub(crate) fn matrix_room(&self) -> Option<&str> {
        self.matrix_room.as_deref()
    }
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberRole {
    pub id: String,
    pub description: String,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct TeamList {
    pub(crate) address: String,
    #[serde(default = "default_true")]
    pub(crate) include_team_members: bool,
    #[serde(default)]
    pub(crate) include_subteam_members: bool,
    #[serde(default)]
    pub(crate) extra_people: Vec<String>,
    #[serde(default)]
    pub(crate) extra_emails: Vec<String>,
    #[serde(default)]
    pub(crate) extra_teams: Vec<String>,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct RawZulipCommon {
    pub(crate) name: String,
    #[serde(default = "default_true")]
    pub(crate) include_team_members: bool,
    #[serde(default)]
    pub(crate) extra_people: Vec<String>,
    #[serde(default)]
    pub(crate) extra_zulip_ids: Vec<u64>,
    #[serde(default)]
    pub(crate) extra_teams: Vec<String>,
    #[serde(default)]
    pub(crate) excluded_people: Vec<String>,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct RawZulipGroup {
    #[serde(flatten)]
    pub(crate) common: RawZulipCommon,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct RawZulipStream {
    #[serde(flatten)]
    pub(crate) common: RawZulipCommon,
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
pub(crate) struct ZulipCommon {
    name: String,
    includes_team_members: bool,
    members: Vec<ZulipMember>,
}

impl ZulipCommon {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Whether the group/stream includes the members of the associated team?
    pub(crate) fn includes_team_members(&self) -> bool {
        self.includes_team_members
    }

    pub(crate) fn members(&self) -> &[ZulipMember] {
        &self.members
    }
}

#[derive(Debug)]
pub(crate) struct ZulipGroup(ZulipCommon);

impl std::ops::Deref for ZulipGroup {
    type Target = ZulipCommon;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug)]
pub(crate) struct ZulipStream(ZulipCommon);

impl std::ops::Deref for ZulipStream {
    type Target = ZulipCommon;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub(crate) enum ZulipMember {
    MemberWithId { github: String, zulip_id: u64 },
    JustId(u64),
    MemberWithoutId { github: String },
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
    pub homepage: Option<String>,
    #[serde(default)]
    pub private_non_synced: Option<bool>,
    pub bots: Vec<Bot>,
    pub access: RepoAccess,
    #[serde(default)]
    pub branch_protections: Vec<BranchProtection>,
}

#[derive(serde_derive::Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Bot {
    Bors,
    Highfive,
    Rustbot,
    RustTimer,
    Rfcbot,
    Craterbot,
    Glacierbot,
    LogAnalyzer,
    Renovate,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RepoAccess {
    pub teams: HashMap<String, RepoPermission>,
    #[serde(default)]
    pub individuals: HashMap<String, RepoPermission>,
}

#[derive(serde_derive::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) enum RepoPermission {
    Triage,
    Write,
    Maintain,
    Admin,
}

#[derive(serde_derive::Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MergeBot {
    Homu,
}

#[derive(serde_derive::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct BranchProtection {
    pub pattern: String,
    #[serde(default)]
    pub ci_checks: Vec<String>,
    #[serde(default)]
    pub dismiss_stale_review: bool,
    #[serde(default)]
    pub required_approvals: Option<u32>,
    #[serde(default = "default_true")]
    pub pr_required: bool,
    #[serde(default)]
    pub allowed_merge_teams: Vec<String>,
    #[serde(default)]
    pub merge_bots: Vec<MergeBot>,
}
