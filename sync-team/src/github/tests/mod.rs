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
