use crate::github::tests::test_utils::{DataModel, RepoData, TeamData};
use rust_team_data::v1::RepoPermission;

mod test_utils;

#[test]
fn team_noop() {
    let model = DataModel::default();
    let gh = model.gh_model();
    let team_diff = model.diff_teams(gh);
    assert!(team_diff.is_empty());
}

#[test]
fn team_create() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    let gh = model.gh_model();
    model.create_team(TeamData::new("admins").gh_team("admins-gh", &[user, user2]));
    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Create(
            CreateTeamDiff {
                org: "rust-lang",
                name: "admins-gh",
                description: "Managed by the rust-lang/team repository.",
                privacy: Closed,
                members: [
                    (
                        "mark",
                        Member,
                    ),
                    (
                        "jan",
                        Member,
                    ),
                ],
            },
        ),
    ]
    "###);
}

#[test]
fn team_add_member() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    model.create_team(TeamData::new("admins").gh_team("admins-gh", &[user]));
    let gh = model.gh_model();

    model.get_team("admins").add_gh_member("admins-gh", user2);
    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Edit(
            EditTeamDiff {
                org: "rust-lang",
                name: "admins-gh",
                name_diff: None,
                description_diff: None,
                privacy_diff: None,
                member_diffs: [
                    (
                        "mark",
                        Noop,
                    ),
                    (
                        "jan",
                        Create(
                            Member,
                        ),
                    ),
                ],
            },
        ),
    ]
    "###);
}

#[test]
fn team_dont_add_member_if_invitation_is_pending() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    model.create_team(TeamData::new("admins").gh_team("admins-gh", &[user]));
    let mut gh = model.gh_model();

    model.get_team("admins").add_gh_member("admins-gh", user2);
    gh.add_invitation("admins-gh", "jan");

    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Edit(
            EditTeamDiff {
                org: "rust-lang",
                name: "admins-gh",
                name_diff: None,
                description_diff: None,
                privacy_diff: None,
                member_diffs: [
                    (
                        "mark",
                        Noop,
                    ),
                    (
                        "jan",
                        Noop,
                    ),
                ],
            },
        ),
    ]
    "###);
}

#[test]
fn team_remove_member() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    model.create_team(TeamData::new("admins").gh_team("admins-gh", &[user, user2]));
    let gh = model.gh_model();

    model
        .get_team("admins")
        .remove_gh_member("admins-gh", user2);

    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Edit(
            EditTeamDiff {
                org: "rust-lang",
                name: "admins-gh",
                name_diff: None,
                description_diff: None,
                privacy_diff: None,
                member_diffs: [
                    (
                        "mark",
                        Noop,
                    ),
                    (
                        "jan",
                        Delete,
                    ),
                ],
            },
        ),
    ]
    "###);
}

#[test]
fn team_delete() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");

    // We need at least two github teams, otherwise the diff for removing the last GH team
    // won't be generated, because no organization is known to scan for existing unmanaged teams.
    model.create_team(
        TeamData::new("admins")
            .gh_team("admins-gh", &[user])
            .gh_team("users-gh", &[user]),
    );
    let gh = model.gh_model();

    model.get_team("admins").remove_gh_team("users-gh");

    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Edit(
            EditTeamDiff {
                org: "rust-lang",
                name: "admins-gh",
                name_diff: None,
                description_diff: None,
                privacy_diff: None,
                member_diffs: [
                    (
                        "mark",
                        Noop,
                    ),
                ],
            },
        ),
        Delete(
            DeleteTeamDiff {
                org: "rust-lang",
                name: "users-gh",
                slug: "users-gh",
            },
        ),
    ]
    "###);
}

#[test]
fn repo_noop() {
    let model = DataModel::default();
    let gh = model.gh_model();
    let diff = model.diff_repos(gh);
    assert!(diff.is_empty());
}

#[test]
fn repo_create() {
    let mut model = DataModel::default();
    let gh = model.gh_model();

    model.create_repo(
        RepoData::new("repo1")
            .description(Some("foo".to_string()))
            .member("user1", RepoPermission::Write)
            .team("team1", RepoPermission::Triage),
    );
    let diff = model.diff_repos(gh);
    insta::assert_debug_snapshot!(diff, @r#"
    [
        Create(
            CreateRepoDiff {
                org: "rust-lang",
                name: "repo1",
                settings: RepoSettings {
                    description: Some(
                        "foo",
                    ),
                    homepage: None,
                    archived: false,
                    auto_merge_enabled: false,
                },
                permissions: [
                    RepoPermissionAssignmentDiff {
                        collaborator: Team(
                            "team1",
                        ),
                        diff: Create(
                            Triage,
                        ),
                    },
                    RepoPermissionAssignmentDiff {
                        collaborator: User(
                            "user1",
                        ),
                        diff: Create(
                            Write,
                        ),
                    },
                ],
                branch_protections: [],
                app_installations: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_add_member() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .description(Some("foo".to_string()))
            .member("user1", RepoPermission::Write)
            .team("team1", RepoPermission::Triage),
    );

    let gh = model.gh_model();
    model
        .get_repo("repo1")
        .add_member("user2", RepoPermission::Admin);

    let diff = model.diff_repos(gh);
    insta::assert_debug_snapshot!(diff, @r#"
    [
        Update(
            UpdateRepoDiff {
                org: "rust-lang",
                name: "repo1",
                repo_node_id: "0",
                repo_id: 0,
                settings_diff: (
                    RepoSettings {
                        description: Some(
                            "foo",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "foo",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [
                    RepoPermissionAssignmentDiff {
                        collaborator: User(
                            "user2",
                        ),
                        diff: Create(
                            Admin,
                        ),
                    },
                ],
                branch_protection_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_change_member_permissions() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .description(Some("foo".to_string()))
            .member("user1", RepoPermission::Write),
    );

    let gh = model.gh_model();
    model
        .get_repo("repo1")
        .members
        .last_mut()
        .unwrap()
        .permission = RepoPermission::Triage;

    let diff = model.diff_repos(gh);
    insta::assert_debug_snapshot!(diff, @r#"
    [
        Update(
            UpdateRepoDiff {
                org: "rust-lang",
                name: "repo1",
                repo_node_id: "0",
                repo_id: 0,
                settings_diff: (
                    RepoSettings {
                        description: Some(
                            "foo",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "foo",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [
                    RepoPermissionAssignmentDiff {
                        collaborator: User(
                            "user1",
                        ),
                        diff: Update(
                            Write,
                            Triage,
                        ),
                    },
                ],
                branch_protection_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_remove_member() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .description(Some("foo".to_string()))
            .member("user1", RepoPermission::Write),
    );

    let gh = model.gh_model();
    model.get_repo("repo1").members.clear();

    let diff = model.diff_repos(gh);
    insta::assert_debug_snapshot!(diff, @r#"
    [
        Update(
            UpdateRepoDiff {
                org: "rust-lang",
                name: "repo1",
                repo_node_id: "0",
                repo_id: 0,
                settings_diff: (
                    RepoSettings {
                        description: Some(
                            "foo",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "foo",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [
                    RepoPermissionAssignmentDiff {
                        collaborator: User(
                            "user1",
                        ),
                        diff: Delete(
                            Write,
                        ),
                    },
                ],
                branch_protection_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}
