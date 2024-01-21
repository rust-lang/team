use crate::github::tests::test_utils::{BranchProtectionBuilder, DataModel, RepoData, TeamData};
use rust_team_data::v1::{BranchProtectionMode, RepoPermission};

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
fn repo_change_description() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").description("foo".to_string()));
    let gh = model.gh_model();
    model.get_repo("repo1").description = "bar".to_string();

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
                            "bar",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_change_homepage() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").homepage(Some("https://foo.rs".to_string())));
    let gh = model.gh_model();
    model.get_repo("repo1").homepage = Some("https://bar.rs".to_string());

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
                            "",
                        ),
                        homepage: Some(
                            "https://foo.rs",
                        ),
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: Some(
                            "https://bar.rs",
                        ),
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_create() {
    let mut model = DataModel::default();
    let gh = model.gh_model();

    model.create_repo(
        RepoData::new("repo1")
            .description("foo".to_string())
            .member("user1", RepoPermission::Write)
            .team("team1", RepoPermission::Triage)
            .branch_protections(vec![BranchProtectionBuilder::pr_required(
                "main",
                &["test"],
                1,
            )
            .build()]),
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
                branch_protections: [
                    (
                        "main",
                        BranchProtection {
                            pattern: "main",
                            is_admin_enforced: true,
                            dismisses_stale_reviews: false,
                            required_approving_review_count: 1,
                            required_status_check_contexts: [
                                "test",
                            ],
                            push_allowances: [],
                            requires_approving_reviews: true,
                        },
                    ),
                ],
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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
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
    model.create_repo(RepoData::new("repo1").member("user1", RepoPermission::Write));

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
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
    model.create_repo(RepoData::new("repo1").member("user1", RepoPermission::Write));

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
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

#[test]
fn repo_add_team() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").member("user1", RepoPermission::Write));

    let gh = model.gh_model();
    model
        .get_repo("repo1")
        .add_team("team1", RepoPermission::Triage);

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [
                    RepoPermissionAssignmentDiff {
                        collaborator: Team(
                            "team1",
                        ),
                        diff: Create(
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
fn repo_change_team_permissions() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").team("team1", RepoPermission::Triage));

    let gh = model.gh_model();
    model.get_repo("repo1").teams.last_mut().unwrap().permission = RepoPermission::Admin;

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [
                    RepoPermissionAssignmentDiff {
                        collaborator: Team(
                            "team1",
                        ),
                        diff: Update(
                            Triage,
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
fn repo_remove_team() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").team("team1", RepoPermission::Write));

    let gh = model.gh_model();
    model.get_repo("repo1").teams.clear();

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [
                    RepoPermissionAssignmentDiff {
                        collaborator: Team(
                            "team1",
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

#[test]
fn repo_archive_repo() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1"));

    let gh = model.gh_model();
    model.get_repo("repo1").archived = true;

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: None,
                        archived: true,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_add_branch_protection() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").team("team1", RepoPermission::Write));

    let gh = model.gh_model();
    model.get_repo("repo1").branch_protections.extend([
        BranchProtectionBuilder::pr_required("master", &["test", "test 2"], 0).build(),
        BranchProtectionBuilder::pr_not_required("beta").build(),
    ]);

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [
                    BranchProtectionDiff {
                        pattern: "master",
                        operation: Create(
                            BranchProtection {
                                pattern: "master",
                                is_admin_enforced: true,
                                dismisses_stale_reviews: false,
                                required_approving_review_count: 0,
                                required_status_check_contexts: [
                                    "test",
                                    "test 2",
                                ],
                                push_allowances: [],
                                requires_approving_reviews: true,
                            },
                        ),
                    },
                    BranchProtectionDiff {
                        pattern: "beta",
                        operation: Create(
                            BranchProtection {
                                pattern: "beta",
                                is_admin_enforced: true,
                                dismisses_stale_reviews: false,
                                required_approving_review_count: 0,
                                required_status_check_contexts: [],
                                push_allowances: [],
                                requires_approving_reviews: false,
                            },
                        ),
                    },
                ],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_update_branch_protection() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .team("team1", RepoPermission::Write)
            .branch_protections(vec![BranchProtectionBuilder::pr_required(
                "master",
                &["test"],
                1,
            )
            .build()]),
    );

    let gh = model.gh_model();
    let protection = model
        .get_repo("repo1")
        .branch_protections
        .last_mut()
        .unwrap();
    match &mut protection.mode {
        BranchProtectionMode::PrRequired {
            ci_checks,
            required_approvals,
        } => {
            ci_checks.push("Test".to_string());
            *required_approvals = 0;
        }
        BranchProtectionMode::PrNotRequired => unreachable!(),
    }
    protection.dismiss_stale_review = true;

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [
                    BranchProtectionDiff {
                        pattern: "master",
                        operation: Update(
                            "0",
                            BranchProtection {
                                pattern: "master",
                                is_admin_enforced: true,
                                dismisses_stale_reviews: false,
                                required_approving_review_count: 1,
                                required_status_check_contexts: [
                                    "test",
                                ],
                                push_allowances: [],
                                requires_approving_reviews: true,
                            },
                            BranchProtection {
                                pattern: "master",
                                is_admin_enforced: true,
                                dismisses_stale_reviews: true,
                                required_approving_review_count: 0,
                                required_status_check_contexts: [
                                    "test",
                                    "Test",
                                ],
                                push_allowances: [],
                                requires_approving_reviews: true,
                            },
                        ),
                    },
                ],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn repo_remove_branch_protection() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .team("team1", RepoPermission::Write)
            .branch_protections(vec![
                BranchProtectionBuilder::pr_required("main", &["test"], 1).build(),
                BranchProtectionBuilder::pr_required("stable", &["test"], 0).build(),
            ]),
    );

    let gh = model.gh_model();
    model.get_repo("repo1").branch_protections.pop().unwrap();

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
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: Some(
                            "",
                        ),
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [
                    BranchProtectionDiff {
                        pattern: "stable",
                        operation: Delete(
                            "1",
                        ),
                    },
                ],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}
