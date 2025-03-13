mod api;

use crate::team_api::TeamApi;
use anyhow::Context;
use api::{ZulipApi, ZulipStream, ZulipUserGroup};
use rust_team_data::v1::{ZulipGroupMember, ZulipStreamMember};

use secrecy::SecretString;
use std::collections::BTreeMap;

pub(crate) struct SyncZulip {
    zulip_controller: ZulipController,
    stream_definitions: BTreeMap<String, Vec<u64>>,
    user_group_definitions: BTreeMap<String, Vec<u64>>,
}

impl SyncZulip {
    pub(crate) fn new(
        username: String,
        token: SecretString,
        team_api: &TeamApi,
        dry_run: bool,
    ) -> anyhow::Result<Self> {
        let zulip_api = ZulipApi::new(username, token, dry_run);
        let mut stream_definitions = get_stream_definitions(team_api, &zulip_api)?;
        let user_group_definitions = get_user_group_definitions(team_api, &zulip_api)?;
        let zulip_controller = ZulipController::new(zulip_api)?;
        // rust-lang-owner is the user who owns the Zulip token.
        // This user needs to be in private streams to be able to
        // add/remove members.
        // Since this user is not in the team repo, we need to add
        // it manually.
        add_rust_lang_owner_to_private_streams(&mut stream_definitions, &zulip_controller)?;
        Ok(Self {
            zulip_controller,
            stream_definitions,
            user_group_definitions,
        })
    }

    pub(crate) fn diff_all(&self) -> anyhow::Result<Diff> {
        let stream_membership_diffs = self
            .stream_definitions
            .iter()
            .filter_map(|(stream_name, member_ids)| {
                self.diff_stream_membership(stream_name, member_ids)
                    .transpose()
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let user_group_diffs = self
            .user_group_definitions
            .iter()
            .filter_map(|(user_group_name, member_ids)| {
                self.diff_user_group(user_group_name, member_ids)
                    .transpose()
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Diff {
            user_group_diffs,
            stream_membership_diffs,
        })
    }

    fn diff_user_group(
        &self,
        user_group_name: &str,
        member_ids: &[u64],
    ) -> anyhow::Result<Option<UserGroupDiff>> {
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
                return Ok(Some(UserGroupDiff::Create(CreateUserGroupDiff {
                    name: user_group_name.to_owned(),
                    description: format!("The {user_group_name} team (managed by the Team repo)"),
                    member_ids: member_ids.to_owned(),
                })));
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
        if add_ids.is_empty() && remove_ids.is_empty() {
            log::debug!(
                "'{user_group_name}' user group ({user_group_id}) does not need to be updated"
            );
            Ok(None)
        } else {
            Ok(Some(UserGroupDiff::Update(UpdateUserGroupDiff {
                name: user_group_name.to_owned(),
                user_group_id,
                member_id_additions: add_ids,
                member_id_deletions: remove_ids,
            })))
        }
    }

    fn diff_stream_membership(
        &self,
        stream_name: &str,
        member_ids: &[u64],
    ) -> anyhow::Result<Option<StreamMembershipDiff>> {
        let stream_id = match self.zulip_controller.stream_id_from_name(stream_name) {
            Some(id) => {
                log::debug!("'{stream_name}' stream ({id}) found on Zulip");
                id
            }
            None => {
                log::error!("no '{stream_name}' user group found on Zulip");
                return Ok(None);
            }
        };
        let is_stream_private = self.zulip_controller.is_stream_private(stream_id)?;

        let existing_members = self.zulip_controller.stream_members_from_id(stream_id)?;
        log::debug!(
            "'{stream_name}' stream ({stream_id}) has members on Zulip {existing_members:?} and needs to have {member_ids:?}",
        );
        let add_ids = member_ids
            .iter()
            .filter(|i| !existing_members.contains(i))
            .copied()
            .collect::<Vec<_>>();
        let remove_ids = if is_stream_private {
            existing_members
                .iter()
                .filter(|i| !member_ids.contains(i))
                .copied()
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        if add_ids.is_empty() && remove_ids.is_empty() {
            log::debug!("'{stream_name}' stream ({stream_id}) does not need to be updated");
            Ok(None)
        } else {
            Ok(Some(StreamMembershipDiff::Update(
                UpdateStreamMembershipDiff {
                    stream_name: stream_name.to_owned(),
                    stream_id,
                    member_id_additions: add_ids,
                    member_id_deletions: remove_ids,
                },
            )))
        }
    }
}

fn add_rust_lang_owner_to_private_streams(
    stream_definitions: &mut BTreeMap<String, Vec<u64>>,
    zulip_controller: &ZulipController,
) -> anyhow::Result<()> {
    // Id of the `rust-lang-owner` Zulip user.
    let rust_lang_owner_id = 494485;
    for (stream_name, members) in stream_definitions {
        let stream_id = zulip_controller
            .stream_id_from_name(stream_name)
            .with_context(|| {
                format!(
                    "Id of stream '{stream_name}' not found. \
                     The stream probably doesn't exist and sync-team doesn't support creating it yet. \
                     Please create the stream manually and add the rust-lang-owner user to it."
                )
            })?;
        let is_stream_private = zulip_controller.zulip_api.is_stream_private(stream_id)?;
        if is_stream_private {
            members.insert(0, rust_lang_owner_id);
        }
    }
    Ok(())
}

pub(crate) struct Diff {
    user_group_diffs: Vec<UserGroupDiff>,
    stream_membership_diffs: Vec<StreamMembershipDiff>,
}

impl Diff {
    pub(crate) fn apply(&self, sync: &SyncZulip) -> anyhow::Result<()> {
        for user_group_diff in &self.user_group_diffs {
            user_group_diff.apply(sync)?;
        }
        for stream_membership_diff in &self.stream_membership_diffs {
            stream_membership_diff.apply(sync)?;
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.user_group_diffs.is_empty() && self.stream_membership_diffs.is_empty()
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !&self.user_group_diffs.is_empty() {
            writeln!(f, "💻 User Group Diffs:")?;
            for team_diff in &self.user_group_diffs {
                write!(f, "{team_diff}")?;
            }
        }

        if !&self.stream_membership_diffs.is_empty() {
            writeln!(f, "💻 Stream Membership Diffs:")?;
            for stream_membership_diff in &self.stream_membership_diffs {
                write!(f, "{stream_membership_diff}")?;
            }
        }

        Ok(())
    }
}

enum StreamMembershipDiff {
    Update(UpdateStreamMembershipDiff),
}

impl StreamMembershipDiff {
    fn apply(&self, sync: &SyncZulip) -> anyhow::Result<()> {
        match self {
            StreamMembershipDiff::Update(u) => u.apply(sync),
        }
    }
}

impl std::fmt::Display for StreamMembershipDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Update(u) => write!(f, "{u}"),
        }
    }
}

struct UpdateStreamMembershipDiff {
    stream_name: String,
    stream_id: u64,
    member_id_additions: Vec<u64>,
    member_id_deletions: Vec<u64>,
}

impl UpdateStreamMembershipDiff {
    fn apply(&self, sync: &SyncZulip) -> Result<(), anyhow::Error> {
        sync.zulip_controller.zulip_api.update_stream_membership(
            &self.stream_name,
            self.stream_id,
            &self.member_id_additions,
            &self.member_id_deletions,
        )
    }
}

impl std::fmt::Display for UpdateStreamMembershipDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "📝 Updating stream membership:")?;
        writeln!(f, "  Name: {}", self.stream_name)?;
        writeln!(f, "  ID: {}", self.stream_id)?;
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
    member_ids: Vec<u64>,
}

impl CreateUserGroupDiff {
    fn apply(&self, sync: &SyncZulip) -> Result<(), anyhow::Error> {
        sync.zulip_controller
            .create_user_group(&self.name, &self.description, &self.member_ids)
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
    user_group_id: u64,
    member_id_additions: Vec<u64>,
    member_id_deletions: Vec<u64>,
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
) -> anyhow::Result<BTreeMap<String, Vec<u64>>> {
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
                        id.copied()
                    }
                    ZulipGroupMember::Id(id) => Some(*id),
                })
                .collect::<Vec<_>>();
            (name, member_ids)
        })
        .collect();
    Ok(user_group_definitions)
}

/// Fetches the definitions of the user streams from the Team API
fn get_stream_definitions(
    team_api: &TeamApi,
    zulip_api: &ZulipApi,
) -> anyhow::Result<BTreeMap<String, Vec<u64>>> {
    let email_map = zulip_api
        .get_users()?
        .into_iter()
        .filter_map(|u| u.email.map(|e| (e, u.user_id)))
        .collect::<BTreeMap<_, _>>();
    let stream_definitions = team_api
        .get_zulip_streams()?
        .streams
        .into_iter()
        .map(|(name, stream)| {
            let members = &stream.members;
            let member_ids = members
                .iter()
                .filter_map(|member| match member {
                    ZulipStreamMember::Email(e) => {
                        let id = email_map.get(e);
                        if id.is_none() {
                            log::warn!("no Zulip id found for '{}'", e);
                        }
                        id.copied()
                    }
                    ZulipStreamMember::Id(id) => Some(*id),
                })
                .collect::<Vec<_>>();
            (name, member_ids)
        })
        .collect();
    Ok(stream_definitions)
}

/// Interacts with the Zulip API
struct ZulipController {
    /// User group name to Zulip user group id
    user_group_ids: BTreeMap<String, ZulipUserGroup>,
    /// Stream name to Zulip stream id
    stream_ids: BTreeMap<String, ZulipStream>,
    /// The Zulip API
    zulip_api: ZulipApi,
}

impl ZulipController {
    /// Create a new `ZulipController`
    fn new(zulip_api: ZulipApi) -> anyhow::Result<Self> {
        let streams = zulip_api.get_streams()?;
        let user_groups = zulip_api.get_user_groups()?;

        let stream_ids = streams
            .into_iter()
            .map(|st| (st.name.clone(), st))
            .collect();
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
            stream_ids,
            zulip_api,
        })
    }

    /// Get a user group id for the given user group name
    fn user_group_id_from_name(&self, user_group_name: &str) -> Option<u64> {
        self.user_group_ids.get(user_group_name).map(|u| u.id)
    }

    /// Get a stream id for the given stream name
    fn stream_id_from_name(&self, stream_name: &str) -> Option<u64> {
        self.stream_ids.get(stream_name).map(|st| st.stream_id)
    }

    /// Create a user group with a certain name, description, and members
    fn create_user_group(
        &self,
        user_group_name: &str,
        description: &str,
        member_ids: &[u64],
    ) -> anyhow::Result<()> {
        self.zulip_api
            .create_user_group(user_group_name, description, member_ids)?;

        Ok(())
    }

    /// Get the members of a user group given its name
    fn user_group_members_from_name(&self, user_group_name: &str) -> Option<Vec<u64>> {
        self.user_group_ids
            .get(user_group_name)
            .map(|u| u.members.to_owned())
    }

    /// Get the members of a stream given its id
    fn stream_members_from_id(&self, stream_id: u64) -> anyhow::Result<Vec<u64>> {
        self.zulip_api.get_stream_members(stream_id)
    }

    fn is_stream_private(&self, stream_id: u64) -> anyhow::Result<bool> {
        self.zulip_api.is_stream_private(stream_id)
    }
}
