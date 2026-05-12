use crate::sync::gws::tests::test_utils::{
    google_group, google_user, privileged_member, privileged_team, run_sync,
};
use crate::sync::gws::{GoogleGroupDiff, GoogleUserDiff, GoogleWorkspaceDiff};

mod test_utils;

#[tokio::test]
async fn diff_spots_nothing() {
    let google_users = vec![
        google_user("ubiratan", "soares"),
        google_user("marco", "ieni"),
    ];

    let google_groups = vec![
        google_group("infra-admins-saml"),
        google_group("security-response"), // groups not related to SAML are not diffed
    ];

    let teams = vec![privileged_team(
        "infra-admins",
        vec![
            privileged_member("ubiratan", "soares"),
            privileged_member("marco", "ieni"),
        ],
    )];

    let diff = run_sync(google_users, google_groups, teams).await;
    assert!(diff.google_users.is_empty());
    assert!(diff.google_groups.is_empty());
}

#[tokio::test]
async fn diff_spots_user_creation() {
    let google_users = vec![
        google_user("ubiratan", "soares"),
        google_user("marco", "ieni"),
    ];

    let google_groups = vec![google_group("infra-admins-saml")];

    let teams = vec![privileged_team(
        "infra-admins",
        vec![
            privileged_member("ubiratan", "soares"),
            privileged_member("marco", "ieni"),
            privileged_member("emily", "albini"),
        ],
    )];

    let diff = run_sync(google_users, google_groups, teams).await;
    let expected = vec![GoogleUserDiff::Create(google_user("emily", "albini"))];

    assert_eq!(diff.google_users, expected);
    assert!(diff.google_groups.is_empty());
}

#[tokio::test]
async fn diff_spots_user_deletion() {
    let google_users = vec![
        google_user("ubiratan", "soares"),
        google_user("marco", "ieni"),
        google_user("emily", "albini"),
    ];

    let google_groups = vec![google_group("infra-admins-saml")];

    let teams = vec![privileged_team(
        "infra-admins",
        vec![privileged_member("emily", "albini")],
    )];

    let diff = run_sync(google_users, google_groups, teams).await;

    let expected = vec![
        GoogleUserDiff::Delete(google_user("ubiratan", "soares")),
        GoogleUserDiff::Delete(google_user("marco", "ieni")),
    ];

    assert_eq!(diff.google_users, expected);
    assert!(diff.google_groups.is_empty());
}

#[tokio::test]
async fn diff_spots_group_creation() {
    let google_users = vec![
        google_user("ubiratan", "soares"),
        google_user("marco", "ieni"),
    ];

    let google_groups = vec![];

    let teams = vec![privileged_team(
        "infra-admins",
        vec![
            privileged_member("ubiratan", "soares"),
            privileged_member("marco", "ieni"),
        ],
    )];

    let diff = run_sync(google_users, google_groups, teams).await;
    let expected = vec![GoogleGroupDiff::Create(google_group("infra-admins-saml"))];

    assert!(diff.google_users.is_empty());
    assert_eq!(expected, diff.google_groups);
}

#[tokio::test]
async fn diff_spots_group_deletion() {
    let google_users = vec![
        google_user("ubiratan", "soares"),
        google_user("marco", "ieni"),
    ];

    let google_groups = vec![
        google_group("infra-admins-saml"),
        google_group("external-auditors-saml"),
    ];

    let teams = vec![privileged_team(
        "infra-admins",
        vec![
            privileged_member("ubiratan", "soares"),
            privileged_member("marco", "ieni"),
        ],
    )];

    let diff = run_sync(google_users, google_groups, teams).await;
    let expected = vec![GoogleGroupDiff::Delete(google_group(
        "external-auditors-saml",
    ))];

    assert!(diff.google_users.is_empty());
    assert_eq!(expected, diff.google_groups);
}

#[tokio::test]
async fn diff_spots_multiple_creations() {
    let google_users = vec![
        google_user("ubiratan", "soares"),
        google_user("marco", "ieni"),
    ];

    let google_groups = vec![google_group("infra-admins-saml")];

    let teams = vec![
        privileged_team(
            "infra-admins",
            vec![
                privileged_member("ubiratan", "soares"),
                privileged_member("marco", "ieni"),
            ],
        ),
        privileged_team(
            "crates-io-admins",
            vec![privileged_member("adam", "harvey")],
        ),
    ];

    let diff = run_sync(google_users, google_groups, teams).await;
    let expected = GoogleWorkspaceDiff {
        google_users: vec![GoogleUserDiff::Create(google_user("adam", "harvey"))],
        google_groups: vec![GoogleGroupDiff::Create(google_group(
            "crates-io-admins-saml",
        ))],
    };

    assert_eq!(expected, diff);
}

#[tokio::test]
async fn diff_spots_multiple_deletions() {
    let google_users = vec![
        google_user("ubiratan", "soares"),
        google_user("marco", "ieni"),
        google_user("adam", "harvey"),
    ];

    let google_groups = vec![
        google_group("infra-admins-saml"),
        google_group("crates-io-admins-saml"),
    ];

    let teams = vec![privileged_team(
        "infra-admins",
        vec![
            privileged_member("ubiratan", "soares"),
            privileged_member("marco", "ieni"),
        ],
    )];

    let diff = run_sync(google_users, google_groups, teams).await;
    let expected = GoogleWorkspaceDiff {
        google_users: vec![GoogleUserDiff::Delete(google_user("adam", "harvey"))],
        google_groups: vec![GoogleGroupDiff::Delete(google_group(
            "crates-io-admins-saml",
        ))],
    };

    assert_eq!(expected, diff);
}
