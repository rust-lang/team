use serde::{Deserialize, Serialize};

pub static BASE_URL: &str = "https://team-api.infra.rust-lang.org/v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Permission {
    pub people: Vec<PermissionPerson>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PermissionPerson {
    pub github_id: usize,
    pub github: String,
    pub name: String,
    pub discord_id: Option<usize>,
}
