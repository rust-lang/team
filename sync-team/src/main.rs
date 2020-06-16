mod github;

use crate::github::SyncGitHub;
use failure::{Error, ResultExt};
use log::{debug, error, info, trace};
use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Command;

enum TeamApi {
    Production,
    Local(PathBuf),
}

impl TeamApi {
    fn get_teams(&self) -> Result<Vec<rust_team_data::v1::Team>, Error> {
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

fn usage() {
    eprintln!("available flags:");
    eprintln!("  --help              Show this help message");
    eprintln!("  --live              Apply the proposed changes to GitHub");
    eprintln!("  --team-repo <path>  Path to the local team repo to use");
    eprintln!("environment variables:");
    eprintln!("  GITHUB_TOKEN  Authentication token with GitHub");
}

fn app() -> Result<(), Error> {
    let token = std::env::var("GITHUB_TOKEN")
        .with_context(|_| "failed to get the GITHUB_TOKEN environment variable")?;

    let mut dry_run = true;
    let mut next_team_repo = false;
    let mut team_repo = None;
    for arg in std::env::args().skip(1) {
        if next_team_repo {
            team_repo = Some(arg);
            next_team_repo = false;
            continue;
        }
        match arg.as_str() {
            "--live" => dry_run = false,
            "--team-repo" => next_team_repo = true,
            "--help" => {
                usage();
                return Ok(());
            }
            other => {
                eprintln!("unknown argument: {}", other);
                usage();
                std::process::exit(1);
            }
        }
    }

    let team_api = team_repo
        .map(|p| TeamApi::Local(p.into()))
        .unwrap_or(TeamApi::Production);

    let sync = SyncGitHub::new(token, &team_api, dry_run)?;
    sync.synchronize_all()?;

    Ok(())
}

fn main() {
    init_log();
    if let Err(err) = app() {
        error!("{}", err);
        for cause in err.iter_causes() {
            error!("caused by: {}", cause);
        }
        std::process::exit(1);
    }
}

fn init_log() {
    let mut env = env_logger::Builder::new();
    env.filter_module("sync_github", log::LevelFilter::Info);
    if let Ok(content) = std::env::var("RUST_LOG") {
        env.parse_filters(&content);
    }
    env.init();
}
