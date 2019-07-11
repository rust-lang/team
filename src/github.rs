use failure::{bail, Error, ResultExt};
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, Method, RequestBuilder};
use std::borrow::Cow;
use std::collections::HashMap;

static API_BASE: &str = "https://api.github.com/";
static TOKEN_VAR: &str = "GITHUB_TOKEN";

#[derive(serde::Deserialize)]
pub(crate) struct User {
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
    token: String,
}

impl GitHubApi {
    pub(crate) fn new() -> Result<Self, Error> {
        let token = std::env::var(TOKEN_VAR)
            .with_context(|_| format!("missing environment variable {}", TOKEN_VAR))?;
        Ok(GitHubApi {
            http: Client::new(),
            token: token.to_string(),
        })
    }

    fn prepare(&self, method: Method, url: &str) -> Result<RequestBuilder, Error> {
        let url = if url.starts_with("https://") {
            Cow::Borrowed(url)
        } else {
            Cow::Owned(format!("{}{}", API_BASE, url))
        };
        Ok(self.http.request(method, url.as_ref()).header(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("token {}", self.token))?,
        ))
    }

    fn graphql<R, V>(&self, query: &str, variables: V) -> Result<R, Error>
    where
        R: for<'de> serde::Deserialize<'de>,
        V: serde::Serialize,
    {
        #[derive(serde::Serialize)]
        struct Request<'a, V> {
            query: &'a str,
            variables: V,
        }
        let res: GraphResult<R> = self
            .prepare(Method::POST, "graphql")?
            .json(&Request { query, variables })
            .send()?
            .error_for_status()?
            .json()?;
        if let Some(error) = res.errors.iter().next() {
            bail!("graphql error: {}", error.message);
        } else if let Some(data) = res.data {
            Ok(data)
        } else {
            bail!("missing graphql data");
        }
    }

    pub(crate) fn user(&self, login: &str) -> Result<User, Error> {
        Ok(self
            .prepare(Method::GET, &format!("users/{}", login))?
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
            for node in res.nodes.into_iter().filter_map(|n| n) {
                result.insert(node.database_id, node.login);
            }
        }
        Ok(result)
    }
}

fn user_node_id(id: usize) -> String {
    base64::encode(&format!("04:User{}", id))
}
