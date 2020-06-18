mod github;
mod mailgun;
mod team_api;

use crate::github::SyncGitHub;
use crate::team_api::TeamApi;
use failure::{Error, ResultExt};
use log::{error, info};

const AVAILABLE_SERVICES: &[&str] = &["github", "mailgun"];

fn usage() {
    eprintln!("available services:");
    for service in AVAILABLE_SERVICES {
        eprintln!("  {}", service);
    }
    eprintln!("available flags:");
    eprintln!("  --help              Show this help message");
    eprintln!("  --live              Apply the proposed changes to GitHub");
    eprintln!("  --team-repo <path>  Path to the local team repo to use");
    eprintln!("environment variables:");
    eprintln!("  GITHUB_TOKEN       Authentication token with GitHub");
    eprintln!("  MAILGUN_API_TOKEN  Authentication token with Mailgun");
}

fn app() -> Result<(), Error> {
    let mut dry_run = true;
    let mut next_team_repo = false;
    let mut team_repo = None;
    let mut services = Vec::new();
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
            service if AVAILABLE_SERVICES.contains(&service) => services.push(service.to_string()),
            _ => {
                eprintln!("unknown argument: {}", arg);
                usage();
                std::process::exit(1);
            }
        }
    }

    let team_api = team_repo
        .map(|p| TeamApi::Local(p.into()))
        .unwrap_or(TeamApi::Production);

    if services.is_empty() {
        info!("no service to synchronize specified, defaulting to all services");
        services = AVAILABLE_SERVICES.iter().map(|s| s.to_string()).collect();
    }

    for service in services {
        info!("synchronizing {}", service);
        match service.as_str() {
            "github" => {
                let token = std::env::var("GITHUB_TOKEN")
                    .with_context(|_| "failed to get the GITHUB_TOKEN environment variable")?;

                let sync = SyncGitHub::new(token, &team_api, dry_run)?;
                sync.synchronize_all()?;
            }
            "mailgun" => {
                mailgun::run()?;
            }
            _ => panic!("unknown service: {}", service),
        }
    }

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
    env.filter_module("sync_team", log::LevelFilter::Info);
    if let Ok(content) = std::env::var("RUST_LOG") {
        env.parse_filters(&content);
    }
    env.init();
}
