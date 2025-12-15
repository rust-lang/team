use crate::data::Data;
use crate::schema::RepoPermission;
use anyhow::{bail, Context};
use log::{debug, info, warn};
use std::collections::{BTreeSet, HashSet};
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
                    .unwrap_or_else(|| panic!("team {team} not found"))
                    .members(&data)
                    .unwrap_or_else(|_| panic!("team {team} members couldn't be loaded"))
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
        r#"
# Data files can be approved by users with write access.
# We don't list these users explicitly to avoid notifying all of them
# on every change to the data files.
/people/**/*.toml
/repos/**/*.toml
/teams/**/*.toml

# Do not require admin approvals for Markdown file modifications.
*.md
"#
    )
    .unwrap();

    // There are several data files that we want to be protected more
    // Notably, the properties of the team and sync-team repositories,
    // the infra-admins and team-repo-admins teams and also the
    // accounts of the infra-admins and team-repo-admins members.

    writeln!(
        codeowners,
        "# Modifying these files requires admin approval."
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

#[derive(Debug, serde::Deserialize)]
struct GitHubRepo {
    name: String,
    fork: bool,
}

#[derive(Debug)]
struct UntrackedRepo {
    org: String,
    name: String,
}

/// Check for untracked repositories and fail if any are found
pub fn check_untracked_repos(data: &Data) -> anyhow::Result<()> {
    let github = crate::api::github::GitHubApi::new();

    // Get allowed GitHub organizations from config instead of hardcoding
    let orgs_to_monitor: Vec<&str> = data
        .config()
        .allowed_github_orgs()
        .iter()
        .filter(|org| {
            // Exclude independent orgs that shouldn't be synchronized
            !data
                .config()
                .independent_github_orgs()
                .contains(org.as_str())
        })
        .map(|s| s.as_str())
        .collect();

    info!(
        "🔍 Checking for untracked repositories in organizations: {}",
        orgs_to_monitor.join(", ")
    );

    info!("Fetching repositories from GitHub...");
    let github_repos = fetch_all_github_repos(&github, &orgs_to_monitor)?;
    info!(
        "Found {} total repositories in GitHub organizations",
        github_repos.len()
    );

    info!("Parsing local TOML files...");
    let tracked_repos = parse_tracked_repos(data);
    info!(
        "Found {} tracked repositories in repos/ directory",
        tracked_repos.len()
    );

    info!("Comparing GitHub repos with tracked repos...");
    let untracked = find_untracked_repos(&github_repos, &tracked_repos);

    if untracked.is_empty() {
        info!("✅ All repositories are tracked!");
        return Ok(());
    }

    warn!("❌ Found {} untracked repositories:", untracked.len());
    for repo in &untracked {
        warn!("  - {}/{}", repo.org, repo.name);
    }

    bail!(
        "Found {} untracked repositories. Please add them to the repos/ directory.",
        untracked.len()
    );
}

fn fetch_all_github_repos(
    github: &crate::api::github::GitHubApi,
    orgs_to_monitor: &[&str],
) -> anyhow::Result<Vec<(String, GitHubRepo)>> {
    let mut all_repos = Vec::new();

    for org in orgs_to_monitor {
        debug!("Fetching repos for org: {}", org);
        let mut page = 1;

        loop {
            let url = format!("orgs/{}/repos?per_page=100&page={}", org, page);

            let repos: Vec<GitHubRepo> = github
                .get(&url)
                .with_context(|| format!("Failed to fetch repos for org: {}", org))?;

            if repos.is_empty() {
                break;
            }

            for repo in repos {
                all_repos.push((org.to_string(), repo));
            }

            page += 1;
        }
    }

    Ok(all_repos)
}

fn parse_tracked_repos(data: &Data) -> HashSet<(String, String)> {
    data.all_repos()
        .map(|repo| (repo.org.clone(), repo.name.clone()))
        .collect()
}

fn find_untracked_repos(
    github_repos: &[(String, GitHubRepo)],
    tracked_repos: &HashSet<(String, String)>,
) -> Vec<UntrackedRepo> {
    github_repos
        .iter()
        .filter(|(org, repo)| {
            // Skip forks
            if repo.fork {
                debug!("Skipping fork: {}/{}", org, repo.name);
                return false;
            }

            // Check if tracked
            !tracked_repos.contains(&(org.clone(), repo.name.clone()))
        })
        .map(|(org, repo)| UntrackedRepo {
            org: org.clone(),
            name: repo.name.clone(),
        })
        .collect()
}
