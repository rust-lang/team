use crate::data::Data;
use crate::schema::RepoPermission;
use anyhow::Context;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Generates the contents of `.github/CODEOWNERS`, based on
/// the infra admins in `infra-admins.toml`.
pub fn generate_codeowners_file(data: Data) -> anyhow::Result<()> {
    let codeowners_content = generate_codeowners_content(data);
    std::fs::write(codeowners_path(), codeowners_content).context("cannot write CODEOWNERS")?;
    Ok(())
}

/// Check if `.github/CODEOWNERS` are up-to-date, based on the
/// `infra-admins.toml` file.
pub fn check_codeowners(data: Data) -> anyhow::Result<()> {
    let expected_codeowners = generate_codeowners_content(data);
    let actual_codeowners =
        std::fs::read_to_string(codeowners_path()).context("cannot read CODEOWNERS")?;
    if expected_codeowners != actual_codeowners {
        return Err(anyhow::anyhow!("CODEOWNERS content is not up-to-date. Regenerate it using `cargo run ci generate-codeowners`."));
    }

    Ok(())
}

/// Sensitive TOML data files.
/// PRs that modify them need to be approved by an infra-admin.
const PROTECTED_PATHS: &[&str] = &[
    "/repos/rust-lang/team.toml",
    "/repos/rust-lang/sync-team.toml",
    "/repos/rust-lang/rust.toml",
    "/teams/infra-admins.toml",
    "/teams/team-repo-admins.toml",
];

/// We want to allow access to the data files to `team-repo-admins`
/// (maintainers), while requiring a review from `infra-admins` (admins)
/// for any other changes.
///
/// We also want to explicitly protect special data files.
fn generate_codeowners_content(data: Data) -> String {
    use std::fmt::Write;

    let mut codeowners = String::new();
    writeln!(
        codeowners,
        r#"# This is an automatically generated file
# Run `cargo run ci generate-codeowners` to regenerate it.
# Note that the file is scanned bottom-to-top and the first match wins.
"#
    )
    .unwrap();

    // For the admins, we use just the people directly listed
    // in the infra-admins.toml file, without resolving
    // other included members, just to be extra sure that no one else is included.
    let admins = data
        .team("infra-admins")
        .expect("infra-admins team not found")
        .raw_people()
        .members
        .iter()
        .map(|m| m.github.as_str())
        .collect::<Vec<&str>>();

    let team_repo = data
        .repos()
        .find(|r| r.org == "rust-lang" && r.name == "team")
        .expect("team repository not found");
    let mut maintainers = team_repo
        .access
        .individuals
        .iter()
        .filter_map(|(user, permission)| match permission {
            RepoPermission::Triage => None,
            RepoPermission::Write | RepoPermission::Maintain | RepoPermission::Admin => {
                Some(user.as_str())
            }
        })
        .collect::<Vec<&str>>();
    maintainers.extend(
        team_repo
            .access
            .teams
            .iter()
            .filter(|(_, permission)| match permission {
                RepoPermission::Triage => false,
                RepoPermission::Write | RepoPermission::Maintain | RepoPermission::Admin => true,
            })
            .flat_map(|(team, _)| {
                data.team(team)
                    .expect(&format!("team {team} not found"))
                    .members(&data)
                    .expect(&format!("team {team} members couldn't be loaded"))
            }),
    );

    let admin_list = admins
        .iter()
        .map(|admin| format!("@{admin}"))
        .collect::<Vec<_>>()
        .join(" ");

    // The codeowners content is parsed bottom-to-top, and the first
    // rule that is matched will be applied. We thus write the most
    // general rules first, and then include specific exceptions.

    // Any changes in the repo not matched by rules below need to have admin
    // approval
    writeln!(
        codeowners,
        r#"# If none of the rules below match, we apply this catch-all rule
# and require admin approval for such a change.
* {admin_list}"#
    )
    .unwrap();

    // Data files have no owner. This means that they can be approved by
    // maintainers (which we want), but at the same time all maintainers will
    // not be pinged if a PR modified these files (which we also want).
    writeln!(
        codeowners,
        "
# Data files can be approved by users with write access.
# We don't list these users explicitly to avoid notifying all of them
# on every change to the data files.
people/**/*.toml @rust-lang/team-repo-admins @rust-lang/mods {admin_list}
repos/**/*.toml  @rust-lang/team-repo-admins @rust-lang/mods {admin_list}
# Useful for teams without leaders.
teams/**/*.toml  @rust-lang/team-repo-admins @rust-lang/mods {admin_list}

# Do not require admin approvals for Markdown file modifications.
*.md

# Team leads can approve changes to their own team files."
    )
    .unwrap();

    // Add team leads as reviewers for their team files
    for (team_name, leads) in data.team_leads() {
        let leads_list = leads
            .iter()
            .map(|lead| format!("@{lead}"))
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(
            codeowners,
            "/teams/{team_name}.toml @rust-lang/team-repo-admins @rust-lang/mods {leads_list}"
        )
        .unwrap();
    }

    // There are several data files that we want to be protected more
    // Notably, the properties of the team and sync-team repositories,
    // the infra-admins and team-repo-admins teams and also the
    // accounts of the infra-admins and team-repo-admins members.

    writeln!(
        codeowners,
        "\n# Modifying these files requires admin approval."
    )
    .unwrap();

    let mut protected_paths: Vec<String> =
        PROTECTED_PATHS.iter().map(|&p| String::from(p)).collect();

    // Some users can be both admins and maintainers.
    let all_users = admins
        .iter()
        .chain(maintainers.iter())
        .collect::<BTreeSet<_>>();
    for user in all_users {
        protected_paths.push(format!("/people/{user}.toml"));
    }

    for path in protected_paths {
        writeln!(codeowners, "{path} {admin_list}").unwrap();
    }
    codeowners
}

fn codeowners_path() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join(".github")
        .join("CODEOWNERS")
}
