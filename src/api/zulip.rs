use std::collections::HashMap;

use anyhow::{Error, bail};
use chrono::{DateTime, Utc};
use reqwest::Method;
use reqwest::{Client, ClientBuilder, Response};
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
            client: ClientBuilder::new()
                .user_agent(crate::USER_AGENT)
                .build()
                .unwrap(),
            auth,
        }
    }

    pub(crate) fn require_auth(&self) -> Result<(), Error> {
        if self.auth.is_none() {
            bail!("missing {USER_VAR} and/or {TOKEN_VAR} environment variables");
        }
        Ok(())
    }

    /// Get all users of the Rust Zulip instance
    pub(crate) async fn get_users(
        &self,
        include_profile_fields: bool,
    ) -> Result<Vec<ZulipUser>, Error> {
        let url = if include_profile_fields {
            "/users?include_custom_profile_fields=true"
        } else {
            "/users"
        };
        let response = self
            .req(Method::GET, url, None)
            .await?
            .error_for_status()?
            .json::<ZulipUsers>()
            .await?
            .members;

        Ok(response)
    }

    /// Get a single user of the Rust Zulip instance
    pub(crate) async fn get_user(&self, user_id: u64) -> Result<ZulipUser, Error> {
        let response = self
            .req(Method::GET, &format!("/users/{user_id}"), None)
            .await?
            .error_for_status()?
            .json::<ZulipOneUser>()
            .await?
            .user;

        Ok(response)
    }

    pub async fn get_last_n_messages_sent_by_user(
        &self,
        user: u64,
        n: u64,
    ) -> anyhow::Result<Vec<MessageInfo>> {
        let query = serde_json::json!([{
            "operator": "sender",
            "operand": user
        }])
        .to_string();

        #[derive(serde::Deserialize)]
        struct Message {
            subject: String,
            timestamp: u64,
        }

        #[derive(serde::Deserialize)]
        struct Response {
            messages: Vec<Message>,
        }

        let response = self
            .req(
                Method::GET,
                &format!("/messages?anchor=newest&num_before={n}&num_after=0&narrow={query}"),
                None,
            )
            .await?
            .error_for_status()?
            .json::<Response>()
            .await?;
        Ok(response
            .messages
            .into_iter()
            .rev()
            .map(|msg| MessageInfo {
                subject: msg.subject,
                timestamp: DateTime::from_timestamp(msg.timestamp as i64, 0).unwrap_or(Utc::now()),
            })
            .collect())
    }

    /// Perform a request against the Zulip API
    async fn req(
        &self,
        method: Method,
        path: &str,
        form: Option<HashMap<&str, &str>>,
    ) -> Result<Response, Error> {
        let mut req = self
            .client
            .request(method, format!("{ZULIP_BASE_URL}{path}"));

        if let Some((username, token)) = &self.auth {
            req = req.basic_auth(username, Some(token))
        }
        if let Some(form) = form {
            req = req.form(&form);
        }

        Ok(req.send().await?)
    }
}

/// A collection of Zulip users, as returned from '/users'
#[derive(Deserialize)]
struct ZulipUsers {
    members: Vec<ZulipUser>,
}

/// A collection of exactly one Zulip user, as returned from '/users/{user_id}'
#[derive(Deserialize)]
struct ZulipOneUser {
    user: ZulipUser,
}

#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct ProfileValue {
    value: String,
}

/// A single Zulip user
#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct ZulipUser {
    pub(crate) user_id: u64,
    #[serde(rename = "full_name")]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) profile_data: HashMap<String, ProfileValue>,
}

impl ZulipUser {
    // The GitHub profile data key is 3873
    pub(crate) fn get_github_username(&self) -> Option<&str> {
        self.profile_data.get("3873").map(|v| v.value.as_str())
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct MessageInfo {
    pub subject: String,
    pub timestamp: chrono::DateTime<Utc>,
}
