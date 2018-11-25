use crate::data::Data;
use std::collections::HashSet;
use failure::{Error, bail};

pub(crate) fn validate(data: &Data) -> Result<(), Error> {
    let mut errors = Vec::new();

    validate_team_leads(data, &mut errors);
    validate_team_members(data, &mut errors);
    validate_inactive_members(data, &mut errors);

    if !errors.is_empty() {
        for err in &errors {
            eprintln!("validation error: {}", err);
        }

        bail!("{} validation errors found", errors.len());
    }

    Ok(())
}

/// Ensure team leaders are part of the teams they lead
fn validate_team_leads(data: &Data, errors: &mut Vec<String>) {
    for team in data.teams() {
        let members = match team.members(data) {
            Ok(m) => m,
            Err(err) => {
                errors.push(err.to_string());
                continue;
            }
        };
        for lead in team.leads() {
            if !members.contains(lead) {
                errors.push(format!("`{}` leads team `{}`, but is not a member of it", lead, team.name()));
            }
        }
    }
}

/// Ensure team members are people
fn validate_team_members(data: &Data, errors: &mut Vec<String>) {
    for team in data.teams() {
        let members = match team.members(data) {
            Ok(m) => m,
            Err(err) => {
                errors.push(err.to_string());
                continue;
            }
        };
        for member in members {
            if data.person(member).is_none() {
                errors.push(format!("person `{}` is member of team `{}` but doesn't exist", member, team.name()));
            }
        }
    }
}

/// Ensure every person is part of at least a team
fn validate_inactive_members(data: &Data, errors: &mut Vec<String>) {
    let mut active_members = HashSet::new();
    for team in data.teams() {
        let members = match team.members(data) {
            Ok(m) => m,
            Err(err) => {
                errors.push(err.to_string());
                continue;
            }
        };
        for member in members {
            active_members.insert(member);
        }
    }

    let all_members = data.people().map(|p| p.github()).collect::<HashSet<_>>();
    for person in all_members.difference(&active_members) {
        errors.push(format!("person `{}` is not a member of any team", person));
    }
}
