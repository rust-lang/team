use crate::sync::GitHubTokens;
use crate::sync::utils::ResponseExt;
use anyhow::{Context, Error, bail};
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use chrono::{DateTime, Duration, Utc};
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, ClientBuilder, RequestBuilder};
use reqwest::{Method, StatusCode};
use secrecy::ExposeSecret;
use std::borrow::Cow;
use std::collections::HashMap;

static API_BASE: &str = "https://api.github.com/";
static TOKEN_VAR: &str = "GITHUB_TOKEN";

#[derive(serde::Deserialize)]
pub(crate) struct User {
    pub(crate) id: u64,
    pub(crate) login: String,
    pub(crate) name: Option<String>,
    pub(crate) email: Option<String>,
}

#[derive(serde::Deserialize)]
struct GraphResult<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphError>,
}

#[derive(serde::Deserialize)]
struct GraphError {
    message: String,
}

#[derive(serde::Deserialize)]
struct GraphNodes<T> {
    nodes: Vec<Option<T>>,
}

pub(crate) struct GitHubApi {
    http: Client,
    token: Option<String>,
    org_tokens: Option<GitHubTokens>,
}

impl GitHubApi {
    pub(crate) fn new() -> Self {
        GitHubApi {
            http: ClientBuilder::new()
                .user_agent(crate::USER_AGENT)
                .build()
                .unwrap(),
            token: std::env::var(TOKEN_VAR).ok(),
            org_tokens: None,
        }
    }

    pub(crate) fn new_with_org_tokens() -> Self {
        GitHubApi {
            http: ClientBuilder::new()
                .user_agent(crate::USER_AGENT)
                .build()
                .unwrap(),
            token: std::env::var(TOKEN_VAR).ok(),
            org_tokens: Some(GitHubTokens::from_env_org_tokens_only()),
        }
    }

    fn prepare(
        &self,
        require_auth: bool,
        org: Option<&str>,
        method: Method,
        url: &str,
    ) -> Result<RequestBuilder, Error> {
        let url = if url.starts_with("https://") {
            Cow::Borrowed(url)
        } else {
            Cow::Owned(format!("{API_BASE}{url}"))
        };
        if require_auth {
            self.require_auth()?;
        }

        let mut req = self.http.request(method, url.as_ref());
        let token = match org {
            Some(org) => self
                .org_tokens
                .as_ref()
                .and_then(|tokens| tokens.get_organization_token(org).ok())
                .map(|token| token.expose_secret()),
            None => self.token.as_deref(),
        };

        if let Some(token) = token {
            req = req.header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("token {token}"))?,
            );
        }
        Ok(req)
    }

    async fn graphql<R, V>(&self, query: &str, variables: V) -> Result<R, Error>
    where
        R: serde::de::DeserializeOwned,
        V: serde::Serialize,
    {
        #[derive(serde::Serialize)]
        struct Request<'a, V> {
            query: &'a str,
            variables: V,
        }
        let res: GraphResult<R> = self
            .prepare(true, None, Method::POST, "graphql")?
            .json(&Request { query, variables })
            .send()
            .await?
            .error_for_status()?
            .json_annotated()
            .await?;
        if let Some(error) = res.errors.first() {
            bail!("graphql error: {}", error.message);
        } else if let Some(data) = res.data {
            Ok(data)
        } else {
            bail!("missing graphql data");
        }
    }

    pub(crate) fn require_auth(&self) -> Result<(), Error> {
        if self.token.is_none() {
            bail!("missing environment variable {}", TOKEN_VAR);
        }
        Ok(())
    }

    pub(crate) async fn user(&self, login: &str) -> Result<User, Error> {
        self.prepare(false, None, Method::GET, &format!("users/{login}"))?
            .send()
            .await?
            .error_for_status()?
            .json_annotated()
            .await
    }

    pub(crate) async fn get<T>(&self, org: Option<&str>, url: &str) -> Result<T, Error>
    where
        T: serde::de::DeserializeOwned,
    {
        loop {
            let response = self.prepare(false, org, Method::GET, url)?.send().await?;

            let status = response.status();
            if status != StatusCode::OK {
                let headers = response.headers();

                // Rate limited
                if status == StatusCode::FORBIDDEN
                    && headers
                        .get("x-ratelimit-remaining")
                        .and_then(|v| v.to_str().ok())
                        == Some("0")
                {
                    let reset_at = headers
                        .get("x-ratelimit-reset")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|t| t.parse::<u64>().ok())
                        .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp as i64, 0))
                        .map(|d| d + chrono::Duration::seconds(1))
                        .unwrap_or(Utc::now() + chrono::Duration::minutes(1));
                    eprintln!("Rate limited. Waiting until {reset_at}");
                    let duration = reset_at
                        .signed_duration_since(Utc::now())
                        .max(Duration::zero());
                    tokio::time::sleep(duration.to_std().unwrap()).await;
                    continue;
                }

                let text = response.text().await?;
                return Err(anyhow::anyhow!("Request failed with {status}: {text}"));
            } else {
                return response.json_annotated().await;
            }
        }
    }

    pub(crate) async fn usernames(&self, ids: &[u64]) -> Result<HashMap<u64, String>, Error> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Usernames {
            database_id: u64,
            login: String,
        }
        #[derive(serde::Serialize)]
        struct Params {
            ids: Vec<String>,
        }
        static QUERY: &str = "
            query($ids: [ID!]!) {
                nodes(ids: $ids) {
                    ... on User {
                        databaseId
                        login
                    }
                }
            }
        ";

        let cant_resolve = |e: &Error| e.to_string().contains("Could not resolve to a node");

        let mut result = HashMap::new();
        for chunk in ids.chunks(100) {
            let res: GraphNodes<Usernames> = match self
                .graphql(
                    QUERY,
                    Params {
                        ids: chunk.iter().map(|id| user_node_id(*id)).collect(),
                    },
                )
                .await
            {
                Ok(res) => res,
                Err(e) => {
                    if cant_resolve(&e) {
                        // This error happens when a user is deleted. Provide
                        // a more helpful error message to pinpoint it:
                        for id in chunk {
                            if let Err(inner_e) = self
                                .graphql::<GraphNodes<Usernames>, Params>(
                                    QUERY,
                                    Params {
                                        ids: vec![user_node_id(*id)],
                                    },
                                )
                                .await
                            {
                                if cant_resolve(&inner_e) {
                                    bail!(
                                        "failed to resolve user id {}: {}\n\
                                        Check if the user has possibly deleted their account.",
                                        id,
                                        e
                                    );
                                } else {
                                    bail!(
                                        "failed to check resolve error: {}\n\
                                        Original error: {}",
                                        inner_e,
                                        e
                                    );
                                }
                            }
                        }
                    }
                    return Err(e);
                }
            };
            for node in res.nodes.into_iter().flatten() {
                result.insert(node.database_id, node.login);
            }
        }
        Ok(result)
    }

    pub(crate) async fn recent_user_comments_in_org(
        &self,
        username: &str,
        org: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<UserComment>> {
        // GitHub's GraphQL API doesn't seem to support filtering comments by author directly.
        // We can use two endpoints here - either use the search query and filter by commented and
        // organization, or use the user query and access its issueComments connection.
        // The user endpoint would be more efficient, in theory. However, if the user makes a lot of
        // comments in different organizations, we might load a lot of data before we get to their
        // comments in the given organization. So instead we use the search endpoint.

        // The endpoint loads issues (and PRs), not comments.
        // The `commenter:` filter guarantees each returned issue has at least one comment
        // from the user. So we fetch `limit` issues and a small number of recent comments
        // per issue, then filter to only the user's comments.
        let search_query = format!("commenter:{username} org:{org} sort:updated-desc");
        let issues_to_fetch = limit;
        let comments_per_issue = 100;

        let data = self
            .graphql::<serde_json::Value, _>(
                r#"
query($query: String!, $issueLimit: Int!, $commentLimit: Int!) {
  search(query: $query, type: ISSUE, first: $issueLimit) {
    nodes {
      ... on Issue {
        number
        url
        title
        repository {
          name
          owner { login }
        }
        comments(first: $commentLimit, orderBy: {field: UPDATED_AT, direction: DESC}) {
          nodes {
            author { login }
            body
            url
            createdAt
          }
        }
      }
      ... on PullRequest {
        number
        url
        title
        repository {
          name
          owner { login }
        }
        comments(first: $commentLimit, orderBy: {field: UPDATED_AT, direction: DESC}) {
          nodes {
            author { login }
            body
            url
            createdAt
          }
        }
      }
    }
  }
}
                "#,
                serde_json::json!({
                    "query": search_query,
                    "issueLimit": issues_to_fetch,
                    "commentLimit": comments_per_issue,
                }),
            )
            .await
            .context("failed to search for user comments")?;

        let mut all_comments: Vec<UserComment> = Vec::new();

        if let Some(nodes) = data["search"]["nodes"].as_array() {
            for node in nodes {
                let repo_owner = node["repository"]["owner"]["login"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let repo_name = node["repository"]["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let issue_number = node["number"].as_u64().unwrap_or(0);
                let issue_title = node["title"].as_str().unwrap_or("Unknown");
                let issue_url = node["url"].as_str().unwrap_or("");

                if let Some(comments) = node["comments"]["nodes"].as_array() {
                    for comment in comments {
                        // Filter to only comments by the target user
                        let author = comment["author"]["login"].as_str().unwrap_or("");
                        if !author.eq_ignore_ascii_case(username) {
                            continue;
                        }

                        let body = comment["body"].as_str().unwrap_or("");
                        let url = comment["url"].as_str().unwrap_or("");
                        let created_at = comment["createdAt"]
                            .as_str()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.with_timezone(&chrono::Utc));

                        all_comments.push(UserComment {
                            repo_owner: repo_owner.clone(),
                            repo_name: repo_name.clone(),
                            issue_number,
                            issue_title: issue_title.to_string(),
                            issue_url: issue_url.to_string(),
                            comment_url: url.to_string(),
                            body: body.to_string(),
                            created_at,
                        });
                    }
                }
            }
        }

        // Sort by creation date (most recent first) and take the limit
        all_comments.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        all_comments.truncate(limit);

        Ok(all_comments)
    }

    pub(crate) async fn recent_user_commits_in_org(
        &self,
        username: &str,
        org: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<CommitInfo>> {
        #[derive(serde::Deserialize, Debug)]
        struct Response {
            items: Vec<octocrab::models::search::CommitSearchResultItem>,
        }

        let response: Response = self.get(None, &format!("search/commits?q=author:{username}+org:{org}&sort=author-date&order=desc&per_page={limit}")).await?;
        Ok(response
            .items
            .into_iter()
            .filter_map(|c| {
                Some(CommitInfo {
                    repo_owner: c.repository.owner.login,
                    repo_name: c.repository.name,
                    created_at: chrono::DateTime::parse_from_rfc3339(&c.commit.committer?.date?)
                        .ok()?
                        .with_timezone(&Utc),
                })
            })
            .collect())
    }
}

fn user_node_id(id: u64) -> String {
    BASE64_STANDARD.encode(format!("04:User{id}"))
}

/// A comment made by a user on an issue or PR.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UserComment {
    pub repo_owner: String,
    pub repo_name: String,
    pub issue_number: u64,
    pub issue_title: String,
    pub issue_url: String,
    pub comment_url: String,
    pub body: String,
    pub created_at: Option<DateTime<Utc>>,
}

/// A commit made by a user on a given repository.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CommitInfo {
    pub repo_owner: String,
    pub repo_name: String,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[test]
    fn prepare_uses_organization_token() {
        let github = github_with_org_tokens(HashMap::from([(
            "rust-lang".to_string(),
            SecretString::from("organization-token"),
        )]));

        let request = github
            .prepare(
                false,
                Some("rust-lang"),
                Method::GET,
                "orgs/rust-lang/repos",
            )
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(
            request.headers().get(header::AUTHORIZATION).unwrap(),
            "token organization-token"
        );
    }

    #[test]
    fn prepare_without_organization_token_is_unauthenticated() {
        let github = github_with_org_tokens(HashMap::new());

        let request = github
            .prepare(
                false,
                Some("rust-lang"),
                Method::GET,
                "orgs/rust-lang/repos",
            )
            .unwrap()
            .build()
            .unwrap();

        assert!(!request.headers().contains_key(header::AUTHORIZATION));
    }

    #[test]
    fn prepare_without_organization_uses_default_token() {
        let github = github_with_org_tokens(HashMap::new());

        let request = github
            .prepare(false, None, Method::GET, "users/rust-lang-owner")
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(
            request.headers().get(header::AUTHORIZATION).unwrap(),
            "token default-token"
        );
    }

    fn github_with_org_tokens(org_tokens: HashMap<String, SecretString>) -> GitHubApi {
        GitHubApi {
            http: ClientBuilder::new().build().unwrap(),
            token: Some("default-token".to_string()),
            org_tokens: Some(GitHubTokens::App {
                org_tokens,
                enterprise_client_ctx: None,
            }),
        }
    }
}
