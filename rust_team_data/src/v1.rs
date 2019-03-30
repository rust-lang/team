use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub static BASE_URL: &str = "https://team-api.infra.rust-lang.org/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamKind {
    Team,
    WorkingGroup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub kind: TeamKind,
    pub subteam_of: Option<String>,
    pub members: Vec<TeamMember>,
    pub website_data: Option<TeamWebsite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub name: String,
    pub github: String,
    pub is_lead: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamWebsite {
    pub name: String,
    pub description: String,
    pub page: String,
    pub email: Option<String>,
    pub repo: Option<String>,
    pub discord: Option<DiscordInvite>,
    pub weight: i64,
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
