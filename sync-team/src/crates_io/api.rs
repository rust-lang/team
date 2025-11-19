use crate::crates_io::CratesIoPublishingConfig;
use crate::utils::ResponseExt;
use anyhow::{Context, anyhow};
use log::debug;
use reqwest::blocking::Client;
use reqwest::header;
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
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

    /// List existing trusted publishing configurations for a given crate.
    pub(crate) fn list_trusted_publishing_github_configs(
        &self,
        krate: &str,
    ) -> anyhow::Result<Vec<TrustedPublishingGitHubConfig>> {
        #[derive(serde::Deserialize)]
        struct GetTrustedPublishing {
            github_configs: Vec<TrustedPublishingGitHubConfig>,
        }

        let response: GetTrustedPublishing = self
            .req::<()>(
                reqwest::Method::GET,
                &format!("/trusted_publishing/github_configs?crate={krate}"),
                None,
            )?
            .error_for_status()?
            .json_annotated()?;

        Ok(response.github_configs)
    }

    /// Create a new trusted publishing configuration for a given crate.
    pub(crate) fn create_trusted_publishing_github_config(
        &self,
        config: &CratesIoPublishingConfig,
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
                None,
            )?
            .error_for_status()
            .with_context(|| anyhow!("Cannot delete trusted publishing config with ID {id}"))?;
        }

        Ok(())
    }

    /// Perform a request against the crates.io API
    fn req<T: Serialize>(
        &self,
        method: reqwest::Method,
        path: &str,
        data: Option<&T>,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        let mut req = self
            .client
            .request(method, format!("{CRATES_IO_BASE_URL}{path}"))
            .bearer_auth(self.token.expose_secret());
        if let Some(data) = data {
            req = req.json(data);
        }

        Ok(req.send()?)
    }
}

#[derive(serde::Deserialize, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrustedPublishingId(u64);

impl Display for TrustedPublishingId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(serde::Deserialize, Debug)]
pub(crate) struct TrustedPublishingGitHubConfig {
    pub(crate) id: TrustedPublishingId,
    pub(crate) repository_owner: String,
    pub(crate) repository_name: String,
    pub(crate) workflow_filename: String,
    pub(crate) environment: Option<String>,
}
