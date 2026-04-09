use super::{construct_ruleset, log_ruleset};
use crate::sync::github::tests::test_utils::{
    BranchProtectionBuilder, DEFAULT_ORG, DataModel, RepoData, TeamData,
};
use rust_team_data::v1::{self, BranchProtectionMode, RepoPermission};

mod test_utils;

#[tokio::test]
async fn team_noop() {
    let model = DataModel::default();
    let gh = model.gh_model();
    let team_diff = model.diff_teams(gh).await;
    assert!(team_diff.is_empty());
}

#[tokio::test]
async fn team_create() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    let gh = model.gh_model();
    model.create_team(TeamData::new("admins").gh_team(DEFAULT_ORG, "admins-gh", &[user, user2]));
    let team_diff = model.diff_teams(gh).await;
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

#[tokio::test]
async fn team_add_member() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    model.create_team(TeamData::new("admins").gh_team(DEFAULT_ORG, "admins-gh", &[user]));
    let gh = model.gh_model();

    model.get_team("admins").add_gh_member("admins-gh", user2);
    let team_diff = model.diff_teams(gh).await;
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

#[tokio::test]
async fn team_dont_add_member_if_invitation_is_pending() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    model.create_team(TeamData::new("admins").gh_team(DEFAULT_ORG, "admins-gh", &[user]));
    let mut gh = model.gh_model();

    model.get_team("admins").add_gh_member("admins-gh", user2);
    gh.add_invitation(DEFAULT_ORG, "admins-gh", "jan");

    let team_diff = model.diff_teams(gh).await;
    insta::assert_debug_snapshot!(team_diff, @"[]");
}

#[tokio::test]
async fn remove_org_members() {
    let mut model = DataModel::default();
    let rust_lang_org = "rust-lang";
    let user = model.create_user("sakura");
    model.create_team(TeamData::new("team-1").gh_team(rust_lang_org, "members-gh", &[user]));
    let mut gh = model.gh_model();
    gh.add_member(rust_lang_org, "martin");

    // Add a bot that shouldn't be removed from the org.
    let bot = "my-bot";
    gh.add_member(rust_lang_org, bot);
    model.add_allowed_org_member(bot);

    let gh_org_diff = model.diff_org_membership(gh).await;

    insta::assert_debug_snapshot!(gh_org_diff, @r#"
    [
        OrgMembershipDiff {
            org: "rust-lang",
            members_to_remove: [
                "martin",
            ],
        },
    ]
    "#);
}

#[tokio::test]
async fn team_remove_member() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");
    let user2 = model.create_user("jan");
    model.create_team(TeamData::new("admins").gh_team(DEFAULT_ORG, "admins-gh", &[user, user2]));
    let gh = model.gh_model();

    model
        .get_team("admins")
        .remove_gh_member("admins-gh", user2);

    let team_diff = model.diff_teams(gh).await;
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

#[tokio::test]
async fn team_delete() {
    let mut model = DataModel::default();
    let user = model.create_user("mark");

    // We need at least two github teams, otherwise the diff for removing the last GH team
    // won't be generated, because no organization is known to scan for existing unmanaged teams.
    model.create_team(
        TeamData::new("admins")
            .gh_team(DEFAULT_ORG, "admins-gh", &[user])
            .gh_team(DEFAULT_ORG, "users-gh", &[user]),
    );
    let gh = model.gh_model();

    model.get_team("admins").remove_gh_team("users-gh");

    let team_diff = model.diff_teams(gh).await;
    insta::assert_debug_snapshot!(team_diff, @r#"
    [
        Delete(
            DeleteTeamDiff {
                org: "rust-lang",
                name: "users-gh",
                slug: "users-gh",
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_noop() {
    let model = DataModel::default();
    let gh = model.gh_model();
    let diff = model.diff_repos(gh).await;
    assert!(diff.is_empty());
}

#[tokio::test]
async fn repo_change_description() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").description("foo".to_string()));
    let gh = model.gh_model();
    model.get_repo("repo1").description = "bar".to_string();

    let diff = model.diff_repos(gh).await;
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
                        description: "foo",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "bar",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_change_homepage() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").homepage(Some("https://foo.rs".to_string())));
    let gh = model.gh_model();
    model.get_repo("repo1").homepage = Some("https://bar.rs".to_string());

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: Some(
                            "https://foo.rs",
                        ),
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: Some(
                            "https://bar.rs",
                        ),
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_create() {
    let mut model = DataModel::default();
    let gh = model.gh_model();

    model.create_repo(
        RepoData::new("repo1")
            .description("foo".to_string())
            .member("user1", RepoPermission::Write)
            .team("team1", RepoPermission::Triage)
            .branch_protections(vec![
                BranchProtectionBuilder::pr_required("main", &["test"], 1).build(),
            ]),
    );
    let diff = model.diff_repos(gh).await;
    insta::assert_debug_snapshot!(diff, @r#"
    [
        Create(
            CreateRepoDiff {
                org: "rust-lang",
                name: "repo1",
                settings: RepoSettings {
                    description: "foo",
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
                rulesets: [
                    Ruleset {
                        id: None,
                        name: "main",
                        target: Branch,
                        source_type: Repository,
                        enforcement: Active,
                        bypass_actors: [],
                        conditions: RulesetConditions {
                            ref_name: RulesetRefNameCondition {
                                include: [
                                    "refs/heads/main",
                                ],
                                exclude: [],
                            },
                        },
                        rules: {
                            Creation,
                            Deletion,
                            PullRequest {
                                parameters: PullRequestParameters {
                                    dismiss_stale_reviews_on_push: false,
                                    require_code_owner_review: false,
                                    require_last_push_approval: false,
                                    required_approving_review_count: 1,
                                    required_review_thread_resolution: false,
                                },
                            },
                            RequiredStatusChecks {
                                parameters: RequiredStatusChecksParameters {
                                    do_not_enforce_on_create: Some(
                                        false,
                                    ),
                                    required_status_checks: [
                                        RequiredStatusCheck {
                                            context: "test",
                                            integration_id: Some(
                                                15368,
                                            ),
                                        },
                                    ],
                                    strict_required_status_checks_policy: false,
                                },
                            },
                            NonFastForward,
                        },
                    },
                ],
                environments: [],
                app_installations: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_add_member() {
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

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
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
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_change_member_permissions() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").member("user1", RepoPermission::Write));

    let gh = model.gh_model();
    model
        .get_repo("repo1")
        .members
        .last_mut()
        .unwrap()
        .permission = RepoPermission::Triage;

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
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
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_remove_member() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").member("user1", RepoPermission::Write));

    let gh = model.gh_model();
    model.get_repo("repo1").members.clear();

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
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
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_add_team() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").member("user1", RepoPermission::Write));

    let gh = model.gh_model();
    model
        .get_repo("repo1")
        .add_team("team1", RepoPermission::Triage);

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
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
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_change_team_permissions() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").team("team1", RepoPermission::Triage));

    let gh = model.gh_model();
    model.get_repo("repo1").teams.last_mut().unwrap().permission = RepoPermission::Admin;

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
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
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_remove_team() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").team("team1", RepoPermission::Write));

    let gh = model.gh_model();
    model.get_repo("repo1").teams.clear();

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
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
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_archive_repo() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1"));

    let gh = model.gh_model();
    model.get_repo("repo1").archived = true;

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: true,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_add_branch_protection() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1").team("team1", RepoPermission::Write));

    let gh = model.gh_model();
    model.get_repo("repo1").branch_protections.extend([
        BranchProtectionBuilder::pr_required("master", &["test", "test 2"], 0).build(),
        BranchProtectionBuilder::pr_not_required("beta").build(),
    ]);

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [
                    RulesetDiff {
                        name: "master",
                        operation: Create(
                            Ruleset {
                                id: None,
                                name: "master",
                                target: Branch,
                                source_type: Repository,
                                enforcement: Active,
                                bypass_actors: [],
                                conditions: RulesetConditions {
                                    ref_name: RulesetRefNameCondition {
                                        include: [
                                            "refs/heads/master",
                                        ],
                                        exclude: [],
                                    },
                                },
                                rules: {
                                    Creation,
                                    Deletion,
                                    PullRequest {
                                        parameters: PullRequestParameters {
                                            dismiss_stale_reviews_on_push: false,
                                            require_code_owner_review: false,
                                            require_last_push_approval: false,
                                            required_approving_review_count: 0,
                                            required_review_thread_resolution: false,
                                        },
                                    },
                                    RequiredStatusChecks {
                                        parameters: RequiredStatusChecksParameters {
                                            do_not_enforce_on_create: Some(
                                                false,
                                            ),
                                            required_status_checks: [
                                                RequiredStatusCheck {
                                                    context: "test",
                                                    integration_id: Some(
                                                        15368,
                                                    ),
                                                },
                                                RequiredStatusCheck {
                                                    context: "test 2",
                                                    integration_id: Some(
                                                        15368,
                                                    ),
                                                },
                                            ],
                                            strict_required_status_checks_policy: false,
                                        },
                                    },
                                    NonFastForward,
                                },
                            },
                        ),
                    },
                    RulesetDiff {
                        name: "beta",
                        operation: Create(
                            Ruleset {
                                id: None,
                                name: "beta",
                                target: Branch,
                                source_type: Repository,
                                enforcement: Active,
                                bypass_actors: [],
                                conditions: RulesetConditions {
                                    ref_name: RulesetRefNameCondition {
                                        include: [
                                            "refs/heads/beta",
                                        ],
                                        exclude: [],
                                    },
                                },
                                rules: {
                                    Creation,
                                    Deletion,
                                    NonFastForward,
                                },
                            },
                        ),
                    },
                ],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_update_branch_protection() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .team("team1", RepoPermission::Write)
            .branch_protections(vec![
                BranchProtectionBuilder::pr_required("master", &["test"], 1).build(),
            ]),
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
    protection.require_conversation_resolution = true;
    protection.require_linear_history = true;
    protection.prevent_force_push = false;
    protection.require_up_to_date_branches = true;

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [
                    RulesetDiff {
                        name: "master",
                        operation: Update(
                            0,
                            Ruleset {
                                id: Some(
                                    0,
                                ),
                                name: "master",
                                target: Branch,
                                source_type: Repository,
                                enforcement: Active,
                                bypass_actors: [],
                                conditions: RulesetConditions {
                                    ref_name: RulesetRefNameCondition {
                                        include: [
                                            "refs/heads/master",
                                        ],
                                        exclude: [],
                                    },
                                },
                                rules: {
                                    Creation,
                                    Deletion,
                                    PullRequest {
                                        parameters: PullRequestParameters {
                                            dismiss_stale_reviews_on_push: false,
                                            require_code_owner_review: false,
                                            require_last_push_approval: false,
                                            required_approving_review_count: 1,
                                            required_review_thread_resolution: false,
                                        },
                                    },
                                    RequiredStatusChecks {
                                        parameters: RequiredStatusChecksParameters {
                                            do_not_enforce_on_create: Some(
                                                false,
                                            ),
                                            required_status_checks: [
                                                RequiredStatusCheck {
                                                    context: "test",
                                                    integration_id: Some(
                                                        15368,
                                                    ),
                                                },
                                            ],
                                            strict_required_status_checks_policy: false,
                                        },
                                    },
                                    NonFastForward,
                                },
                            },
                            Ruleset {
                                id: None,
                                name: "master",
                                target: Branch,
                                source_type: Repository,
                                enforcement: Active,
                                bypass_actors: [],
                                conditions: RulesetConditions {
                                    ref_name: RulesetRefNameCondition {
                                        include: [
                                            "refs/heads/master",
                                        ],
                                        exclude: [],
                                    },
                                },
                                rules: {
                                    Creation,
                                    Deletion,
                                    RequiredLinearHistory,
                                    PullRequest {
                                        parameters: PullRequestParameters {
                                            dismiss_stale_reviews_on_push: true,
                                            require_code_owner_review: false,
                                            require_last_push_approval: false,
                                            required_approving_review_count: 0,
                                            required_review_thread_resolution: true,
                                        },
                                    },
                                    RequiredStatusChecks {
                                        parameters: RequiredStatusChecksParameters {
                                            do_not_enforce_on_create: Some(
                                                false,
                                            ),
                                            required_status_checks: [
                                                RequiredStatusCheck {
                                                    context: "Test",
                                                    integration_id: Some(
                                                        15368,
                                                    ),
                                                },
                                                RequiredStatusCheck {
                                                    context: "test",
                                                    integration_id: Some(
                                                        15368,
                                                    ),
                                                },
                                            ],
                                            strict_required_status_checks_policy: true,
                                        },
                                    },
                                },
                            },
                        ),
                    },
                ],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_remove_branch_protection() {
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

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [
                    RulesetDiff {
                        name: "stable",
                        operation: Delete(
                            1,
                        ),
                    },
                ],
                environment_diffs: [],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[test]
fn ruleset_creation_logs_non_default_disabled_flags() {
    let mut protection = BranchProtectionBuilder::pr_not_required("main").build();

    // Change defaults
    protection.prevent_creation = false;
    protection.prevent_deletion = false;
    protection.prevent_force_push = false;

    let ruleset = construct_ruleset(&protection, vec![]);
    let mut rendered = String::new();
    log_ruleset(&ruleset, None, &mut rendered).unwrap();

    assert!(rendered.contains("Restrict creations: false"));
    assert!(rendered.contains("Restrict deletions: false"));
    assert!(rendered.contains("Forbid force pushes: false"));
    assert!(rendered.contains("Require pull requests: false"));
}

#[test]
fn ruleset_updates_log_disabled_toggle_rules_as_false() {
    let old = construct_ruleset(
        &BranchProtectionBuilder::pr_required("main", &["test"], 1).build(),
        vec![],
    );

    let mut new_protection = BranchProtectionBuilder::pr_required("main", &["test"], 1).build();

    // Change default
    new_protection.prevent_force_push = false;

    let new = construct_ruleset(&new_protection, vec![]);
    let mut rendered = String::new();
    log_ruleset(&old, Some(&new), &mut rendered).unwrap();

    assert!(rendered.contains("Forbid force pushes: true => false"));
}

#[tokio::test]
async fn independent_orgs_are_not_synced() {
    let mut model = DataModel::default();
    let user = model.create_user("sakura");

    let independent_org = "independent-org";

    // Create a team, so that membership is synced.
    model.create_team(TeamData::new("team").gh_team(independent_org, "team-gh", &[user]));

    let mut gh = model.gh_model();

    // Add a member who is not part of any team.
    gh.add_member(independent_org, "independent-user-1");

    model.add_independent_github_org(independent_org);

    let gh_org_diff = model.diff_org_membership(gh).await;

    // No members should be removed for independent organizations
    insta::assert_debug_snapshot!(gh_org_diff, @"[]");
}

#[tokio::test]
async fn repo_environment_noop() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .environment("production")
            .environment("staging"),
    );
    let gh = model.gh_model();
    let diff = model.diff_repos(gh).await;
    assert!(diff.is_empty());
}

#[tokio::test]
async fn repo_environment_create() {
    let mut model = DataModel::default();
    model.create_repo(RepoData::new("repo1"));
    let gh = model.gh_model();

    model.get_repo("repo1").environments.insert(
        "production".to_string(),
        v1::Environment {
            branches: vec![],
            tags: vec![],
        },
    );
    model.get_repo("repo1").environments.insert(
        "staging".to_string(),
        v1::Environment {
            branches: vec![],
            tags: vec![],
        },
    );

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [],
                environment_diffs: [
                    Create(
                        "production",
                        Environment {
                            branches: [],
                            tags: [],
                        },
                    ),
                    Create(
                        "staging",
                        Environment {
                            branches: [],
                            tags: [],
                        },
                    ),
                ],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_environment_delete() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .environment("production")
            .environment("staging"),
    );
    let gh = model.gh_model();

    model.get_repo("repo1").environments.clear();

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [],
                environment_diffs: [
                    Delete(
                        "production",
                    ),
                    Delete(
                        "staging",
                    ),
                ],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_environment_update() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1")
            .environment("production")
            .environment("staging"),
    );
    let gh = model.gh_model();

    // Remove staging, keep production, add dev
    model.get_repo("repo1").environments.clear();
    model.get_repo("repo1").environments.insert(
        "production".to_string(),
        v1::Environment {
            branches: vec![],
            tags: vec![],
        },
    );
    model.get_repo("repo1").environments.insert(
        "dev".to_string(),
        v1::Environment {
            branches: vec![],
            tags: vec![],
        },
    );

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [],
                environment_diffs: [
                    Create(
                        "dev",
                        Environment {
                            branches: [],
                            tags: [],
                        },
                    ),
                    Delete(
                        "staging",
                    ),
                ],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}

#[tokio::test]
async fn repo_environment_update_branches() {
    let mut model = DataModel::default();
    model.create_repo(
        RepoData::new("repo1").environment_with_branches("production", &["main", "release/*"]),
    );
    let gh = model.gh_model();

    // Update branches for production environment
    model.get_repo("repo1").environments.insert(
        "production".to_string(),
        v1::Environment {
            branches: vec!["main".to_string(), "stable".to_string()],
            tags: vec![],
        },
    );

    let diff = model.diff_repos(gh).await;
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
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                    RepoSettings {
                        description: "",
                        homepage: None,
                        archived: false,
                        auto_merge_enabled: false,
                    },
                ),
                permission_diffs: [],
                branch_protection_diffs: [],
                ruleset_diffs: [],
                environment_diffs: [
                    Update {
                        name: "production",
                        add_branches: [
                            "stable",
                        ],
                        remove_branches: [
                            "release/*",
                        ],
                        add_tags: [],
                        remove_tags: [],
                        new_branches: [
                            "main",
                            "stable",
                        ],
                        new_tags: [],
                    },
                ],
                app_installation_diffs: [],
            },
        ),
    ]
    "#);
}
