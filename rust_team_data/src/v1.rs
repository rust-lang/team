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
pub struct ZulipStream {
    pub name: String,
    pub members: Vec<ZulipStreamMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ZulipStreamMember {
    // TODO(rylev): this variant can be removed once
    // it is verified that no one is relying on it
    Email(String),
    Id(u64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZulipStreams {
    pub streams: IndexMap<String, ZulipStream>,
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
    pub crates: Vec<Crate>,
    pub environments: IndexMap<String, Environment>,
    pub archived: bool,
    // This attribute is not synced by sync-team.
    pub private: bool,
    // Is the GitHub "Auto-merge" option enabled?
    // https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/incorporating-changes-from-a-pull-request/automatically-merging-a-pull-request
    pub auto_merge_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrateTeamOwner {
    pub org: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Crate {
    pub name: String,
    pub crates_io_publishing: Option<CratesIoPublishing>,
    pub trusted_publishing_only: bool,
    /// GitHub teams that have access to this crate on crates.io
    pub teams: Vec<CrateTeamOwner>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Bot {
    Bors,
    Highfive,
    Rustbot,
    RustTimer,
    Rfcbot,
    Craterbot,
    Glacierbot,
    LogAnalyzer,
    Renovate,
    HerokuDeployAccess,
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
#[serde(rename_all = "snake_case")]
pub enum BranchProtectionMode {
    PrRequired {
        ci_checks: Vec<String>,
        required_approvals: u32,
    },
    PrNotRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MergeBot {
    Homu,
    RustTimer,
    Bors,
    WorkflowsCratesIo,
    PromoteRelease,
}

impl MergeBot {
    pub fn app_id(&self) -> Option<i64> {
        match self {
            MergeBot::WorkflowsCratesIo => Some(2201425),
            MergeBot::Bors => Some(278306),
            MergeBot::PromoteRelease => Some(217112),
            // These are user-based bots, not GitHub Apps
            MergeBot::RustTimer | MergeBot::Homu => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionTarget {
    #[default]
    Branch,
    Tag,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MergeQueueMethod {
    #[default]
    Merge,
    Squash,
    Rebase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BranchProtection {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "is_branch_target")]
    pub target: ProtectionTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub dismiss_stale_review: bool,
    pub mode: BranchProtectionMode,
    pub allowed_merge_teams: Vec<String>,
    pub merge_bots: Vec<MergeBot>,
    pub allowed_merge_apps: Vec<MergeBot>,
    pub require_up_to_date_branches: bool,
    pub merge_queue: bool,
    #[serde(default, skip_serializing_if = "is_default_merge_queue_method")]
    pub merge_queue_method: MergeQueueMethod,
    #[serde(
        default = "default_merge_queue_max_entries_to_build",
        skip_serializing_if = "is_default_merge_queue_max_entries_to_build"
    )]
    pub merge_queue_max_entries_to_build: u32,
    #[serde(
        default = "default_merge_queue_min_entries_to_merge_wait_minutes",
        skip_serializing_if = "is_default_merge_queue_min_entries_to_merge_wait_minutes"
    )]
    pub merge_queue_min_entries_to_merge_wait_minutes: u32,
    #[serde(
        default = "default_merge_queue_max_entries_to_merge",
        skip_serializing_if = "is_default_merge_queue_max_entries_to_merge"
    )]
    pub merge_queue_max_entries_to_merge: u32,
    #[serde(
        default = "default_merge_queue_check_response_timeout_minutes",
        skip_serializing_if = "is_default_merge_queue_check_response_timeout_minutes"
    )]
    pub merge_queue_check_response_timeout_minutes: u32,
    pub prevent_creation: bool,
    pub prevent_update: bool,
    pub prevent_deletion: bool,
    pub prevent_force_push: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CratesIoPublishing {
    pub workflow_file: String,
    pub environment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Environment {
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Person {
    pub name: String,
    pub email: Option<String>,
    pub github_id: u64,
    pub github_sponsors: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct People {
    /// GitHub name as key.
    pub people: IndexMap<String, Person>,
}

fn is_branch_target(target: &ProtectionTarget) -> bool {
    matches!(target, ProtectionTarget::Branch)
}

fn is_default_merge_queue_method(method: &MergeQueueMethod) -> bool {
    matches!(method, MergeQueueMethod::Merge)
}

fn default_merge_queue_max_entries_to_build() -> u32 {
    5
}

fn is_default_merge_queue_max_entries_to_build(value: &u32) -> bool {
    *value == default_merge_queue_max_entries_to_build()
}

fn default_merge_queue_min_entries_to_merge_wait_minutes() -> u32 {
    5
}

fn is_default_merge_queue_min_entries_to_merge_wait_minutes(value: &u32) -> bool {
    *value == default_merge_queue_min_entries_to_merge_wait_minutes()
}

fn default_merge_queue_max_entries_to_merge() -> u32 {
    5
}

fn is_default_merge_queue_max_entries_to_merge(value: &u32) -> bool {
    *value == default_merge_queue_max_entries_to_merge()
}

fn default_merge_queue_check_response_timeout_minutes() -> u32 {
    60
}

fn is_default_merge_queue_check_response_timeout_minutes(value: &u32) -> bool {
    *value == default_merge_queue_check_response_timeout_minutes()
}
