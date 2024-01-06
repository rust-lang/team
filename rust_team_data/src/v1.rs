use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub static BASE_URL: &str = "https://team-api.infra.rust-lang.org/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TeamKind {
    Team,
    WorkingGroup,
    ProjectGroup,
    MarkerTeam,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Team {
    pub name: String,
    pub kind: TeamKind,
    pub subteam_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_level: Option<bool>,
    pub members: Vec<TeamMember>,
    pub alumni: Vec<TeamMember>,
    pub github: Option<TeamGitHub>,
    pub website_data: Option<TeamWebsite>,
    pub roles: Vec<MemberRole>,
    pub discord: Vec<TeamDiscord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamMember {
    pub name: String,
    pub github: String,
    pub github_id: u64,
    pub is_lead: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamGitHub {
    pub teams: Vec<GitHubTeam>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitHubTeam {
    pub org: String,
    pub name: String,
    pub members: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamWebsite {
    pub name: String,
    pub description: String,
    pub page: String,
    pub email: Option<String>,
    pub repo: Option<String>,
    pub discord: Option<DiscordInvite>,
    pub zulip_stream: Option<String>,
    pub matrix_room: Option<String>,
    pub weight: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemberRole {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamDiscord {
    pub name: String,
    pub members: Vec<u64>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscordInvite {
    pub channel: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Teams {
    #[serde(flatten)]
    pub teams: IndexMap<String, Team>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Repos {
    #[serde(flatten)]
    pub repos: IndexMap<String, Vec<Repo>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct List {
    pub address: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Lists {
    pub lists: IndexMap<String, List>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZulipGroup {
    pub name: String,
    pub members: Vec<ZulipGroupMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ZulipGroupMember {
    // TODO(rylev): this variant can be removed once
    // it is verified that noone is relying on it
    Email(String),
    Id(u64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZulipGroups {
    pub groups: IndexMap<String, ZulipGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Permission {
    pub people: Vec<PermissionPerson>,
    pub github_users: Vec<String>,
    pub github_ids: Vec<u64>,
    pub discord_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PermissionPerson {
    pub github_id: u64,
    pub github: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rfcbot {
    pub teams: IndexMap<String, RfcbotTeam>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RfcbotTeam {
    pub name: String,
    pub ping: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZulipMapping {
    /// Zulip ID to GitHub ID
    pub users: IndexMap<u64, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Repo {
    pub org: String,
    pub name: String,
    pub description: String,
    pub homepage: Option<String>,
    pub bots: Vec<Bot>,
    pub teams: Vec<RepoTeam>,
    pub members: Vec<RepoMember>,
    pub branch_protections: Vec<BranchProtection>,
    pub archived: bool,
    // This attribute is not synced by sync-team.
    pub private: bool,
    // Is the GitHub "Auto-merge" option enabled?
    // https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/incorporating-changes-from-a-pull-request/automatically-merging-a-pull-request
    pub auto_merge_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Bot {
    Bors,
    Highfive,
    Rustbot,
    RustTimer,
    Rfcbot,
    Renovate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoTeam {
    pub name: String,
    pub permission: RepoPermission,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoMember {
    pub name: String,
    pub permission: RepoPermission,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RepoPermission {
    Write,
    Admin,
    Maintain,
    Triage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BranchProtection {
    pub pattern: String,
    pub ci_checks: Vec<String>,
    pub dismiss_stale_review: bool,
    pub required_approvals: u32,
    pub allowed_merge_teams: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Person {
    pub name: String,
    pub email: Option<String>,
    pub github_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct People {
    /// GitHub name as key.
    pub people: IndexMap<String, Person>,
}
