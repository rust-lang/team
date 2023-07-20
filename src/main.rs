#![allow(clippy::new_ret_no_self, clippy::redundant_closure)]

mod data;
#[macro_use]
mod permissions;
mod check_synced;
mod github;
mod schema;
mod static_api;
mod validate;
mod zulip;

const USER_AGENT: &str = "https://github.com/rust-lang/team (infra@rust-lang.org)";

use data::Data;
use schema::{Email, Team, TeamKind};

use failure::{err_msg, Error};
use log::{error, info, warn};
use std::{collections::HashMap, path::PathBuf};
use structopt::StructOpt;

#[derive(structopt::StructOpt)]
#[structopt(name = "team", about = "manage the rust team members")]
enum Cli {
    #[structopt(name = "check", help = "check if the configuration is correct")]
    Check {
        #[structopt(long = "strict", help = "fail if optional checks are not executed")]
        strict: bool,
        #[structopt(
            long = "skip",
            multiple = true,
            help = "skip one or more validation steps"
        )]
        skip: Vec<String>,
    },
    #[structopt(
        name = "add-person",
        help = "add a new person from their GitHub profile"
    )]
    AddPerson { github_name: String },
    #[structopt(
        name = "add-repo",
        help = "add a new repo config from an existing GitHub repo"
    )]
    AddRepo { org: String, name: String },
    #[structopt(name = "static-api", help = "generate the static API")]
    StaticApi { dest: String },
    #[structopt(name = "show-person", help = "print information about a person")]
    ShowPerson { github_username: String },
    #[structopt(name = "dump-teams", help = "Lists all teams")]
    DumpTeams {
        #[structopt(
            long = "exclude-wgs",
            help = "whether to exclude listing working groups or not"
        )]
        exclude_working_groups: bool,
        #[structopt(
            long = "exclude-subteams",
            help = "whether to exclude listing subteams or not"
        )]
        exclude_subteams: bool,
        #[structopt(
            long = "include-pgs",
            help = "whether to include listing project groups or not"
        )]
        include_project_groups: bool,
        #[structopt(long = "only-leads", help = "whether to list only leads of the team")]
        only_leads: bool,
    },
    #[structopt(name = "dump-team", help = "print the members of a team")]
    DumpTeam { name: String },
    #[structopt(name = "dump-list", help = "print all the emails in a list")]
    DumpList { name: String },
    #[structopt(
        name = "dump-website",
        help = "dump website internationalization data as a .ftl file"
    )]
    DumpWebsite,
    #[structopt(
        name = "dump-permission",
        help = "print all the people with a permission"
    )]
    DumpPermission { name: String },
    #[structopt(name = "encrypt-email", help = "encrypt an email address")]
    EncryptEmail,
    #[structopt(name = "decrypt-email", help = "decrypt an email address")]
    DecryptEmail,
    #[structopt(
        name = "check-synced",
        help = "checked whether a particular resource is synced"
    )]
    CheckSynced,
}

fn main() {
    let mut env = env_logger::Builder::new();
    env.format_timestamp(None);
    env.format_module_path(false);
    env.filter_module("rust_team", log::LevelFilter::Info);
    if std::env::var("RUST_TEAM_FORCE_COLORS").is_ok() {
        env.write_style(env_logger::WriteStyle::Always);
    }
    env.parse_default_env();
    env.init();

    if let Err(e) = run() {
        error!("{}", e);
        for e in e.iter_causes() {
            error!("cause: {}", e);
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    let cli = Cli::from_args();
    let data = Data::load()?;
    match cli {
        Cli::Check { strict, skip } => {
            crate::validate::validate(
                &data,
                strict,
                &skip.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            )?;
        }
        Cli::AddPerson { ref github_name } => {
            #[derive(serde::Serialize)]
            #[serde(rename_all = "kebab-case")]
            struct PersonToAdd<'a> {
                name: &'a str,
                github: &'a str,
                github_id: usize,
                #[serde(skip_serializing_if = "Option::is_none")]
                email: Option<&'a str>,
            }

            let github = github::GitHubApi::new();
            let user = github.user(github_name)?;
            let github_name = user.login;
            let github_id = user.id;

            if data.person(&github_name).is_some() {
                failure::bail!("person already in the repo: {}", github_name);
            }

            let file = format!("people/{}.toml", github_name);
            std::fs::write(
                &file,
                toml::to_string_pretty(&PersonToAdd {
                    name: user.name.as_deref().unwrap_or_else(|| {
                        warn!(
                            "the person is missing the name on GitHub, defaulting to the username"
                        );
                        github_name.as_str()
                    }),
                    github: &github_name,
                    github_id,
                    email: user.email.as_deref().or_else(|| {
                        warn!("the person is missing the email on GitHub, leaving the field empty");
                        None
                    }),
                })?
                .as_bytes(),
            )?;

            info!("written data to {}", file);
        }
        Cli::AddRepo { org, name } => {
            #[derive(serde::Serialize, Debug)]
            #[serde(rename_all = "kebab-case")]
            struct AccessToAdd {
                #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
                teams: HashMap<String, String>,
                #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
                individuals: HashMap<String, String>,
            }
            #[derive(serde::Serialize, Debug)]
            #[serde(rename_all = "kebab-case")]
            struct BranchToAdd {
                name: String,
                #[serde(skip_serializing_if = "Option::is_none")]
                dismiss_stale_review: Option<bool>,
                #[serde(skip_serializing_if = "Option::is_none")]
                ci_checks: Option<Vec<String>>,
            }
            #[derive(serde::Serialize, Debug)]
            #[serde(rename_all = "kebab-case")]
            struct RepoToAdd<'a> {
                org: &'a str,
                name: &'a str,
                description: &'a str,
                bots: Vec<String>,
                access: AccessToAdd,
                #[serde(skip_serializing_if = "Vec::is_empty")]
                branch: Vec<BranchToAdd>,
            }
            let github = github::GitHubApi::new();
            let repo = match github.repo(&org, &name)? {
                Some(r) => r,
                None => failure::bail!("The repo '{}/{}' was not found on GitHub", org, name),
            };
            let mut teams = HashMap::new();
            let mut bots = Vec::new();

            const BOTS: &[&str] = &["bors", "rust-highfive", "rust-timer", "rustbot", "rfcbot"];
            let switch = |bot| {
                if bot == "rust-highfive" {
                    "highfive".to_owned()
                } else {
                    bot
                }
            };
            for team in github.repo_teams(&org, &name)? {
                if BOTS.contains(&team.name.as_str()) {
                    let name = switch(team.name);
                    bots.push(name.to_owned());
                } else if team.name == "bots" {
                    bots.extend(BOTS.iter().map(|&s| switch(s.to_owned())));
                } else {
                    teams.insert(team.name, team.permission.as_toml().to_owned());
                }
            }

            let individuals = github
                .repo_collaborators(&org, &name)?
                .into_iter()
                .map(|c| (c.name, c.permissions.highest().to_owned()))
                .collect();

            let mut branches = Vec::new();
            for branch in github.protected_branches(&org, &name)? {
                let protection = github.branch_protection(&org, &name, &branch.name)?;
                branches.push(BranchToAdd {
                    name: branch.name,
                    ci_checks: protection.required_status_checks.map(|c| c.contexts),
                    dismiss_stale_review: protection
                        .required_pull_request_reviews
                        .map(|r| r.dismiss_stale_reviews),
                });
            }

            let repo = RepoToAdd {
                org: &org,
                name: &name,
                description: &repo.description.unwrap_or_else(|| {
                    format!("The {name} repo maintained by the rust-lang project")
                }),
                bots,
                access: AccessToAdd { teams, individuals },
                branch: branches,
            };
            let file = format!("repos/{org}/{name}.toml");
            std::fs::write(file, toml::to_string_pretty(&repo)?.as_bytes())?;
        }
        Cli::StaticApi { ref dest } => {
            let dest = PathBuf::from(dest);
            let generator = crate::static_api::Generator::new(&dest, &data)?;
            generator.generate()?;
        }
        Cli::ShowPerson {
            ref github_username,
        } => {
            let person = data
                .person(github_username)
                .ok_or_else(|| err_msg("unknown person"))?;

            println!("-- {} --", person.name());
            println!();

            println!("github: @{}", person.github());
            if let Email::Present(email) = person.email() {
                println!("email:  {}", email);
            }
            println!();

            let mut bors_permissions = person.permissions().bors().clone();
            let mut other_permissions = person.permissions().booleans().clone();

            println!("teams:");
            let mut teams: Vec<_> = data
                .teams()
                .filter_map(|team| match team.contains_person(&data, person) {
                    Ok(true) => Some(Ok(team)),
                    Ok(false) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<_, _>>()?;
            teams.sort_by_key(|team| team.name());
            if teams.is_empty() {
                println!("  (none)");
            } else {
                for team in teams {
                    println!("  - {}", team.name());
                    bors_permissions.extend(team.permissions().bors().clone());
                    other_permissions.extend(team.permissions().booleans().clone());

                    if team.leads().contains(person.github()) {
                        bors_permissions.extend(team.leads_permissions().bors().clone());
                        other_permissions.extend(team.leads_permissions().booleans().clone());
                    }
                }
            }
            println!();

            let mut bors_permissions: Vec<_> = bors_permissions.into_iter().collect();
            bors_permissions.sort_by_key(|(repo, _)| repo.clone());
            println!("bors permissions:");
            if bors_permissions.is_empty() {
                println!("  (none)");
            } else {
                for (repo, perms) in bors_permissions {
                    println!("  - {}", repo);
                    if perms.review() {
                        println!("    - review");
                    }
                    if perms.try_() {
                        println!("    - try");
                    }
                }
            }
            println!();

            let mut other_permissions: Vec<_> = other_permissions
                .into_iter()
                .filter_map(|(key, value)| if value { Some(key) } else { None })
                .collect();
            other_permissions.sort();
            println!("other permissions:");
            if other_permissions.is_empty() {
                println!("  (none)");
            } else {
                for key in other_permissions {
                    println!("  - {}", key);
                }
            }
        }

        Cli::DumpTeams {
            exclude_working_groups,
            exclude_subteams,
            include_project_groups,
            only_leads,
        } => {
            for team in data.teams() {
                let excluded_wg = exclude_working_groups && team.kind() == TeamKind::WorkingGroup;
                let excluded_project_group =
                    !include_project_groups && team.kind() == TeamKind::ProjectGroup;
                let excluded_sub_teams = exclude_subteams && team.subteam_of().is_some();
                let excluded_marker_team = team.kind() == TeamKind::MarkerTeam;
                if excluded_wg
                    || excluded_project_group
                    || excluded_sub_teams
                    || excluded_marker_team
                {
                    continue;
                }
                println!("{} ({}):", team.name(), team.kind());
                if let Some(parent) = team.subteam_of() {
                    println!("  parent team: {}", parent);
                }

                println!("  members: ");
                dump_team_members(team, &data, only_leads, 1)?;
            }
        }

        Cli::DumpTeam { ref name } => {
            let team = data.team(name).ok_or_else(|| err_msg("unknown team"))?;
            dump_team_members(team, &data, false, 0)?;
        }
        Cli::DumpList { ref name } => {
            let list = data.list(name)?.ok_or_else(|| err_msg("unknown list"))?;
            let mut emails = list.emails().iter().collect::<Vec<_>>();
            emails.sort();
            for email in emails {
                println!("{}", email);
            }
        }
        Cli::DumpWebsite => {
            println!(
                "# Autogenerated by `cargo run dump-website` in https://github.com/rust-lang/team"
            );
            let mut teams: Vec<_> = data.teams().collect();
            teams.sort_by_key(|team| team.name());
            for team in teams {
                if let Some(website) = team.website_data() {
                    let name = team.name();
                    println!("governance-team-{}-name = {}", name, website.name());
                    println!(
                        "governance-team-{}-description = {}\n",
                        name,
                        website.description()
                    );
                }
            }
        }
        Cli::DumpPermission { ref name } => {
            if !crate::schema::Permissions::available(data.config()).contains(name) {
                failure::bail!("unknown permission: {}", name);
            }
            let mut allowed = crate::permissions::allowed_people(&data, name)?
                .into_iter()
                .map(|person| person.github())
                .collect::<Vec<_>>();
            allowed.sort_unstable();
            for github_username in &allowed {
                println!("{}", github_username);
            }
        }
        Cli::EncryptEmail => {
            let plain: String = dialoguer::Input::new()
                .with_prompt("Plaintext address")
                .interact_text()?;
            let key = dialoguer::Password::new()
                .with_prompt("Secret key")
                .interact()?;
            println!(
                "{}",
                rust_team_data::email_encryption::encrypt(&key, &plain)?
            );
        }
        Cli::DecryptEmail => {
            let encrypted: String = dialoguer::Input::new()
                .with_prompt("Encrypted address")
                .interact_text()?;
            let key = dialoguer::Password::new()
                .with_prompt("Secret key")
                .interact()?;
            println!(
                "{}",
                rust_team_data::email_encryption::try_decrypt(&key, &encrypted)?
            );
        }
        Cli::CheckSynced => check_synced::check(&data)?,
    }

    Ok(())
}

fn dump_team_members(
    team: &Team,
    data: &Data,
    only_leads: bool,
    tab_offset: u8,
) -> Result<(), Error> {
    let leads = team.leads();
    let mut members = team.members(data)?.into_iter().collect::<Vec<_>>();
    members.sort_unstable();
    for member in members {
        if only_leads && !leads.contains(member) {
            continue;
        }
        println!(
            "{}{}{}",
            "\t".repeat(tab_offset as usize),
            member,
            if leads.contains(member) {
                " (lead)"
            } else {
                ""
            }
        );
    }
    Ok(())
}
