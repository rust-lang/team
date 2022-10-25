use failure::{bail, Error};
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder};
use reqwest::header::{self, HeaderValue};
use reqwest::Method;
use std::borrow::Cow;
use std::collections::HashMap;

static API_BASE: &str = "https://api.github.com/";
static TOKEN_VAR: &str = "GITHUB_TOKEN";

#[derive(serde::Deserialize)]
pub(crate) struct User {
    pub(crate) id: usize,
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
}

impl GitHubApi {
    pub(crate) fn new() -> Self {
        GitHubApi {
            http: ClientBuilder::new()
                .user_agent(crate::USER_AGENT)
                .build()
                .unwrap(),
            token: std::env::var(TOKEN_VAR).ok(),
        }
    }

    fn prepare(
        &self,
        require_auth: bool,
        method: Method,
        url: &str,
    ) -> Result<RequestBuilder, Error> {
        let url = if url.starts_with("https://") {
            Cow::Borrowed(url)
        } else {
            Cow::Owned(format!("{}{}", API_BASE, url))
        };
        if require_auth {
            self.require_auth()?;
        }

        let mut req = self.http.request(method, url.as_ref());
        if let Some(token) = &self.token {
            req = req.header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("token {}", token))?,
            );
        }
        Ok(req)
    }

    fn graphql<R, V>(&self, query: &str, variables: V) -> Result<R, Error>
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
            .prepare(true, Method::POST, "graphql")?
            .json(&Request { query, variables })
            .send()?
            .error_for_status()?
            .json()?;
        if let Some(error) = res.errors.get(0) {
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

    pub(crate) fn user(&self, login: &str) -> Result<User, Error> {
        Ok(self
            .prepare(false, Method::GET, &format!("users/{}", login))?
            .send()?
            .error_for_status()?
            .json()?)
    }

    pub(crate) fn usernames(&self, ids: &[usize]) -> Result<HashMap<usize, String>, Error> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Usernames {
            database_id: usize,
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

        let mut result = HashMap::new();
        for chunk in ids.chunks(100) {
            let res: GraphNodes<Usernames> = self.graphql(
                QUERY,
                Params {
                    ids: chunk.iter().map(|id| user_node_id(*id)).collect(),
                },
            )?;
            for node in res.nodes.into_iter().flatten() {
                result.insert(node.database_id, node.login);
            }
        }
        Ok(result)
    }

    pub(crate) fn repo(&self, org: &str, repo: &str) -> Result<Option<Repo>, Error> {
        let resp = self
            .prepare(true, Method::GET, &format!("repos/{}/{}", org, repo))?
            .send()?;
        match resp.status() {
            reqwest::StatusCode::OK => Ok(Some(resp.json()?)),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            _ => Err(resp.error_for_status().unwrap_err().into()),
        }
    }

    /// Get all teams for the rust-lang org
    pub(crate) fn teams(&self) -> Result<Vec<GitHubTeam>, Error> {
        Ok(self
            .prepare(true, Method::GET, "orgs/rust-lang/teams?per_page=100")?
            .send()?
            .error_for_status()?
            .json()?)
    }

    /// Get all users who have not yet accepted the invitation
    pub(crate) fn pending_org_invites(&self) -> Result<Vec<User>, Error> {
        Ok(self
            .prepare(true, Method::GET, "orgs/rust-lang/invitations?per_page=100")?
            .send()?
            .error_for_status()?
            .json()?)
    }

    /// Get all team members for the team with the given id
    pub(crate) fn team_members(&self, id: usize) -> Result<Vec<GitHubMember>, Error> {
        let mut members = Vec::new();
        let mut page_num = 1;
        loop {
            let page: Vec<GitHubMember> = self
                .prepare(
                    true,
                    Method::GET,
                    &format!("teams/{}/members?per_page=100&page={}", id, page_num),
                )?
                .send()?
                .error_for_status()?
                .json()?;
            let len = page.len();
            members.extend(page);
            if len < 100 {
                break;
            }
            page_num += 1;
        }
        Ok(members)
    }

    pub(crate) fn repo_teams(&self, org: &str, repo: &str) -> Result<Vec<Team>, Error> {
        let resp = self
            .prepare(true, Method::GET, &format!("repos/{}/{}/teams", org, repo))?
            .send()?;
        Ok(resp.error_for_status()?.json()?)
    }

    pub(crate) fn protected_branches(&self, org: &str, repo: &str) -> Result<Vec<Branch>, Error> {
        let resp = self
            .prepare(
                true,
                Method::GET,
                &format!("repos/{}/{}/branches?protected=true", org, repo),
            )?
            .send()?;
        Ok(resp.error_for_status()?.json()?)
    }

    pub(crate) fn branch_protection(
        &self,
        org: &str,
        repo: &str,
        branch: &str,
    ) -> Result<BranchProtection, Error> {
        let resp = self
            .prepare(
                true,
                Method::GET,
                &format!("repos/{}/{}/branches/{}/protection", org, repo, branch),
            )?
            .send()?;
        Ok(resp.error_for_status()?.json()?)
    }
}

fn user_node_id(id: usize) -> String {
    base64::encode(&format!("04:User{}", id))
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct GitHubTeam {
    pub(crate) id: usize,
    pub(crate) name: String,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct GitHubMember {
    pub(crate) id: usize,
    #[serde(rename = "login")]
    pub(crate) name: String,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Repo {
    pub(crate) description: String,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Team {
    pub(crate) name: String,
    pub(crate) permission: Permission,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Permission(String);

impl Permission {
    pub(crate) fn as_toml(&self) -> &str {
        match self.0.as_str() {
            "push" => "write",
            s => s,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq, Copy, Clone)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TeamPrivacy {
    Closed,
    Secret,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Branch {
    pub(crate) name: String,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct BranchProtection {
    pub(crate) required_status_checks: Option<StatusChecks>,
    pub(crate) required_pull_request_reviews: Option<RequiredReviews>,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct StatusChecks {
    pub(crate) contexts: Vec<String>,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct RequiredReviews {
    pub(crate) dismiss_stale_reviews: bool,
}
