use crate::data::Data;
use crate::schema::RepoPermission;
use anyhow::{Context, bail};
use log::{debug, info, warn};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
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
        return Err(anyhow::anyhow!(
            "CODEOWNERS content is not up-to-date. Regenerate it using `cargo run ci generate-codeowners`."
        ));
    }

    Ok(())
}

/// Sensitive TOML data files.
/// PRs that modify them need to be approved by an infra-admin.
const PROTECTED_PATHS: &[&str] = &[
    "/repos/rust-lang/team.toml",
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
    description: Option<String>,
    homepage: Option<String>,
    fork: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct UntrackedRepo {
    org: String,
    name: String,
    description: String,
    homepage: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckUntrackedReposResult {
    AllTracked,
    MissingRepositoryConfigsCreated,
}

/// Check for untracked repositories and create missing configuration files
pub async fn check_untracked_repos(
    data: &Data,
    data_dir: &Path,
    create_missing: bool,
) -> anyhow::Result<CheckUntrackedReposResult> {
    let github = crate::api::github::GitHubApi::new_with_org_tokens();

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
    let github_repos = fetch_all_github_repos(&github, &orgs_to_monitor).await?;
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
        return Ok(CheckUntrackedReposResult::AllTracked);
    }

    warn!("❌ Found {} untracked repositories:", untracked.len());
    for repo in &untracked {
        warn!("  - {}/{}", repo.org, repo.name);
    }

    if create_missing {
        create_missing_repo_configs(data_dir, &untracked)?;
        return Ok(CheckUntrackedReposResult::MissingRepositoryConfigsCreated);
    }

    bail!(
        "Found {} untracked repositories. Please add them to the repos/ directory.",
        untracked.len()
    );
}

async fn fetch_all_github_repos(
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
                .get(Some(org), &url)
                .await
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
            description: repo.description.clone().unwrap_or_default(),
            homepage: repo
                .homepage
                .clone()
                .filter(|homepage| !homepage.is_empty()),
        })
        .collect()
}

#[derive(serde::Serialize)]
struct GeneratedRepoConfig<'a> {
    org: &'a str,
    name: &'a str,
    description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    homepage: Option<&'a str>,
    // schema::Repo has no Serde defaults for bots and access,
    // omitting them would make Data::load fail.
    bots: Vec<&'static str>,
    access: GeneratedRepoAccess,
}

#[derive(serde::Serialize)]
struct GeneratedRepoAccess {
    teams: BTreeMap<String, String>,
}

impl<'a> From<&'a UntrackedRepo> for GeneratedRepoConfig<'a> {
    fn from(repo: &'a UntrackedRepo) -> Self {
        Self {
            org: &repo.org,
            name: &repo.name,
            description: &repo.description,
            homepage: repo.homepage.as_deref(),
            bots: Vec::new(),
            access: GeneratedRepoAccess {
                teams: BTreeMap::new(),
            },
        }
    }
}

fn create_missing_repo_configs(data_dir: &Path, repos: &[UntrackedRepo]) -> anyhow::Result<()> {
    for repo in repos {
        let contents =
            toml::to_string_pretty(&GeneratedRepoConfig::from(repo)).with_context(|| {
                format!(
                    "failed to serialize configuration for {}/{}",
                    repo.org, repo.name
                )
            })?;

        let missing_repo_config_toml = data_dir
            .join("repos")
            .join(&repo.org)
            .join(format!("{}.toml", repo.name));

        std::fs::File::create_new(&missing_repo_config_toml)
            .with_context(|| format!("failed to create {missing_repo_config_toml:?}"))?
            .write_all(contents.as_bytes())
            .with_context(|| format!("failed to write {missing_repo_config_toml:?}"))?;

        info!("Created {}", missing_repo_config_toml.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{GitHubRepo, UntrackedRepo, create_missing_repo_configs, find_untracked_repos};
    use crate::schema::Repo;
    use std::collections::HashSet;

    #[test]
    fn finds_only_untracked_non_fork_repositories() {
        let github_repos = vec![
            (
                "rust-lang".into(),
                GitHubRepo {
                    name: "tracked".into(),
                    description: Some("Tracked repository".into()),
                    homepage: None,
                    fork: false,
                },
            ),
            (
                "rust-lang".into(),
                GitHubRepo {
                    name: "fork".into(),
                    description: None,
                    homepage: None,
                    fork: true,
                },
            ),
            (
                "rust-lang".into(),
                GitHubRepo {
                    name: "missing".into(),
                    description: Some("Missing repository".into()),
                    homepage: Some("https://example.com".into()),
                    fork: false,
                },
            ),
        ];
        let tracked = HashSet::from([("rust-lang".into(), "tracked".into())]);

        let untracked = find_untracked_repos(&github_repos, &tracked);

        assert_eq!(
            untracked,
            vec![UntrackedRepo {
                org: "rust-lang".into(),
                name: "missing".into(),
                description: "Missing repository".into(),
                homepage: Some("https://example.com".into()),
            }]
        );
    }

    #[test]
    fn creates_parseable_repository_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos/rust-lang/example.toml");

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let repo = UntrackedRepo {
            org: "rust-lang".into(),
            name: "example".into(),
            description: "An \"example\" repository\nwith two lines".into(),
            homepage: Some("https://example.com".into()),
        };

        create_missing_repo_configs(dir.path(), &[repo]).unwrap();

        assert!(path.exists());

        let contents = std::fs::read_to_string(path).unwrap();
        let parsed: Repo = toml::from_str(&contents).unwrap();

        assert_eq!(parsed.org, "rust-lang");
        assert_eq!(parsed.name, "example");
        assert_eq!(
            parsed.description,
            "An \"example\" repository\nwith two lines"
        );
        assert_eq!(parsed.homepage.as_deref(), Some("https://example.com"));
        assert!(parsed.bots.is_empty());
        assert!(parsed.access.teams.is_empty());
    }

    #[test]
    fn omits_empty_homepage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos/rust-lang/example.toml");

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let repo = UntrackedRepo {
            org: "rust-lang".into(),
            name: "example".into(),
            description: String::new(),
            homepage: None,
        };

        create_missing_repo_configs(dir.path(), &[repo]).unwrap();
        let contents = std::fs::read_to_string(path).unwrap();

        assert!(!contents.contains("homepage"));
    }

    #[test]
    fn does_not_overwrite_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos/rust-lang/example.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "existing contents").unwrap();
        let repo = UntrackedRepo {
            org: "rust-lang".into(),
            name: "example".into(),
            description: "new contents".into(),
            homepage: None,
        };

        create_missing_repo_configs(dir.path(), &[repo]).unwrap_err();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "existing contents");
    }
}
