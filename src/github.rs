use failure::{Error, ResultExt};
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, Method, RequestBuilder};
use std::borrow::Cow;

static API_BASE: &str = "https://api.github.com/";
static TOKEN_VAR: &str = "GITHUB_TOKEN";

#[derive(serde::Deserialize)]
pub(crate) struct User {
    pub(crate) login: String,
    pub(crate) name: Option<String>,
    pub(crate) email: Option<String>,
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

    pub(crate) fn user(&self, login: &str) -> Result<User, Error> {
        Ok(self
            .prepare(Method::GET, &format!("users/{}", login))?
            .send()?
            .error_for_status()?
            .json()?)
    }
}
