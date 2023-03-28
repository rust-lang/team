//! Validates that remote data matches data in team repo.

use crate::data::Data;
use crate::github::{self, GitHubApi};
use crate::schema::{self, ZulipGroupMember};
use crate::zulip::ZulipApi;
use log::{error, warn};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

pub(crate) fn check(data: &Data) -> Result<(), failure::Error> {
    check_github(data)?;
    check_zulip(data)?;
    Ok(())
}

fn check_zulip(data: &Data) -> Result<(), failure::Error> {
    let zulip = ZulipApi::new();
    zulip.require_auth()?;
    let mut remote_groups = zulip
        .get_user_groups()?
        .into_iter()
        .filter(|g| !g.is_system_group)
        .map(|g| (g.name.clone(), g))
        .collect::<HashMap<_, _>>();
    let remote_users = zulip.get_users()?;
    let zulip_id_to_name = remote_users
        .into_iter()
        .map(|u| (u.user_id, u.name))
        .collect::<HashMap<_, _>>();
    let name_from_id = |id| {
        zulip_id_to_name
            .get(&id)
            .unwrap_or_else(|| panic!("Zulip ID {} was not present in /users", id))
    };
    for (_, local_group) in &data.zulip_groups()? {
        match remote_groups.remove(local_group.name()) {
            Some(rg) => {
                let mut remote_members = rg.members.iter().collect::<HashSet<_>>();
                for local_member in local_group.members() {
                    let i = match local_member {
                        ZulipGroupMember::MemberWithId { zulip_id, .. } => *zulip_id,
                        ZulipGroupMember::JustId(zulip_id) => *zulip_id,
                        ZulipGroupMember::MemberWithoutId { github } => {
                            error!("Member '{github}' of Zulip user group '{}' does not have a Zulip id", local_group.name());
                            continue;
                        }
                    };
                    if !remote_members.remove(&i) {
                        error!(
                            "Zulip user '{}' is in the team repo for '{}' but not in the remote Zulip user group",
                            name_from_id(i),
                            local_group.name()
                        )
                    }
                }
                for remote_member_id in remote_members {
                    error!(
                        "Zulip user '{}' is in the remote Zulip user group '{}' but not in the team repo",
                        name_from_id(*remote_member_id),
                        local_group.name()
                    )
                }
            }
            None => error!(
                "User group '{}' is in the team repo but not on Zulip",
                local_group.name()
            ),
        }
    }

    for (_, remote_group) in remote_groups {
        error!(
            "Zulip group '{}' is on Zulip but not in team repo",
            remote_group.name
        )
    }
    Ok(())
}

pub(crate) fn check_github(data: &Data) -> Result<(), failure::Error> {
    const BOT_TEAMS: &[&str] = &["bors", "bots", "rfcbot", "highfive"];
    let github = GitHubApi::new();
    let pending_invites = github.pending_org_invites()?;
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
        let local_teams = team.github_teams(data)?;
        let local_team = local_teams.into_iter().find(|t| t.org == "rust-lang");
        let local_team = match local_team {
            Some(t) => t,
            None => continue,
        };
        match remote_teams.remove(local_team.name) {
            Some((_, remote_members)) => {
                check_team_members_match(local_team, remote_members, &pending_invites);
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
    pending_invites: &[github::User],
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
        if pending_invites.iter().any(|u| u.login == local_member_name) {
            warn!(
            "'{}' is in team repo definition for '{}' but has not yet accepted the org invite on GitHub",
            local_member_name, local_team.name
        );
        } else {
            error!(
                "'{}' is in team repo definition for '{}' but not on GitHub",
                local_member_name, local_team.name
            );
        }
    }
}
