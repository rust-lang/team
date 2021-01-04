use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub static BASE_URL: &str = "https://team-api.infra.rust-lang.org/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamKind {
    Team,
    WorkingGroup,
    ProjectGroup,
    MarkerTeam,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub kind: TeamKind,
    pub subteam_of: Option<String>,
    pub members: Vec<TeamMember>,
    pub alumni: Vec<TeamMember>,
    pub github: Option<TeamGitHub>,
    pub website_data: Option<TeamWebsite>,
    pub discord_role: Option<DiscordRole>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub name: String,
    pub github: String,
    pub github_id: usize,
    pub is_lead: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamGitHub {
    pub teams: Vec<GitHubTeam>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubTeam {
    pub org: String,
    pub name: String,
    pub members: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamWebsite {
    pub name: String,
    pub description: String,
    pub page: String,
    pub email: Option<String>,
    pub repo: Option<String>,
    pub discord: Option<DiscordInvite>,
    pub zulip_stream: Option<String>,
    pub weight: i64,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordRole {
    pub name: String,
    pub role_id: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordInvite {
    pub channel: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Teams {
    #[serde(flatten)]
    pub teams: IndexMap<String, Team>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {
    pub address: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lists {
    pub lists: IndexMap<String, List>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permission {
    pub github_users: Vec<String>,
    pub github_ids: Vec<usize>,
    pub discord_ids: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rfcbot {
    pub teams: IndexMap<String, RfcbotTeam>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfcbotTeam {
    pub name: String,
    pub ping: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZulipMapping {
    /// Zulip ID to GitHub ID
    pub users: IndexMap<usize, usize>,
}
