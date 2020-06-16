use failure::Error;
use log::{debug, info, trace};
use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Command;

pub(crate) enum TeamApi {
    Production,
    Local(PathBuf),
}

impl TeamApi {
    pub(crate) fn get_teams(&self) -> Result<Vec<rust_team_data::v1::Team>, Error> {
        debug!("loading teams list from the Team API");
        Ok(self
            .req::<rust_team_data::v1::Teams>("teams.json")?
            .teams
            .into_iter()
            .map(|(_k, v)| v)
            .collect())
    }

    fn req<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, Error> {
        match self {
            TeamApi::Production => {
                let base = std::env::var("TEAM_DATA_BASE_URL")
                    .map(|s| Cow::Owned(s))
                    .unwrap_or_else(|_| Cow::Borrowed(rust_team_data::v1::BASE_URL));
                let url = format!("{}/{}", base, url);
                trace!("http request: GET {}", url);
                Ok(reqwest::get(&url)?.error_for_status()?.json()?)
            }
            TeamApi::Local(ref path) => {
                let dest = tempfile::tempdir()?;
                info!(
                    "generating the content of the Team API from {}",
                    path.display()
                );
                let status = Command::new("cargo")
                    .arg("run")
                    .arg("--")
                    .arg("static-api")
                    .arg(&dest.path())
                    .env("RUST_LOG", "rust_team=warn")
                    .current_dir(path)
                    .status()?;
                if status.success() {
                    info!("contents of the Team API generated successfully");
                    let contents = std::fs::read(dest.path().join("v1").join(url))?;
                    Ok(serde_json::from_slice(&contents)?)
                } else {
                    failure::bail!("failed to generate the contents of the Team API");
                }
            }
        }
    }
}
