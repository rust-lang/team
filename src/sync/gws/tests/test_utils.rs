use crate::sync::gws::api::{GoogleWorkspaceApiClient, Group, User, UserName};
use crate::sync::gws::{GoogleWorkspaceDiff, RUST_LANG_GWS_DOMAIN, SyncGoogleWorkspace};
use async_trait::async_trait;
use rust_team_data::v1::{GoogleWorkspace, Team, TeamKind, TeamMember};

pub fn normal_member(name: &str) -> TeamMember {
    TeamMember {
        name: name.into(),
        github: name.into(),
        github_id: 1234567,
        is_lead: false,
        roles: vec![],
        google_workspace: None,
    }
}

pub fn privileged_member(name: &str, surname: &str) -> TeamMember {
    TeamMember {
        google_workspace: Some(GoogleWorkspace {
            first_name: name.into(),
            last_name: surname.into(),
            account_handle: format!("{name}.{surname}"),
        }),
        ..normal_member(name)
    }
}

pub fn normal_team(name: &str, members: Vec<TeamMember>) -> Team {
    Team {
        kind: TeamKind::Team,
        name: name.to_string(),
        github: None,
        website_data: None,
        subteam_of: None,
        top_level: Some(true),
        alumni: vec![],
        roles: vec![],
        google_workspace_saml_group: None,
        members,
    }
}

pub fn privileged_team(name: &str, members: Vec<TeamMember>) -> Team {
    Team {
        google_workspace_saml_group: Some(true),
        ..normal_team(name, members)
    }
}

pub fn google_user(name: &str, surname: &str) -> User {
    User {
        name: UserName {
            given_name: name.into(),
            family_name: surname.into(),
        },
        primary_email: format!("{name}.{surname}@{RUST_LANG_GWS_DOMAIN}"),
    }
}

pub fn google_group(name: &str) -> Group {
    Group {
        name: name.to_string(),
        email: format!("{name}@{RUST_LANG_GWS_DOMAIN}"),
    }
}

pub async fn run_sync(
    google_users: Vec<User>,
    google_groups: Vec<Group>,
    teams: Vec<Team>,
) -> GoogleWorkspaceDiff {
    let gws = FakeGoogleWorkspace {
        google_users,
        google_groups,
    };

    let sync = SyncGoogleWorkspace::new(teams, Box::new(gws))
        .await
        .expect("cannot create sync");

    let google_users_diff = sync.diff_users().expect("cannot diff accounts");
    let google_groups_diff = sync.diff_groups().expect("cannot diff groups");
    GoogleWorkspaceDiff {
        google_users: google_users_diff,
        google_groups: google_groups_diff,
    }
}

struct FakeGoogleWorkspace {
    pub google_users: Vec<User>,
    pub google_groups: Vec<Group>,
}

#[async_trait]
impl GoogleWorkspaceApiClient for FakeGoogleWorkspace {
    async fn get_users(&self) -> anyhow::Result<Vec<User>> {
        Ok(self.google_users.clone())
    }

    async fn get_groups(&self) -> anyhow::Result<Vec<Group>> {
        Ok(self.google_groups.clone())
    }
}
