//! Validates that remote data matches data in team repo.

use crate::data::Data;
use crate::github::{self, GitHubApi};
use crate::schema;
use log::error;
use rayon::prelude::*;
use std::collections::HashMap;

pub(crate) fn check(data: &Data) -> Result<(), failure::Error> {
    const BOT_TEAMS: &[&str] = &["bors", "bots", "rfcbot", "highfive"];
    let github = GitHubApi::new();
    let mut remote_teams = github
        .teams()?
        .into_par_iter()
        .filter(|t| !BOT_TEAMS.contains(&t.name.as_str()))
        .map(|team| {
            let members = github.team_members(team.id)?;
            Ok((team.name.clone(), (team, members)))
        })
        .collect::<Result<HashMap<_, _>, failure::Error>>()?;

    for team in data.teams() {
        let local_teams = team.github_teams(&data)?;
        let local_team = local_teams.into_iter().find(|t| t.org == "rust-lang");
        let local_team = match local_team {
            Some(t) => t,
            None => continue,
        };
        match remote_teams.remove(local_team.name) {
            Some((_, remote_members)) => {
                check_team_members_match(local_team, remote_members);
            }
            None => error!("Team '{}' in team repo but not on GitHub", local_team.name),
        }
    }
    for (remote_team_name, _) in remote_teams {
        error!("Team '{}' on GitHub but not in team repo", remote_team_name)
    }
    Ok(())
}

fn check_team_members_match(
    local_team: schema::GitHubTeam,
    remote_members: Vec<github::GitHubMember>,
) {
    let mut local_members = local_team.members;
    for remote_member in remote_members.iter() {
        let pos = local_members
            .iter()
            .position(|(_, id)| id == &remote_member.id);
        match pos {
            Some(pos) => {
                local_members.swap_remove(pos);
            }
            None => error!(
                "'{}' is on GitHub '{}' team but not in team repo definition",
                remote_member.name, local_team.name
            ),
        }
    }
    for (local_member_name, _) in local_members {
        error!(
            "'{}' is in team repo definition for '{}' but not on GitHub",
            local_member_name, local_team.name
        );
    }
}
