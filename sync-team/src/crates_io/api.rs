use crate::crates_io::CrateConfig;
use crate::utils::ResponseExt;
use anyhow::{Context, anyhow};
use log::debug;
use reqwest::blocking::Client;
use reqwest::header;
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

// OpenAPI spec: https://crates.io/api/openapi.json
const CRATES_IO_BASE_URL: &str = "https://crates.io/api/v1";

/// Access to the Zulip API
#[derive(Clone)]
pub(crate) struct CratesIoApi {
    client: Client,
    token: SecretString,
    dry_run: bool,
}

impl CratesIoApi {
    pub(crate) fn new(token: SecretString, dry_run: bool) -> Self {
        let mut map = HeaderMap::default();
        map.insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::USER_AGENT),
        );

        Self {
            client: reqwest::blocking::ClientBuilder::default()
                .default_headers(map)
                .build()
                .unwrap(),
            token,
            dry_run,
        }
    }

    pub(crate) fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Return the user ID based on the username.
    pub(crate) fn get_user_id(&self, username: &str) -> anyhow::Result<UserId> {
        #[derive(serde::Deserialize)]
        struct User {
            id: u32,
        }

        #[derive(serde::Deserialize)]
        struct UserResponse {
            user: User,
        }

        let response: UserResponse = self
            .req::<()>(
                reqwest::Method::GET,
                &format!("/users/{username}"),
                HashMap::new(),
                None,
            )?
            .error_for_status()?
            .json_annotated()?;

        Ok(UserId(response.user.id))
    }

    /// List existing trusted publishing configurations for a given crate.
    pub(crate) fn list_trusted_publishing_github_configs(
        &self,
        user_id: UserId,
    ) -> anyhow::Result<Vec<TrustedPublishingGitHubConfig>> {
        #[derive(serde::Deserialize)]
        struct GetTrustedPublishing {
            github_configs: Vec<TrustedPublishingGitHubConfig>,
        }

        let mut configs = vec![];
        self.req_paged::<(), GetTrustedPublishing, _>(
            "/trusted_publishing/github_configs",
            HashMap::from([("user_id".to_string(), user_id.0.to_string())]),
            None,
            |resp| configs.extend(resp.github_configs),
        )?;

        Ok(configs)
    }

    /// List owners of a given crate.
    pub(crate) fn list_crate_owners(&self, krate: &str) -> anyhow::Result<Vec<CratesIoOwner>> {
        #[derive(serde::Deserialize)]
        struct OwnersResponse {
            users: Vec<CratesIoOwner>,
        }

        let response: OwnersResponse = self
            .req::<()>(
                reqwest::Method::GET,
                &format!("/crates/{krate}/owners"),
                HashMap::new(),
                None,
            )?
            .error_for_status()?
            .json_annotated()?;

        Ok(response.users)
    }

    /// Invite the specified user(s) or team(s) to own a given crate.
    pub(crate) fn invite_crate_owners(
        &self,
        krate: &str,
        owners: &[CratesIoOwner],
    ) -> anyhow::Result<()> {
        debug!("Inviting owners {owners:?} to crate {krate}");

        #[derive(serde::Serialize)]
        struct InviteOwnersRequest<'a> {
            owners: Vec<&'a str>,
        }

        let owners = owners.iter().map(|o| o.login.as_str()).collect::<Vec<_>>();

        if !self.dry_run {
            self.req(
                reqwest::Method::PUT,
                &format!("/crates/{krate}/owners"),
                HashMap::new(),
                Some(&InviteOwnersRequest { owners }),
            )?
            .error_for_status()?;
        }

        Ok(())
    }

    /// Delete the specified owner(s) of a given crate.
    pub(crate) fn delete_crate_owners(
        &self,
        krate: &str,
        owners: &[CratesIoOwner],
    ) -> anyhow::Result<()> {
        debug!("Deleting owners {owners:?} from crate {krate}");

        #[derive(serde::Serialize)]
        struct DeleteOwnersRequest<'a> {
            owners: &'a [&'a str],
        }

        let owners = owners.iter().map(|o| o.login.as_str()).collect::<Vec<_>>();

        if !self.dry_run {
            self.req(
                reqwest::Method::DELETE,
                &format!("/crates/{krate}/owners"),
                HashMap::new(),
                Some(&DeleteOwnersRequest { owners: &owners }),
            )?
            .error_for_status()
            .with_context(|| {
                anyhow::anyhow!("Cannot delete owner(s) {owners:?} from krate {krate}")
            })?;
        }
        Ok(())
    }

    /// Create a new trusted publishing configuration for a given crate.
    pub(crate) fn create_trusted_publishing_github_config(
        &self,
        config: &CrateConfig,
    ) -> anyhow::Result<()> {
        debug!(
            "Creating trusted publishing config for '{}' in repo '{}/{}', workflow file '{}' and environment '{}'",
            config.krate.0,
            config.repo_org,
            config.repo_name,
            config.workflow_file,
            config.environment
        );

        if self.dry_run {
            return Ok(());
        }

        #[derive(serde::Serialize)]
        struct TrustedPublishingGitHubConfigCreate<'a> {
            repository_owner: &'a str,
            repository_name: &'a str,
            #[serde(rename = "crate")]
            krate: &'a str,
            workflow_filename: &'a str,
            environment: Option<&'a str>,
        }

        #[derive(serde::Serialize)]
        struct CreateTrustedPublishing<'a> {
            github_config: TrustedPublishingGitHubConfigCreate<'a>,
        }

        let request = CreateTrustedPublishing {
            github_config: TrustedPublishingGitHubConfigCreate {
                repository_owner: &config.repo_org,
                repository_name: &config.repo_name,
                krate: &config.krate.0,
                workflow_filename: &config.workflow_file,
                environment: Some(&config.environment),
            },
        };

        self.req(
            reqwest::Method::POST,
            "/trusted_publishing/github_configs",
            HashMap::new(),
            Some(&request),
        )?
        .error_for_status()
        .with_context(|| anyhow!("Cannot created trusted publishing config {config:?}"))?;

        Ok(())
    }

    /// Delete a trusted publishing configuration with the given ID.
    pub(crate) fn delete_trusted_publishing_github_config(
        &self,
        id: TrustedPublishingId,
    ) -> anyhow::Result<()> {
        debug!("Deleting trusted publishing with ID {id}");

        if !self.dry_run {
            self.req::<()>(
                reqwest::Method::DELETE,
                &format!("/trusted_publishing/github_configs/{}", id.0),
                HashMap::new(),
                None,
            )?
            .error_for_status()
            .with_context(|| anyhow!("Cannot delete trusted publishing config with ID {id}"))?;
        }

        Ok(())
    }

    /// Return all crates owned by the given user.
    pub(crate) fn get_crates_owned_by(&self, user: UserId) -> anyhow::Result<Vec<CratesIoCrate>> {
        #[derive(serde::Deserialize)]
        struct CratesResponse {
            crates: Vec<CratesIoCrate>,
        }

        let mut crates = vec![];
        self.req_paged::<(), CratesResponse, _>(
            "/crates",
            HashMap::from([("user_id".to_string(), user.0.to_string())]),
            None,
            |res| {
                crates.extend(res.crates);
            },
        )?;

        Ok(crates)
    }

    /// Enable or disable the `trustpub_only` crate option, which specifies whether a crate
    /// has to be published **only** through trusted publishing.
    pub(crate) fn set_trusted_publishing_only(
        &self,
        krate: &str,
        value: bool,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct PatchCrateRequest {
            #[serde(rename = "crate")]
            krate: Crate,
        }

        #[derive(serde::Serialize)]
        struct Crate {
            trustpub_only: bool,
        }

        if !self.dry_run {
            self.req(
                reqwest::Method::PATCH,
                &format!("/crates/{krate}"),
                HashMap::new(),
                Some(&PatchCrateRequest {
                    krate: Crate {
                        trustpub_only: value,
                    },
                }),
            )?
            .error_for_status()
            .with_context(|| anyhow::anyhow!("Cannot patch crate {krate}"))?;
        }

        Ok(())
    }

    /// Perform a request against the crates.io API
    fn req<T: Serialize>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: HashMap<String, String>,
        data: Option<&T>,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        let mut req = self
            .client
            .request(method, format!("{CRATES_IO_BASE_URL}{path}"))
            .bearer_auth(self.token.expose_secret())
            .query(&query);
        if let Some(data) = data {
            req = req.json(data);
        }

        Ok(req.send()?)
    }

    /// Fetch a resource that is paged.
    fn req_paged<T: Serialize, R: DeserializeOwned, F>(
        &self,
        path: &str,
        mut query: HashMap<String, String>,
        data: Option<&T>,
        mut handle_response: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(R),
    {
        #[derive(serde::Deserialize, Debug)]
        struct Response<R> {
            meta: MetaResponse,
            #[serde(flatten)]
            data: R,
        }

        #[derive(serde::Deserialize, Debug)]
        struct MetaResponse {
            next_page: Option<String>,
        }

        if !query.contains_key("per_page") {
            query.insert("per_page".to_string(), "100".to_string());
        }

        let mut query = query;
        let mut path_extra: Option<String> = None;
        loop {
            let path = match path_extra {
                Some(p) => format!("{path}{p}"),
                None => path.to_owned(),
            };

            let response: Response<R> = self
                .req(reqwest::Method::GET, &path, query, data)?
                .error_for_status()?
                .json_annotated()?;
            handle_response(response.data);
            match response.meta.next_page {
                Some(next) => {
                    path_extra = Some(next);
                    query = HashMap::new();
                }
                None => break,
            }
        }
        Ok(())
    }
}

#[derive(Copy, Clone, Debug)]
pub struct UserId(pub u32);

#[derive(serde::Deserialize, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrustedPublishingId(u64);

impl Display for TrustedPublishingId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
pub(crate) struct TrustedPublishingGitHubConfig {
    #[serde(rename = "crate")]
    pub(crate) krate: String,
    pub(crate) id: TrustedPublishingId,
    pub(crate) repository_owner: String,
    pub(crate) repository_name: String,
    pub(crate) workflow_filename: String,
    pub(crate) environment: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct CratesIoCrate {
    pub(crate) name: String,
    #[serde(rename = "trustpub_only")]
    pub(crate) trusted_publishing_only: bool,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq, Hash, Copy, Clone)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OwnerKind {
    User,
    Team,
}

#[derive(serde::Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) struct CratesIoOwner {
    login: String,
    kind: OwnerKind,
}

impl CratesIoOwner {
    pub(crate) fn team(org: String, name: String) -> Self {
        Self {
            login: format!("github:{org}:{name}"),
            kind: OwnerKind::Team,
        }
    }

    pub(crate) fn kind(&self) -> OwnerKind {
        self.kind
    }

    pub(crate) fn login(&self) -> &str {
        &self.login
    }
}
