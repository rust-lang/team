mod api;

use crate::team_api::TeamApi;
use api::{ZulipApi, ZulipUserGroup};
use rust_team_data::v1::ZulipGroupMember;

use std::collections::BTreeMap;

pub(crate) fn run(
    username: String,
    token: String,
    team_api: &TeamApi,
    dry_run: bool,
) -> anyhow::Result<()> {
    let zulip_api = ZulipApi::new(username, token, dry_run);
    let user_group_definitions = get_user_group_definitions(team_api, &zulip_api)?;
    let mut controller = ZulipController::new(zulip_api, dry_run)?;

    for (name, members) in user_group_definitions {
        controller.create_or_update_user_group(&name, &members)?;
    }

    Ok(())
}

/// Fetches the definitions of the user groups from the Team API
fn get_user_group_definitions(
    team_api: &TeamApi,
    zulip_api: &ZulipApi,
) -> anyhow::Result<BTreeMap<String, Vec<usize>>> {
    let email_map = zulip_api
        .get_users()?
        .into_iter()
        .map(|u| (u.email, u.user_id))
        .collect::<BTreeMap<_, _>>();
    let user_group_definitions = team_api
        .get_zulip_groups()?
        .groups
        .into_iter()
        .map(|(name, group)| {
            let members = &group.members;
            let member_ids = members
                .iter()
                .filter_map(|member| match member {
                    ZulipGroupMember::Email(e) => {
                        let id = email_map.get(e);
                        if id.is_none() {
                            log::warn!("no Zulip id found for '{}'", e);
                        }
                        id
                    }
                    ZulipGroupMember::Id(id) => Some(id),
                })
                .copied()
                .collect::<Vec<_>>();
            (name, member_ids)
        })
        .collect();
    Ok(user_group_definitions)
}

/// Interacts with the Zulip API
struct ZulipController {
    /// User group name to Zulip user group id
    user_group_ids: BTreeMap<String, ZulipUserGroup>,
    /// The Zulip API
    zulip_api: ZulipApi,
    /// Whether this is a dry run or not
    dry_run: bool,
}

impl ZulipController {
    /// Create a new `ZulipController`
    fn new(zulip_api: ZulipApi, dry_run: bool) -> anyhow::Result<Self> {
        let user_groups = zulip_api.get_user_groups()?;

        let user_group_ids = user_groups
            .into_iter()
            .map(|mut ug| {
                // sort for better diagnostics
                ug.members.sort_unstable();
                (ug.name.clone(), ug)
            })
            .collect();

        Ok(Self {
            user_group_ids,
            zulip_api,
            dry_run,
        })
    }

    /// Create or update a user group for the given team name and members
    fn create_or_update_user_group(
        &mut self,
        user_group_name: &str,
        member_zulip_ids: &[usize],
    ) -> anyhow::Result<()> {
        let id = self.user_group_id_from_name(user_group_name);
        let user_group_id = match id {
            Some(id) => {
                log::debug!(
                    "'{}' user group ({}) already exists on Zulip",
                    user_group_name,
                    id
                );
                id
            }
            None => {
                log::debug!(
                    "no '{}' user group found on Zulip. Creating one...",
                    user_group_name
                );
                self.create_user_group(
                    user_group_name,
                    &format!("The {user_group_name} team (managed by the Team repo)"),
                    member_zulip_ids,
                )?;
                return Ok(());
            }
        };

        let existing_members = self.user_group_members_from_name(user_group_name).unwrap();
        log::debug!(
            "'{}' user group ({}) has members on Zulip {:?} and needs to have {:?}",
            user_group_name,
            user_group_id,
            existing_members,
            member_zulip_ids
        );
        let add_ids = member_zulip_ids
            .iter()
            .filter(|i| !existing_members.contains(i))
            .copied()
            .collect::<Vec<_>>();
        let remove_ids = existing_members
            .iter()
            .filter(|i| !member_zulip_ids.contains(i))
            .copied()
            .collect::<Vec<_>>();

        // We don't currently update the members field of the cached user group because it's
        // not necessary, but for correctness sake we should consider doing so
        self.zulip_api
            .update_user_group_members(user_group_id, &add_ids, &remove_ids)
    }

    /// Get a user group id for the given user group name
    fn user_group_id_from_name(&self, user_group_name: &str) -> Option<usize> {
        self.user_group_ids.get(user_group_name).map(|u| u.id)
    }

    /// Create a user group with a certain name, description, and members
    fn create_user_group(
        &mut self,
        user_group_name: &str,
        description: &str,
        member_ids: &[usize],
    ) -> anyhow::Result<usize> {
        self.zulip_api
            .create_user_group(user_group_name, description, member_ids)?;

        if self.dry_run {
            // If this is a dry run, we insert a record since it won't be on the actual API
            self.user_group_ids.insert(
                user_group_name.to_owned(),
                ZulipUserGroup {
                    id: 0,
                    name: user_group_name.to_owned(),
                    members: member_ids.into(),
                },
            );
        } else {
            // Otherwise, update the user group cache so it has the user group that was just created
            let user_groups = self.zulip_api.get_user_groups()?;
            let user_groups = user_groups
                .into_iter()
                .map(|ug| (ug.name.clone(), ug))
                .collect();
            self.user_group_ids = user_groups;
        }

        Ok(self
            .user_group_id_from_name(user_group_name)
            .expect("user group id not found even thoough it was just created"))
    }

    /// Get the members of a user group given its name
    fn user_group_members_from_name(&self, user_group_name: &str) -> Option<&[usize]> {
        self.user_group_ids
            .get(user_group_name)
            .map(|u| &u.members[..])
    }
}
