#![allow(clippy::enum_variant_names)]

mod data;
#[macro_use]
mod permissions;
mod ci;
mod github;
mod schema;
mod static_api;
mod validate;
mod zulip;

const USER_AGENT: &str = "https://github.com/rust-lang/team (infra@rust-lang.org)";

use data::Data;
use schema::{Email, Team, TeamKind};
use zulip::ZulipApi;

use crate::ci::{check_codeowners, generate_codeowners_file};
use crate::schema::RepoPermission;
use anyhow::{bail, format_err, Error};
use clap::Parser;
use log::{error, info, warn};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

#[derive(clap::ValueEnum, Clone, Debug)]
enum DumpIndividualAccessGroupBy {
    Person,
    Repo,
}

#[derive(clap::Parser, Debug)]
/// Manage the Rust team members
enum Cli {
    /// Check if the configuration is correct
    Check {
        /// Fail if optional checks are not executed
        #[arg(long)]
        strict: bool,
        /// Skip one or more validation steps
        #[arg(long, num_args = 1..)]
        skip: Vec<String>,
    },
    /// Add a new person from their GitHub profile
    AddPerson { github_name: String },
    /// Generate the static API
    StaticApi { dest: String },
    /// Print information about a person
    ShowPerson { github_username: String },
    /// List all teams
    DumpTeams {
        /// Whether to exclude listing working groups or not
        #[arg(long = "exclude-wgs")]
        exclude_working_groups: bool,
        /// Whether to exclude listing subteams or not
        #[arg(long)]
        exclude_subteams: bool,
        /// Whether to include listing project groups or not
        #[arg(long = "include-pgs")]
        include_project_groups: bool,
        /// Whether to list only leads of the team
        #[arg(long)]
        only_leads: bool,
    },
    /// Print the members of a team
    DumpTeam { name: String },
    /// Print all the emails in a list
    DumpList { name: String },
    /// Dump website internationalization data as a .ftl file
    DumpWebsite,
    /// Print all the people with a permission
    DumpPermission { name: String },
    /// Print all the people with an individual access to a repository
    DumpIndividualAccess {
        #[arg(long, default_value = "repo")]
        group_by: DumpIndividualAccessGroupBy,
    },
    /// Encrypt an email address
    EncryptEmail,
    /// Decrypt an email address
    DecryptEmail,
    /// CI scripts
    #[clap(subcommand)]
    Ci(CiOpts),
}

#[derive(clap::Parser, Debug)]
enum CiOpts {
    /// Generate the .github/CODEOWNERS file
    GenerateCodeowners,
    /// Check if the .github/CODEOWNERS file is up-to-date
    CheckCodeowners,
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
        error!("{:?}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    let cli = Cli::parse();
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
                github_id: u64,
                #[serde(skip_serializing_if = "Option::is_none")]
                email: Option<&'a str>,
            }

            let github = github::GitHubApi::new();
            let user = github.user(github_name)?;
            let github_name = user.login;
            let github_id = user.id;

            if data.person(&github_name).is_some() {
                bail!("person already in the repo: {}", github_name);
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
                .ok_or_else(|| format_err!("unknown person"))?;

            println!("-- {} --", person.name());
            println!();

            println!("github: @{}", person.github());
            if let Some(zulip_id) = person.zulip_id() {
                let zulip = ZulipApi::new();
                match zulip.require_auth() {
                    Ok(()) => match zulip.get_user(zulip_id) {
                        Ok(user) => println!("zulip: {} ({zulip_id})", user.name),
                        Err(err) => {
                            println!("zulip_id: {zulip_id}  # Failed to look up Zulip name: {err}")
                        }
                    },
                    Err(err) => {
                        // We have no authentication credentials, so don't even attempt the network access.
                        println!("zulip_id: {zulip_id}  # Skipped name lookup: {err}");
                    }
                }
            }
            if let Email::Present(email) = person.email() {
                println!("email: {}", email);
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
            let team = data.team(name).ok_or_else(|| format_err!("unknown team"))?;
            dump_team_members(team, &data, false, 0)?;
        }
        Cli::DumpList { ref name } => {
            let list = data
                .list(name)?
                .ok_or_else(|| format_err!("unknown list"))?;
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
            let mut roles = BTreeMap::new();
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
                for role in team.roles() {
                    roles.insert(&role.id, &role.description);
                }
            }
            for (role_id, description) in roles {
                println!("governance-role-{role_id} = {description}");
            }
        }
        Cli::DumpPermission { ref name } => {
            if !crate::schema::Permissions::available(data.config()).contains(name) {
                bail!("unknown permission: {}", name);
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
        Cli::DumpIndividualAccess { group_by } => {
            // user -> (repo, access)
            let mut users: HashMap<String, Vec<(String, RepoPermission)>> = HashMap::default();
            for repo in data.repos() {
                let repo_name = format!("{}/{}", repo.org, repo.name);
                for (user, access) in &repo.access.individuals {
                    users
                        .entry(user.clone())
                        .or_default()
                        .push((repo_name.clone(), access.clone()));
                }
            }
            let output: HashMap<String, Vec<(String, RepoPermission)>> = match group_by {
                DumpIndividualAccessGroupBy::Person => users,
                DumpIndividualAccessGroupBy::Repo => {
                    let mut repos: HashMap<String, Vec<(String, RepoPermission)>> = HashMap::new();
                    for (user, accesses) in users {
                        for (repo, access) in accesses {
                            repos.entry(repo).or_default().push((user.clone(), access));
                        }
                    }
                    repos
                }
            };
            let mut output = output.into_iter().collect::<Vec<_>>();
            output.sort_unstable_by_key(|(key, _)| key.clone());
            for (_, values) in output.iter_mut() {
                values.sort_unstable_by_key(|(name, _)| name.clone());
            }
            for (key, values) in output {
                println!("{key}");
                for (name, permission) in values {
                    println!("\t {name}: {permission:?}");
                }
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
        Cli::Ci(opts) => match opts {
            CiOpts::GenerateCodeowners => generate_codeowners_file(data)?,
            CiOpts::CheckCodeowners => check_codeowners(data)?,
        },
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
            "\t".repeat(usize::from(tab_offset)),
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
