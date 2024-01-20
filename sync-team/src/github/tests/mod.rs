use crate::github::tests::test_utils::{DataModel, TeamData};

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
    model.create_team(TeamData::new("admins").gh_team("admins", &[user, user2]));
    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Create(
            CreateTeamDiff {
                org: "rust-lang",
                name: "admins",
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
    model.create_team(TeamData::new("admins").gh_team("admins", &[user]));
    let gh = model.gh_model();

    model.get_team("admins").add_gh_member("admins", user2);
    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Edit(
            EditTeamDiff {
                org: "rust-lang",
                name: "admins",
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
    model.create_team(TeamData::new("admins").gh_team("admins", &[user]));
    let mut gh = model.gh_model();

    model.get_team("admins").add_gh_member("admins", user2);
    gh.add_invitation("admins", "jan");

    let team_diff = model.diff_teams(gh);
    insta::assert_debug_snapshot!(team_diff, @r###"
    [
        Edit(
            EditTeamDiff {
                org: "rust-lang",
                name: "admins",
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
