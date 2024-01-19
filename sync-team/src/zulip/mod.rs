mod api;

use crate::team_api::TeamApi;
use api::{ZulipApi, ZulipUserGroup};
use rust_team_data::v1::ZulipGroupMember;

use std::{cell::RefCell, collections::BTreeMap, mem};

pub(crate) struct SyncZulip {
    zulip_controller: ZulipController,
    user_group_definitions: BTreeMap<String, Vec<usize>>,
}

impl SyncZulip {
    pub(crate) fn new(
        username: String,
        token: String,
        team_api: &TeamApi,
        dry_run: bool,
    ) -> anyhow::Result<Self> {
        let zulip_api = ZulipApi::new(username, token, dry_run);
        let user_group_definitions = get_user_group_definitions(team_api, &zulip_api)?;
        let zulip_controller = ZulipController::new(zulip_api, dry_run)?;
        Ok(Self {
            zulip_controller,
            user_group_definitions,
        })
    }

    pub(crate) fn diff_all(&self) -> anyhow::Result<Diff> {
        self.user_group_definitions
            .iter()
            .map(|(user_group_name, member_ids)| self.diff_user_group(user_group_name, member_ids))
            .collect::<anyhow::Result<Vec<_>>>()
            .map(|user_group_diffs| Diff { user_group_diffs })
    }

    fn diff_user_group(
        &self,
        user_group_name: &str,
        member_ids: &[usize],
    ) -> anyhow::Result<UserGroupDiff> {
        let id = self
            .zulip_controller
            .user_group_id_from_name(user_group_name);
        let user_group_id = match id {
            Some(id) => {
                log::debug!("'{user_group_name}' user group ({id}) already exists on Zulip");
                id
            }
            None => {
                log::debug!("no '{user_group_name}' user group found on Zulip");
                return Ok(UserGroupDiff::Create(CreateUserGroupDiff {
                    name: user_group_name.to_owned(),
                    description: format!("The {user_group_name} team (managed by the Team repo)"),
                    member_ids: member_ids.to_owned(),
                }));
            }
        };

        let existing_members = self
            .zulip_controller
            .user_group_members_from_name(user_group_name)
            .unwrap();
        log::debug!(
            "'{user_group_name}' user group ({user_group_id}) has members on Zulip {existing_members:?} and needs to have {member_ids:?}",
        );
        let add_ids = member_ids
            .iter()
            .filter(|i| !existing_members.contains(i))
            .copied()
            .collect::<Vec<_>>();
        let remove_ids = existing_members
            .iter()
            .filter(|i| !member_ids.contains(i))
            .copied()
            .collect::<Vec<_>>();
        Ok(UserGroupDiff::Update(UpdateUserGroupDiff {
            name: user_group_name.to_owned(),
            user_group_id,
            member_id_additions: add_ids,
            member_id_deletions: remove_ids,
        }))
    }
}

pub(crate) struct Diff {
    user_group_diffs: Vec<UserGroupDiff>,
}

impl Diff {
    pub(crate) fn apply(&self, sync: &SyncZulip) -> anyhow::Result<()> {
        for user_group_diff in &self.user_group_diffs {
            user_group_diff.apply(sync)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "💻 User Group Diffs:")?;
        for team_diff in &self.user_group_diffs {
            write!(f, "{team_diff}")?;
        }
        Ok(())
    }
}

enum UserGroupDiff {
    Create(CreateUserGroupDiff),
    Update(UpdateUserGroupDiff),
}

impl UserGroupDiff {
    fn apply(&self, sync: &SyncZulip) -> anyhow::Result<()> {
        match self {
            UserGroupDiff::Create(c) => c.apply(sync),
            UserGroupDiff::Update(u) => u.apply(sync),
        }
    }
}

impl std::fmt::Display for UserGroupDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create(c) => write!(f, "{c}"),
            Self::Update(u) => write!(f, "{u}"),
        }
    }
}

struct CreateUserGroupDiff {
    name: String,
    description: String,
    member_ids: Vec<usize>,
}

impl CreateUserGroupDiff {
    fn apply(&self, sync: &SyncZulip) -> Result<(), anyhow::Error> {
        sync.zulip_controller
            .create_user_group(&self.name, &self.description, &self.member_ids)
            .map(|_| ())
    }
}

impl std::fmt::Display for CreateUserGroupDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "➕ Creating user group:")?;
        writeln!(f, "  Name: {}", self.name)?;
        writeln!(f, "  Description: {}", self.description)?;
        writeln!(f, "  Members:")?;
        for member_id in &self.member_ids {
            writeln!(f, "    {member_id}")?;
        }
        Ok(())
    }
}

struct UpdateUserGroupDiff {
    name: String,
    user_group_id: usize,
    member_id_additions: Vec<usize>,
    member_id_deletions: Vec<usize>,
}

impl UpdateUserGroupDiff {
    fn apply(&self, sync: &SyncZulip) -> Result<(), anyhow::Error> {
        sync.zulip_controller.zulip_api.update_user_group_members(
            self.user_group_id,
            &self.member_id_additions,
            &self.member_id_deletions,
        )
    }
}

impl std::fmt::Display for UpdateUserGroupDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "📝 Updating user group:")?;
        writeln!(f, "  Name: {}", self.name)?;
        writeln!(f, "  Members:")?;
        for member_id in &self.member_id_additions {
            writeln!(f, "    ➕ {member_id}")?;
        }
        for member_id in &self.member_id_deletions {
            writeln!(f, "    − {member_id}")?;
        }
        Ok(())
    }
}

/// Fetches the definitions of the user groups from the Team API
fn get_user_group_definitions(
    team_api: &TeamApi,
    zulip_api: &ZulipApi,
) -> anyhow::Result<BTreeMap<String, Vec<usize>>> {
    let email_map = zulip_api
        .get_users()?
        .into_iter()
        .filter_map(|u| u.email.map(|e| (e, u.user_id)))
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
    user_group_ids: RefCell<BTreeMap<String, ZulipUserGroup>>,
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
            user_group_ids: RefCell::new(user_group_ids),
            zulip_api,
            dry_run,
        })
    }

    /// Get a user group id for the given user group name
    fn user_group_id_from_name(&self, user_group_name: &str) -> Option<usize> {
        self.user_group_ids
            .borrow()
            .get(user_group_name)
            .map(|u| u.id)
    }

    /// Create a user group with a certain name, description, and members
    fn create_user_group(
        &self,
        user_group_name: &str,
        description: &str,
        member_ids: &[usize],
    ) -> anyhow::Result<usize> {
        self.zulip_api
            .create_user_group(user_group_name, description, member_ids)?;

        if self.dry_run {
            // If this is a dry run, we insert a record since it won't be on the actual API
            self.user_group_ids.borrow_mut().insert(
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
            *self.user_group_ids.borrow_mut() = user_groups;
        }

        Ok(self
            .user_group_id_from_name(user_group_name)
            .expect("user group id not found even thoough it was just created"))
    }

    /// Get the members of a user group given its name
    fn user_group_members_from_name(&self, user_group_name: &str) -> Option<Vec<usize>> {
        self.user_group_ids
            .borrow()
            .get(user_group_name)
            .map(|u| u.members.to_owned())
    }
}
