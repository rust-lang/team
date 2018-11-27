use crate::data::Data;
use failure::{bail, Error};
use regex::Regex;
use std::collections::HashSet;

pub(crate) fn validate(data: &Data) -> Result<(), Error> {
    let mut errors = Vec::new();

    validate_team_leads(data, &mut errors);
    validate_team_members(data, &mut errors);
    validate_inactive_members(data, &mut errors);
    validate_list_email_addresses(data, &mut errors);
    validate_list_extra_people(data, &mut errors);
    validate_list_extra_teams(data, &mut errors);
    validate_list_addresses(data, &mut errors);
    validate_discord_name(data, &mut errors);

    if !errors.is_empty() {
        errors.sort();
        errors.dedup_by(|a, b| a == b);

        for err in &errors {
            eprintln!("validation error: {}", err);
        }

        bail!("{} validation errors found", errors.len());
    }

    Ok(())
}

/// Ensure team leaders are part of the teams they lead
fn validate_team_leads(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, errors| {
        let members = team.members(data)?;
        wrapper(team.leads().iter(), errors, |lead, _| {
            if !members.contains(lead) {
                bail!(
                    "`{}` leads team `{}`, but is not a member of it",
                    lead,
                    team.name()
                );
            }
            Ok(())
        });
        Ok(())
    });
}

/// Ensure t_eam members are people
fn validate_team_members(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, errors| {
        wrapper(team.members(data)?.iter(), errors, |member, _| {
            if data.person(member).is_none() {
                bail!(
                    "person `{}` is member of team `{}` but doesn't exist",
                    member,
                    team.name()
                );
            }
            Ok(())
        });
        Ok(())
    });
}

/// Ensure every person is part of at least a team
fn validate_inactive_members(data: &Data, errors: &mut Vec<String>) {
    let mut active_members = HashSet::new();
    wrapper(data.teams(), errors, |team, _| {
        let members = team.members(data)?;
        for member in members {
            active_members.insert(member);
        }
        Ok(())
    });

    let all_members = data.people().map(|p| p.github()).collect::<HashSet<_>>();
    wrapper(
        all_members.difference(&active_members),
        errors,
        |person, _| {
            bail!("person `{}` is not a member of any team", person);
        },
    );
}

/// Ensure every member of a team with a mailing list has an email address
fn validate_list_email_addresses(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, errors| {
        if team.lists(data)?.is_empty() {
            return Ok(());
        }
        wrapper(team.members(data)?.iter(), errors, |member, _| {
            let member = data.person(member).unwrap();
            if member.email().is_none() {
                bail!(
                    "person `{}` is a member of a mailing list but has no email address",
                    member.github()
                );
            }
            Ok(())
        });
        Ok(())
    });
}

/// Ensure members of extra-people in a list are real people
fn validate_list_extra_people(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, errors| {
        wrapper(team.raw_lists().iter(), errors, |list, _| {
            for person in &list.extra_people {
                if data.person(person).is_none() {
                    bail!(
                        "person `{}` does not exist (in list `{}`)",
                        person,
                        list.address
                    );
                }
            }
            Ok(())
        });
        Ok(())
    });
}

/// Ensure members of extra-people in a list are real people
fn validate_list_extra_teams(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, errors| {
        wrapper(team.raw_lists().iter(), errors, |list, _| {
            for list_team in &list.extra_teams {
                if data.team(list_team).is_none() {
                    bail!(
                        "team `{}` does not exist (in list `{}`)",
                        list_team,
                        list.address
                    );
                }
            }
            Ok(())
        });
        Ok(())
    });
}

/// Ensure the list addresses are correct
fn validate_list_addresses(data: &Data, errors: &mut Vec<String>) {
    let email_re = Regex::new(r"^[a-zA-Z0-9_\.-]+@([a-zA-Z0-9_\.-]+)$").unwrap();
    let config = data.config().allowed_mailing_lists_domains();
    wrapper(data.teams(), errors, |team, errors| {
        wrapper(team.raw_lists().iter(), errors, |list, _| {
            if let Some(captures) = email_re.captures(&list.address) {
                if !config.contains(&captures[1]) {
                    bail!("list address on a domain we don't own: `{}`", list.address);
                }
            } else {
                bail!("invalid list address: `{}`", list.address);
            }
            Ok(())
        });
        Ok(())
    });
}

/// Ensure the Discord name is formatted properly
fn validate_discord_name(data: &Data, errors: &mut Vec<String>) {
    // https://discordapp.com/developers/docs/resources/user#usernames-and-nicknames
    let name_re = Regex::new(r"^[^@#:`]{2,32}#[0-9]{4}$").unwrap();
    wrapper(data.people(), errors, |person, _| {
        if let Some(name) = person.discord() {
            if !name_re.is_match(name) {
                bail!(
                    "user `{}` has an invalid discord name: {}",
                    person.github(),
                    name
                );
            }
        }
        Ok(())
    })
}

fn wrapper<T, I, F>(iter: I, errors: &mut Vec<String>, mut func: F)
where
    I: Iterator<Item = T>,
    F: FnMut(T, &mut Vec<String>) -> Result<(), Error>,
{
    for item in iter {
        if let Err(err) = func(item, errors) {
            errors.push(err.to_string());
        }
    }
}
