use crate::sync::utils::ResponseExt;
use indexmap::IndexMap;
use log::{debug, trace};
use std::borrow::Cow;
use std::ops::Deref;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Deserialize)]
struct ApiRepo {
    #[serde(flatten)]
    repo: rust_team_data::v1::Repo,
    #[serde(default)]
    rulesets: Vec<rust_team_data::v1::BranchProtection>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ApiRepos {
    #[serde(flatten)]
    repos: IndexMap<String, Vec<ApiRepo>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Repo {
    pub(crate) branch_protections: Vec<rust_team_data::v1::BranchProtection>,
    pub(crate) rulesets: Vec<rust_team_data::v1::BranchProtection>,
    api_repo: rust_team_data::v1::Repo,
}

impl Repo {
    pub(crate) fn new(
        repo: rust_team_data::v1::Repo,
        rulesets: Vec<rust_team_data::v1::BranchProtection>,
    ) -> Self {
        let mut branch_protections = repo.branch_protections.clone();

        for ruleset in &rulesets {
            if let Some(position) = branch_protections.iter().position(|item| item == ruleset) {
                branch_protections.remove(position);
            }
        }

        Self {
            branch_protections,
            rulesets,
            api_repo: repo,
        }
    }
}

impl From<ApiRepo> for Repo {
    fn from(value: ApiRepo) -> Self {
        let ApiRepo { repo, rulesets } = value;
        Self::new(repo, rulesets)
    }
}

impl Deref for Repo {
    type Target = rust_team_data::v1::Repo;

    fn deref(&self) -> &Self::Target {
        &self.api_repo
    }
}

/// Determines how do we get access to the ground-truth data from `rust-lang/team`.
pub enum TeamApi {
    /// Access the live data from the published production REST API.
    Production,
    /// Directly access a directory with prebuilt JSON data.
    Prebuilt(PathBuf),
}

impl TeamApi {
    pub(crate) async fn get_teams(&self) -> anyhow::Result<Vec<rust_team_data::v1::Team>> {
        debug!("loading teams list from the Team API");
        Ok(self
            .req::<rust_team_data::v1::Teams>("teams.json")
            .await?
            .teams
            .into_iter()
            .map(|(_k, v)| v)
            .collect())
    }

    pub(crate) async fn get_repos(&self) -> anyhow::Result<Vec<Repo>> {
        debug!("loading teams list from the Team API");
        Ok(self
            .req::<ApiRepos>("repos.json")
            .await?
            .repos
            .into_iter()
            .flat_map(|(_k, v)| v.into_iter().map(Repo::from))
            .collect())
    }

    pub(crate) async fn get_lists(&self) -> anyhow::Result<rust_team_data::v1::Lists> {
        debug!("loading email lists list from the Team API");
        self.req::<rust_team_data::v1::Lists>("lists.json").await
    }

    pub(crate) async fn get_zulip_groups(&self) -> anyhow::Result<rust_team_data::v1::ZulipGroups> {
        debug!("loading GitHub id to Zulip id map from the Team API");
        self.req::<rust_team_data::v1::ZulipGroups>("zulip-groups.json")
            .await
    }

    pub(crate) async fn get_zulip_streams(
        &self,
    ) -> anyhow::Result<rust_team_data::v1::ZulipStreams> {
        debug!("loading Zulip streams from the Team API");
        self.req::<rust_team_data::v1::ZulipStreams>("zulip-streams.json")
            .await
    }

    async fn req<T: serde::de::DeserializeOwned>(&self, url: &str) -> anyhow::Result<T> {
        match self {
            TeamApi::Production => {
                let base = std::env::var("TEAM_DATA_BASE_URL")
                    .map(Cow::Owned)
                    .unwrap_or_else(|_| Cow::Borrowed(rust_team_data::v1::BASE_URL));
                let url = format!("{base}/{url}");
                trace!("http request: GET {url}");
                Ok(reqwest::get(&url)
                    .await?
                    .error_for_status()?
                    .json_annotated()
                    .await?)
            }
            TeamApi::Prebuilt(directory) => {
                let contents = std::fs::read(directory.join("v1").join(url))?;
                Ok(serde_json::from_slice(&contents)?)
            }
        }
    }
}
