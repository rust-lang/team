#![allow(clippy::new_ret_no_self, clippy::redundant_closure)]

mod data;
#[macro_use]
mod permissions;
mod github;
mod schema;
mod static_api;
mod validate;

use crate::{data::Data, schema::Email};
use failure::{err_msg, Error};
use log::{error, info, warn};
use std::path::PathBuf;
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
    #[structopt(name = "static-api", help = "generate the static API")]
    StaticApi { dest: String },
    #[structopt(name = "show-person", help = "print information about a person")]
    ShowPerson { github_username: String },
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
}

fn main() {
    let mut env = env_logger::Builder::new();
    env.default_format_timestamp(false);
    env.default_format_module_path(false);
    env.filter_module("rust_team", log::LevelFilter::Info);
    if std::env::var("RUST_TEAM_FORCE_COLORS").is_ok() {
        env.write_style(env_logger::WriteStyle::Always);
    }
    if let Ok(content) = std::env::var("RUST_LOG") {
        env.parse(&content);
    }
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

            println!("teams:");
            let mut teams: Vec<_> = data
                .teams()
                .filter(|team| team.members(&data).unwrap().contains(person.github()))
                .collect();
            teams.sort_by_key(|team| team.name());
            if teams.is_empty() {
                println!("  (none)");
            } else {
                for team in teams {
                    println!("  - {}", team.name());
                    bors_permissions.extend(team.permissions().bors().clone());
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
        }

        Cli::DumpTeam { ref name } => {
            let team = data.team(name).ok_or_else(|| err_msg("unknown team"))?;

            let leads = team.leads();
            let mut members = team.members(&data)?.into_iter().collect::<Vec<_>>();
            members.sort_unstable();
            for member in members {
                println!(
                    "{}{}",
                    member,
                    if leads.contains(member) {
                        " (lead)"
                    } else {
                        ""
                    }
                );
            }
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
            for team in data.teams() {
                if let Some(ref website) = team.website_data() {
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
            if !crate::schema::Permissions::available(data.config()).contains(&name) {
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
    }

    Ok(())
}
