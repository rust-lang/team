use anyhow::{bail, Error};
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder};
use reqwest::header::{self, HeaderValue};
use reqwest::Method;
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

    pub(crate) fn user(&self, login: &str) -> Result<User, Error> {
        Ok(self
            .prepare(false, Method::GET, &format!("users/{}", login))?
            .send()?
            .error_for_status()?
            .json()?)
    }

    pub(crate) fn usernames(&self, ids: &[u64]) -> Result<HashMap<u64, String>, Error> {
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
            let res: GraphNodes<Usernames> = match self.graphql(
                QUERY,
                Params {
                    ids: chunk.iter().map(|id| user_node_id(*id)).collect(),
                },
            ) {
                Ok(res) => res,
                Err(e) => {
                    if cant_resolve(&e) {
                        // This error happens when a user is deleted. Provide
                        // a more helpful error message to pinpoint it:
                        for id in chunk {
                            if let Err(inner_e) = self.graphql::<GraphNodes<Usernames>, Params>(
                                QUERY,
                                Params {
                                    ids: vec![user_node_id(*id)],
                                },
                            ) {
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

    pub(crate) fn repo_teams(&self, org: &str, repo: &str) -> Result<Vec<Team>, Error> {
        let resp = self
            .prepare(true, Method::GET, &format!("repos/{}/{}/teams", org, repo))?
            .send()?;
        Ok(resp.error_for_status()?.json()?)
    }

    pub(crate) fn repo_collaborators(
        &self,
        org: &str,
        repo: &str,
    ) -> Result<Vec<RepoCollaborator>, Error> {
        let resp = self
            .prepare(
                true,
                Method::GET,
                &format!("repos/{org}/{repo}/collaborators?affiliation=direct"),
            )?
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

fn user_node_id(id: u64) -> String {
    base64::encode(format!("04:User{id}"))
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Repo {
    pub(crate) description: Option<String>,
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

#[derive(serde::Deserialize, Debug)]
pub(crate) struct RepoCollaborator {
    #[serde(alias = "login")]
    pub(crate) name: String,
    pub(crate) permissions: Permissions,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct Permissions {
    triage: bool,
    push: bool,
    maintain: bool,
    admin: bool,
}

impl Permissions {
    pub(crate) fn highest(&self) -> &str {
        if self.admin {
            "admin"
        } else if self.maintain {
            "maintain"
        } else if self.push {
            "write"
        } else if self.triage {
            "triage"
        } else {
            "read"
        }
    }
}
