use crate::utils::ResponseExt;
use log::{debug, info, trace};
use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Command;

/// Determines how do we get access to the ground-truth data from `rust-lang/team`.
pub(crate) enum TeamApi {
    /// Access the live data from the published production REST API.
    Production,
    /// Build the JSON data from a checkout of `rust-lang/team`.
    Checkout(PathBuf),
    /// Directly access a directory with prebuilt JSON data.
    Prebuilt(PathBuf),
}

impl TeamApi {
    pub(crate) fn get_teams(&self) -> anyhow::Result<Vec<rust_team_data::v1::Team>> {
        debug!("loading teams list from the Team API");
        Ok(self
            .req::<rust_team_data::v1::Teams>("teams.json")?
            .teams
            .into_iter()
            .map(|(_k, v)| v)
            .collect())
    }

    pub(crate) fn get_repos(&self) -> anyhow::Result<Vec<rust_team_data::v1::Repo>> {
        debug!("loading teams list from the Team API");
        Ok(self
            .req::<rust_team_data::v1::Repos>("repos.json")?
            .repos
            .into_iter()
            .flat_map(|(_k, v)| v)
            .collect())
    }

    pub(crate) fn get_lists(&self) -> anyhow::Result<rust_team_data::v1::Lists> {
        debug!("loading email lists list from the Team API");
        self.req::<rust_team_data::v1::Lists>("lists.json")
    }

    pub(crate) fn get_zulip_groups(&self) -> anyhow::Result<rust_team_data::v1::ZulipGroups> {
        debug!("loading GitHub id to Zulip id map from the Team API");
        self.req::<rust_team_data::v1::ZulipGroups>("zulip-groups.json")
    }

    fn req<T: serde::de::DeserializeOwned>(&self, url: &str) -> anyhow::Result<T> {
        match self {
            TeamApi::Production => {
                let base = std::env::var("TEAM_DATA_BASE_URL")
                    .map(Cow::Owned)
                    .unwrap_or_else(|_| Cow::Borrowed(rust_team_data::v1::BASE_URL));
                let url = format!("{base}/{url}");
                trace!("http request: GET {}", url);
                Ok(reqwest::blocking::get(&url)?
                    .error_for_status()?
                    .json_annotated()?)
            }
            TeamApi::Checkout(path) => {
                let dest = tempfile::tempdir()?;
                info!(
                    "generating the content of the Team API from {}",
                    path.display()
                );
                let status = Command::new("cargo")
                    .arg("run")
                    .arg("--")
                    .arg("static-api")
                    .arg(dest.path())
                    .env("RUST_LOG", "rust_team=warn")
                    .current_dir(path)
                    .status()?;
                if status.success() {
                    info!("contents of the Team API generated successfully");
                    let contents = std::fs::read(dest.path().join("v1").join(url))?;
                    Ok(serde_json::from_slice(&contents)?)
                } else {
                    anyhow::bail!("failed to generate the contents of the Team API");
                }
            }
            TeamApi::Prebuilt(directory) => {
                let contents = std::fs::read(directory.join("v1").join(url))?;
                Ok(serde_json::from_slice(&contents)?)
            }
        }
    }
}
