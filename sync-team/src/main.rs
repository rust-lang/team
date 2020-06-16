mod github;

use crate::github::{GitHub, TeamPrivacy, TeamRole};
use failure::{Error, ResultExt};
use log::{debug, error, info, trace, warn};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;

static DEFAULT_DESCRIPTION: &str = "Managed by the rust-lang/team repository.";
static DEFAULT_PRIVACY: TeamPrivacy = TeamPrivacy::Closed;

struct Sync {
    github: GitHub,
    teams: Vec<rust_team_data::v1::Team>,
    usernames_cache: HashMap<usize, String>,
    org_owners: HashMap<String, HashSet<usize>>,
}

impl Sync {
    fn new(token: String, team_api: &TeamApi, dry_run: bool) -> Result<Self, Error> {
        let github = GitHub::new(token, dry_run);
        if dry_run {
            warn!("sync-github is running in dry mode, no changes will be applied.");
            warn!("run the binary with the --live flag to apply the changes.");
        }
        let teams = team_api.get_teams()?;

        debug!("caching mapping between user ids and usernames");
        let users = teams
            .iter()
            .filter_map(|t| t.github.as_ref().map(|gh| &gh.teams))
            .flat_map(|teams| teams)
            .flat_map(|team| &team.members)
            .copied()
            .collect::<HashSet<_>>();
        let usernames_cache = github.usernames(&users.into_iter().collect::<Vec<_>>())?;

        debug!("caching organization owners");
        let orgs = teams
            .iter()
            .filter_map(|t| t.github.as_ref())
            .flat_map(|gh| &gh.teams)
            .map(|gh_team| &gh_team.org)
            .collect::<HashSet<_>>();
        let mut org_owners = HashMap::new();
        for org in &orgs {
            org_owners.insert(org.to_string(), github.org_owners(&org)?);
        }

        Ok(Sync {
            github,
            teams,
            usernames_cache,
            org_owners,
        })
    }

    fn synchronize_all(&self) -> Result<(), Error> {
        for team in &self.teams {
            if let Some(gh) = &team.github {
                for github_team in &gh.teams {
                    self.synchronize(github_team)?;
                }
            }
        }
        Ok(())
    }

    fn synchronize(&self, github_team: &rust_team_data::v1::GitHubTeam) -> Result<(), Error> {
        let slug = format!("{}/{}", github_team.org, github_team.name);
        debug!("synchronizing {}", slug);

        // Ensure the team exists and is consistent
        let team = match self.github.team(&github_team.org, &github_team.name)? {
            Some(team) => team,
            None => self.github.create_team(
                &github_team.org,
                &github_team.name,
                DEFAULT_DESCRIPTION,
                DEFAULT_PRIVACY,
            )?,
        };
        if team.name != github_team.name
            || team.description != DEFAULT_DESCRIPTION
            || team.privacy != DEFAULT_PRIVACY
        {
            self.github.edit_team(
                &team,
                &github_team.name,
                DEFAULT_DESCRIPTION,
                DEFAULT_PRIVACY,
            )?;
        }

        let mut current_members = self.github.team_memberships(&team)?;

        // Ensure all expected members are in the team
        for member in &github_team.members {
            let expected_role = self.expected_role(&github_team.org, *member);
            let username = &self.usernames_cache[member];
            if let Some(member) = current_members.remove(&member) {
                if member.role != expected_role {
                    info!(
                        "{}: user {} has the role {} instead of {}, changing them...",
                        slug, username, member.role, expected_role
                    );
                    self.github.set_membership(&team, username, expected_role)?;
                } else {
                    debug!("{}: user {} is in the correct state", slug, username);
                }
            } else {
                info!("{}: user {} is missing, adding them...", slug, username);
                // If the user is not a member of the org and they *don't* have a pending
                // invitation this will send the invite email and add the membership in a "pending"
                // state.
                //
                // If the user didn't accept the invitation yet the next time the tool runs, the
                // method will be called again. Thankfully though in that case GitHub doesn't send
                // yet another invitation email to the user, but treats the API call as a noop, so
                // it's safe to do it multiple times.
                self.github.set_membership(&team, username, expected_role)?;
            }
        }

        // The previous cycle removed expected members from current_members, so it only contains
        // members to delete now.
        for member in current_members.values() {
            info!(
                "{}: user {} is not in the team anymore, removing them...",
                slug, member.username
            );
            self.github.remove_membership(&team, &member.username)?;
        }

        Ok(())
    }

    fn expected_role(&self, org: &str, user: usize) -> TeamRole {
        if let Some(true) = self
            .org_owners
            .get(org)
            .map(|owners| owners.contains(&user))
        {
            TeamRole::Maintainer
        } else {
            TeamRole::Member
        }
    }
}

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
    let sync = Sync::new(token, &team_api, dry_run)?;
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
