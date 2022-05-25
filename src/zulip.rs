use std::collections::HashMap;

use failure::{bail, Error};
use reqwest::Client;
use serde::Deserialize;

const ZULIP_BASE_URL: &str = "https://rust-lang.zulipchat.com/api/v1";
static TOKEN_VAR: &str = "ZULIP_TOKEN";
static USER_VAR: &str = "ZULIP_USER";

/// Access to the Zulip API
#[derive(Clone)]
pub(crate) struct ZulipApi {
    client: Client,
    auth: Option<(String, String)>,
}

impl ZulipApi {
    /// Create a new `ZulipApi` instance
    pub(crate) fn new() -> Self {
        let username = std::env::var(USER_VAR).ok();
        let token = std::env::var(TOKEN_VAR).ok();
        let auth = match (username, token) {
            (Some(u), Some(t)) => Some((u, t)),
            _ => None,
        };
        Self {
            client: Client::new(),
            auth,
        }
    }

    pub(crate) fn require_auth(&self) -> Result<(), Error> {
        if self.auth.is_none() {
            bail!(
                "missing either {} or {} environment variables",
                USER_VAR,
                TOKEN_VAR
            );
        }
        Ok(())
    }

    /// Get all users of the Rust Zulip instance
    pub(crate) fn get_users(&self) -> Result<Vec<ZulipUser>, Error> {
        let response = self
            .req(reqwest::Method::GET, "/users", None)?
            .error_for_status()?
            .json::<ZulipUsers>()?
            .members;

        Ok(response)
    }

    /// Perform a request against the Zulip API
    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        form: Option<HashMap<&str, &str>>,
    ) -> Result<reqwest::Response, Error> {
        let mut req = self
            .client
            .request(method, &format!("{}{}", ZULIP_BASE_URL, path));

        if let Some((username, token)) = &self.auth {
            req = req.basic_auth(username, Some(token))
        }
        if let Some(form) = form {
            req = req.form(&form);
        }

        Ok(req.send()?)
    }
}

/// A collection of Zulip users
#[derive(Deserialize)]
struct ZulipUsers {
    members: Vec<ZulipUser>,
}

/// A single Zulip user
#[derive(Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct ZulipUser {
    #[serde(rename = "delivery_email")]
    pub(crate) email: String,
    pub(crate) user_id: usize,
}
