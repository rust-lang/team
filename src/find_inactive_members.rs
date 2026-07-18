//! This binary serves as a utility to find members of Rust teams that haven't been active
//! on Zulip or GitHub for a long time.
//!
//! It should help members of the Leadership Council with their duty of periodically removing
//! inactive users (https://github.com/rust-lang/leadership-council/blob/main/policies/membership/auto-alumni.md).
use crate::api::github::{CommitInfo, GitHubApi, UserComment};
use crate::api::zulip::{MessageInfo, ZulipApi};
use crate::sync::team_api::TeamApi;
use chrono::Utc;
use futures_util::StreamExt;
use rust_team_data::v1;
use rust_team_data::v1::TeamKind;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub async fn find_inactive_members(
    team_filter: Option<String>,
    cutoff_days: u64,
) -> anyhow::Result<()> {
    let team_api = TeamApi::Production;
    let teams = team_api.get_teams().await?;

    let users = find_team_members(&team_api, &teams, team_filter).await?;
    println!("Found {} team members", users.len());

    let zulip_api = ZulipApi::new();
    let zulip_api = &zulip_api;

    let gh_api = GitHubApi::new();
    let gh_api = &gh_api;

    let cache = UserCache::new(Path::new(".user-cache"));
    let cache = &cache;

    let user_count = users.len();
    let mut stream = futures_util::stream::iter(users.into_iter().map(|user| async move {
        if let Ok(info) = cache.load(&user.username) {
            return (user, info);
        }

        let last_github_comments = gh_api
            .recent_user_comments_in_org(&user.username, "rust-lang", 3)
            .await
            .expect("Cannot fetch GitHub comment activity");

        // We search for commits, because finding issues/PRs is more expensive in terms of rate
        // limits
        let last_github_commits = gh_api
            .recent_user_commits_in_org(&user.username, "rust-lang", 3)
            .await
            .expect("Cannot fetch GitHub commit activity");

        let last_zulip_messages = if let Some(zulip_id) = user.zulip_id {
            zulip_api
                .get_last_n_messages_sent_by_user(zulip_id, 3)
                .await
                .expect("Cannot fetch Zulip messages")
        } else {
            vec![]
        };

        // To give more leeways for rate limits
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let info = UserInfo {
            last_zulip_messages,
            last_github_comments,
            last_github_commits,
        };
        cache
            .store(&user.username, info.clone())
            .expect("Cannot write cache entry");

        (user, info)
    }))
    .buffer_unordered(5);

    let mut users = Vec::new();
    while let Some((user, info)) = stream.next().await {
        users.push((user, info));
        if users.len() % 10 == 0 {
            eprintln!(
                "{}/{user_count} ({:.0}%)",
                users.len(),
                (users.len() as f64 / user_count as f64) * 100.0
            );
        }
    }
    eprintln!("Download finished\n");
    print_results(&teams, users, cutoff_days).await?;

    Ok(())
}

async fn find_team_members(
    team_api: &TeamApi,
    teams: &[v1::Team],
    team_filter: Option<String>,
) -> anyhow::Result<Vec<User>> {
    let team_teams = teams
        .iter()
        .filter(|team| match team.kind {
            TeamKind::Team => true,
            TeamKind::WorkingGroup => false,
            TeamKind::ProjectGroup => false,
            TeamKind::MarkerTeam => false,
            TeamKind::Unknown => false,
        })
        .filter(|team| {
            if let Some(filter) = &team_filter {
                team.name.contains(filter)
            } else {
                true
            }
        })
        .collect::<Vec<_>>();

    let mut team_members = HashSet::new();
    for team in team_teams {
        team_members.extend(team.members.iter().map(|m| (m.github.clone(), m.github_id)));
    }

    let zulip_map = team_api.get_zulip_map().await?;
    let gh_id_to_zulip_id: HashMap<u64, u64> =
        zulip_map.users.into_iter().map(|(k, v)| (v, k)).collect();

    let mut users = Vec::new();
    for (username, github_id) in team_members {
        let zulip_id = gh_id_to_zulip_id.get(&github_id).copied();
        let user = User {
            username,
            github_id,
            zulip_id,
        };
        users.push(user);
    }
    Ok(users)
}

async fn print_results(
    teams: &[v1::Team],
    mut users: Vec<(User, UserInfo)>,
    cutoff_days: u64,
) -> anyhow::Result<()> {
    const NEVER: u64 = 99999;

    // Keep only users who didn't have any Zulip public message or GitHub contribution in the past
    // `cutoff_days`.
    users.retain(|(_, info)| {
        info.zulip_age_days().unwrap_or(NEVER) > cutoff_days
            && info.github_comment_age_days().unwrap_or(NEVER) > cutoff_days
            && info.github_commit_age_days().unwrap_or(NEVER) > cutoff_days
    });
    eprintln!("Inactive users: {}", users.len());

    users.sort_by_key(|(_, info)| {
        // Sort by the largest minimum of these durations
        Reverse(
            info.zulip_age_days()
                .unwrap_or(NEVER)
                .min(info.github_comment_age_days().unwrap_or(NEVER))
                .min(info.github_commit_age_days().unwrap_or(NEVER)),
        )
    });

    let mut person_to_teams: HashMap<String, Vec<String>> = HashMap::new();
    for team in teams {
        if team.name == "all" || team.name == "leads" {
            continue;
        }
        for member in &team.members {
            person_to_teams
                .entry(member.github.clone())
                .or_default()
                .push(team.name.clone());
        }
    }

    let now = Utc::now();
    for (user, info) in users {
        let comments = if info.last_github_comments.is_empty() {
            "never".to_string()
        } else {
            format!(
                "{} days ago",
                info.last_github_comments
                    .iter()
                    .filter_map(|c| c.created_at)
                    .map(|d| now.signed_duration_since(d).num_days().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let commits = if info.last_github_commits.is_empty() {
            "never".to_string()
        } else {
            format!(
                "{} days ago",
                info.last_github_commits
                    .iter()
                    .map(|c| c.created_at)
                    .map(|d| now.signed_duration_since(d).num_days().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        println!(
            r#"**{}**
    - Zulip: {}{}
    - GitHub comments (rust-lang): {comments}
    - GitHub commits (rust-lang): {commits}
    - Teams: {}
"#,
            user.username,
            info.zulip_age_days()
                .map(|s| format!("{s} days ago"))
                .unwrap_or_else(|| "never".to_string()),
            if user.zulip_id.is_none() {
                " (no Zulip account)"
            } else {
                ""
            },
            person_to_teams
                .get(&user.username)
                .cloned()
                .unwrap_or_default()
                .join(", ")
        );
    }
    Ok(())
}

struct UserCache {
    directory: PathBuf,
}

impl UserCache {
    fn new(path: &Path) -> Self {
        std::fs::create_dir_all(path).unwrap();
        Self {
            directory: path.to_path_buf(),
        }
    }

    fn load(&self, username: &str) -> anyhow::Result<UserInfo> {
        let data = std::fs::read(self.path(username))?;
        let entry: CacheEntry = serde_json::from_slice(&data)?;

        // If the cache is too old, do not use it
        if Utc::now().signed_duration_since(entry.timestamp) > chrono::Duration::days(30) {
            Err(anyhow::anyhow!("Cache entry for {username} is too old"))
        } else {
            Ok(entry.info)
        }
    }

    fn store(&self, username: &str, info: UserInfo) -> anyhow::Result<()> {
        let data = serde_json::to_string(&CacheEntry {
            timestamp: Utc::now(),
            info,
        })?;
        std::fs::write(self.path(username), data)?;
        Ok(())
    }

    fn path(&self, username: &str) -> PathBuf {
        self.directory.join(format!("{username}.json"))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    timestamp: chrono::DateTime<Utc>,
    info: UserInfo,
}

#[derive(PartialEq, Eq, Hash, Debug)]
struct User {
    username: String,
    github_id: u64,
    zulip_id: Option<u64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct UserInfo {
    last_zulip_messages: Vec<MessageInfo>,
    last_github_comments: Vec<UserComment>,
    last_github_commits: Vec<CommitInfo>,
}

impl UserInfo {
    fn zulip_age_days(&self) -> Option<u64> {
        self.last_zulip_messages
            .first()
            .map(|msg| Utc::now().signed_duration_since(msg.timestamp).num_days() as u64)
    }

    fn github_comment_age_days(&self) -> Option<u64> {
        self.last_github_comments
            .iter()
            .filter_map(|comment| comment.created_at)
            .next()
            .map(|date| Utc::now().signed_duration_since(date).num_days() as u64)
    }

    fn github_commit_age_days(&self) -> Option<u64> {
        self.last_github_commits
            .iter()
            .map(|commit| commit.created_at)
            .next()
            .map(|date| Utc::now().signed_duration_since(date).num_days() as u64)
    }
}
