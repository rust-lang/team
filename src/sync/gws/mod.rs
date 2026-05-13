mod api;
#[cfg(test)]
mod tests;

use crate::sync::gws::api::{GoogleWorkspaceApiClient, Group, User, UserName};
use std::collections::BTreeSet;
use std::fmt::Debug;

pub(crate) const RUST_LANG_GWS_DOMAIN: &str = "rust-lang.org";

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub(crate) enum GoogleGroupDiff {
    Create(Group),
    Delete(Group),
}

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub(crate) enum GoogleUserDiff {
    Create(User),
    Delete(User),
}

/// A diff between the team repo and the state on Google Workspace
#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub(crate) struct GoogleWorkspaceDiff {
    google_groups: Vec<GoogleGroupDiff>,
    google_users: Vec<GoogleUserDiff>,
}

/// The engine that evaluates diffs between our current configuration and
/// the actual state in Google Workspace
#[allow(dead_code)]
pub(crate) struct SyncGoogleWorkspace {
    actual_users: Vec<User>,
    actual_groups: Vec<Group>,
    configured_teams: Vec<rust_team_data::v1::Team>,
}

#[allow(dead_code)]
impl SyncGoogleWorkspace {
    pub async fn new(
        teams: Vec<rust_team_data::v1::Team>,
        gws_api_client: Box<dyn GoogleWorkspaceApiClient>,
    ) -> anyhow::Result<Self> {
        let gws_users = gws_api_client.get_users().await?;
        let gws_groups = gws_api_client.get_groups().await?;
        let sync = Self {
            actual_users: gws_users,
            actual_groups: gws_groups,
            configured_teams: teams,
        };
        Ok(sync)
    }

    pub(crate) fn diff_all(&self) -> anyhow::Result<GoogleWorkspaceDiff> {
        let google_groups_diff = self.diff_groups()?;
        let google_users_diff = self.diff_users()?;

        let diff = GoogleWorkspaceDiff {
            google_groups: google_groups_diff,
            google_users: google_users_diff,
        };
        Ok(diff)
    }

    fn diff_groups(&self) -> anyhow::Result<Vec<GoogleGroupDiff>> {
        let declared_groups = self
            .configured_teams
            .iter()
            .filter(|team| team.google_workspace_saml_group.unwrap_or_default())
            .map(|gws| Group::new(&gws.name))
            .collect::<BTreeSet<_>>();

        let declared_emails = declared_groups
            .iter()
            .map(|group| group.email.as_str())
            .collect::<BTreeSet<_>>();

        let actual_saml_groups = self.actual_groups.iter().filter(|group| group.is_saml());

        let actual_emails = actual_saml_groups
            .clone()
            .map(|group| group.email.as_str())
            .collect::<BTreeSet<_>>();

        let additions = declared_groups
            .iter()
            .filter(|group| !actual_emails.contains(group.email.as_str()))
            .map(|group| GoogleGroupDiff::Create(group.clone()));

        let deletions = actual_saml_groups
            .filter(|group| !declared_emails.contains(group.email.as_str()))
            .map(|group| GoogleGroupDiff::Delete(group.clone()));

        let diffs = additions.chain(deletions).collect();
        Ok(diffs)
    }

    fn diff_users(&self) -> anyhow::Result<Vec<GoogleUserDiff>> {
        let declared_users = self
            .configured_teams
            .iter()
            .filter(|team| team.google_workspace_saml_group.unwrap_or_default())
            .flat_map(|team| team.members.iter())
            .filter_map(|member| {
                member.google_workspace.as_ref().map(|gws| User {
                    primary_email: format!("{}@{RUST_LANG_GWS_DOMAIN}", gws.account_handle),
                    name: UserName {
                        given_name: gws.first_name.to_string(),
                        family_name: gws.last_name.to_string(),
                    },
                })
            })
            .collect::<BTreeSet<_>>();

        let declared_emails = declared_users
            .iter()
            .map(|user| user.primary_email.as_str())
            .collect::<BTreeSet<_>>();

        let actual_emails = self
            .actual_users
            .iter()
            .map(|user| user.primary_email.as_str())
            .collect::<BTreeSet<_>>();

        let diffs = declared_users
            .iter()
            .filter(|user| !actual_emails.contains(user.primary_email.as_str()))
            .map(|user| GoogleUserDiff::Create(user.clone()))
            .chain(
                self.actual_users
                    .iter()
                    .filter(|user| !declared_emails.contains(user.primary_email.as_str()))
                    .map(|user| GoogleUserDiff::Delete(user.clone())),
            )
            .collect();

        Ok(diffs)
    }
}
