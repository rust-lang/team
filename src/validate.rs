use crate::data::Data;
use crate::github::GitHubApi;
use crate::schema::{Email, Permissions};
use failure::{bail, Error};
use log::{error, warn};
use regex::Regex;
use std::collections::{HashMap, HashSet};

static CHECKS: &[fn(&Data, &mut Vec<String>)] = &[
    validate_wg_names,
    validate_subteam_of,
    validate_team_leads,
    validate_team_members,
    validate_inactive_members,
    validate_list_email_addresses,
    validate_list_extra_people,
    validate_list_extra_teams,
    validate_list_addresses,
    validate_people_addresses,
    validate_discord_name,
    validate_duplicate_permissions,
    validate_permissions,
    validate_rfcbot_labels,
    validate_rfcbot_exclude_members,
    validate_team_names,
    validate_github_teams,
];

static GITHUB_CHECKS: &[fn(&Data, &GitHubApi, &mut Vec<String>)] = &[validate_github_usernames];

pub(crate) fn validate(data: &Data, strict: bool) -> Result<(), Error> {
    let mut errors = Vec::new();

    for check in CHECKS {
        check(data, &mut errors);
    }

    let github = GitHubApi::new();
    if let Err(err) = github.require_auth() {
        if strict {
            return Err(err);
        } else {
            warn!("couldn't perform checks relying on the GitHub API, some errors will not be detected");
            warn!("cause: {}", err);
        }
    } else {
        for check in GITHUB_CHECKS {
            check(data, &github, &mut errors);
        }
    }

    if !errors.is_empty() {
        errors.sort();
        errors.dedup_by(|a, b| a == b);

        for err in &errors {
            error!("validation error: {}", err);
        }

        bail!("{} validation errors found", errors.len());
    }

    Ok(())
}

/// Ensure working group names start with `wg-`
fn validate_wg_names(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, _| {
        match (team.is_wg() == team.name().starts_with("wg-"), team.is_wg()) {
            (false, true) => bail!(
                "working group `{}`'s name doesn't start with wg-",
                team.name()
            ),
            (false, false) => bail!(
                "team `{}` seems like a working group but has `wg = false`",
                team.name()
            ),
            (true, _) => {}
        }
        Ok(())
    });
}

/// Ensure `subteam-of` points to an existing team
fn validate_subteam_of(data: &Data, errors: &mut Vec<String>) {
    let teams: HashMap<_, _> = data
        .teams()
        .map(|t| (t.name(), t.subteam_of().is_some()))
        .collect();
    wrapper(data.teams(), errors, |team, _| {
        if let Some(subteam_of) = team.subteam_of() {
            match teams.get(subteam_of) {
                Some(false) => {}
                Some(true) => bail!(
                    "team `{}` can't be a subteam of a subteam (`{}`)",
                    team.name(),
                    subteam_of
                ),
                None => bail!(
                    "the parent of team `{}` doesn't exist: `{}`",
                    team.name(),
                    subteam_of
                ),
            }
        }
        Ok(())
    });
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
            if !data.person(person).unwrap().permissions().has_any() {
                bail!(
                    "person `{}` is not a member of any team and has no permissions",
                    person
                );
            }
            Ok(())
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
            if let Some(member) = data.person(member) {
                if let Email::Missing = member.email() {
                    bail!(
                        "person `{}` is a member of a mailing list but has no email address",
                        member.github()
                    );
                }
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

/// Ensure people email addresses are correct
fn validate_people_addresses(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.people(), errors, |person, _| {
        if let Email::Present(email) = person.email() {
            if !email.contains('@') {
                bail!("invalid email address of `{}`: {}", person.github(), email);
            }
        }
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

/// Ensure members of teams with permissions don't explicitly have those permissions
fn validate_duplicate_permissions(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, errors| {
        wrapper(team.members(&data)?.iter(), errors, |member, _| {
            if let Some(person) = data.person(member) {
                for permission in Permissions::AVAILABLE {
                    if team.permissions().has(permission)
                        && person.permissions().has_directly(permission)
                    {
                        bail!(
                            "user `{}` has the permission `{}` both explicitly and through \
                             the `{}` team",
                            member,
                            permission,
                            team.name()
                        );
                    }
                }
            }
            Ok(())
        });
        Ok(())
    });
}

/// Ensure the permissions are valid
fn validate_permissions(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, _| {
        team.permissions()
            .validate(format!("team `{}`", team.name()))?;
        Ok(())
    });
    wrapper(data.people(), errors, |person, _| {
        person
            .permissions()
            .validate(format!("user `{}`", person.github()))?;
        Ok(())
    });
}

/// Ensure there are no duplicate rfcbot labels
fn validate_rfcbot_labels(data: &Data, errors: &mut Vec<String>) {
    let mut labels = HashSet::new();
    wrapper(data.teams(), errors, move |team, errors| {
        if let Some(rfcbot) = team.rfcbot_data() {
            if !labels.insert(rfcbot.label.clone()) {
                errors.push(format!("duplicate rfcbot label: {}", rfcbot.label));
            }
        }
        Ok(())
    });
}

/// Ensure rfcbot's exclude-members only contains not duplicated team members
fn validate_rfcbot_exclude_members(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, move |team, errors| {
        if let Some(rfcbot) = team.rfcbot_data() {
            let mut exclude = HashSet::new();
            let members = team.members(data)?;
            wrapper(rfcbot.exclude_members.iter(), errors, move |member, _| {
                if !exclude.insert(member) {
                    bail!(
                        "duplicate member in `{}` rfcbot.exclude-members: {}",
                        team.name(),
                        member
                    );
                }
                if !members.contains(member.as_str()) {
                    bail!(
                        "person `{}` is not a member of team `{}` (in rfcbot.exclude-members)",
                        member,
                        team.name()
                    );
                }
                Ok(())
            });
        }
        Ok(())
    });
}

/// Ensure team names are alphanumeric + `-`
fn validate_team_names(data: &Data, errors: &mut Vec<String>) {
    wrapper(data.teams(), errors, |team, _| {
        if !team.name().chars().all(|c| c.is_alphanumeric() || c == '-') {
            bail!(
                "team name `{}` can only be alphanumeric with dashes",
                team.name()
            );
        }
        match (team.is_wg() == team.name().starts_with("wg-"), team.is_wg()) {
            (false, true) => bail!(
                "working group `{}`'s name doesn't start with wg-",
                team.name()
            ),
            (false, false) => bail!(
                "team `{}` seems like a working group but has `wg = false`",
                team.name()
            ),
            (true, _) => {}
        }
        Ok(())
    });
}

/// Ensure GitHub teams are unique and in the allowed orgs
fn validate_github_teams(data: &Data, errors: &mut Vec<String>) {
    let mut found = HashMap::new();
    let allowed = data.config().allowed_github_orgs();
    wrapper(data.teams(), errors, |team, errors| {
        wrapper(
            team.github_teams(data)?.into_iter(),
            errors,
            |gh_team, _| {
                if !allowed.contains(&*gh_team.org) {
                    bail!(
                        "GitHub organization `{}` isn't allowed (in team `{}`)",
                        gh_team.org,
                        team.name()
                    );
                }
                if let Some(other) = found.insert((gh_team.org, gh_team.name), team.name()) {
                    bail!(
                        "GitHub team `{}/{}` is defined for both the `{}` and `{}` teams",
                        gh_team.org,
                        gh_team.name,
                        team.name(),
                        other
                    );
                }
                Ok(())
            },
        );
        Ok(())
    });
}

/// Ensure there are no misspelled GitHub account names
fn validate_github_usernames(data: &Data, github: &GitHubApi, errors: &mut Vec<String>) {
    let people = data
        .people()
        .map(|p| (p.github_id(), p))
        .collect::<HashMap<_, _>>();
    match github.usernames(&people.keys().cloned().collect::<Vec<_>>()) {
        Ok(res) => wrapper(res.iter(), errors, |(id, name), _| {
            let original = people[id].github();
            if original != name {
                bail!("user `{}` changed username to `{}`", original, name);
            }
            Ok(())
        }),
        Err(err) => errors.push(format!("couldn't verify GitHub usernames: {}", err)),
    }
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
