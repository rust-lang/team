use std::collections::HashMap;

use crate::sync::Config;
use crate::sync::github::api::url::TokenType;
use anyhow::Context as _;
use chrono::Duration;
use octocrab::OctocrabBuilder;
use octocrab::models::{AppId, InstallationId};
use secrecy::SecretString;

/// Enterprise GitHub App used for certain operations that cannot be performed with an organization
/// GitHub app.
#[derive(Clone)]
pub struct EnterpriseAppCtx {
    /// Token for the enterprise installation of an enterprise GH app.
    ///
    /// Used to:
    /// - Find out in which repositories is an app installed in.
    ///
    /// The token has to be available for the whole duration of the process.
    enterprise_token: SecretString,
    /// Maps an organization to a pre-configured organization installation token of an enterprise GH
    /// app. We need this token, because the enterprise token does not have permissions for fetching
    /// everything we need about apps installed in an organization (sigh).
    ///
    /// Used to:
    /// - Find which apps are installed in a given organization.
    ///
    /// The token has to be available for the whole duration of the process.
    org_tokens: HashMap<String, SecretString>,
    /// Name of the enterprise.
    enterprise_name: String,
}

#[derive(Clone)]
pub enum GitHubTokens {
    /// Authentication using a set of GitHub apps.
    ///
    /// Stores one token per organization for most API operations.
    /// For operations involving other GitHub apps, also stores GitHub enterprise app token(s).
    App {
        /// Maps an organization to a pre-configured token.
        /// The token has to be available for the whole duration of the process.
        org_tokens: HashMap<String, SecretString>,
        /// Context for using enterprise GitHub App.
        enterprise_client_ctx: EnterpriseAppCtx,
    },
    /// One token for all API calls (used with Personal Access Token).
    Pat(SecretString),
}

impl GitHubTokens {
    /// Returns a HashMap of GitHub organization names mapped to their API tokens.
    ///
    /// Parses environment variables in the format GITHUB_TOKEN_{ORG_NAME}
    /// to retrieve GitHub tokens.
    pub async fn from_env(config: &Config) -> anyhow::Result<Self> {
        let mut tokens = HashMap::new();

        for (key, value) in std::env::vars() {
            if let Some(org_name) = org_name_from_env_var(&key) {
                tokens.insert(org_name, SecretString::from(value));
            }
        }

        if tokens.is_empty() {
            let pat_token = std::env::var("GITHUB_TOKEN")
                .context("failed to get any GitHub token environment variable")?;
            Ok(GitHubTokens::Pat(SecretString::from(pat_token)))
        } else {
            // We are using GitHub App authentication.

            // For the organization-level tokens, we use separate GitHub apps per organization.
            // Those apps are preauthorized (from CI), and we directly load their tokens from
            // environment variables.

            // Then we also need to load an enterprise GitHub App used to manage GitHub App
            // installations. Since the CI action that we use does not support enterprise apps yet,
            // we instead load the app id and the secret key through environment variables and
            // generate the necessary tokens ourselves.
            // Ideally, we would at least get the enterprise app installation id from GitHub, but
            // for some reason their endpoint for that does not work at the moment.
            // So we also have to pass the installation ID manually.
            let enterprise_name = get_var("ENTERPRISE_NAME")?;
            let enterprise_gh_app_id = get_var("ENTERPRISE_APP_ID")?
                .parse::<u64>()
                .map(AppId)
                .context("Enterprise app ID is not a number")?;
            let enterprise_gh_app_installation_id = get_var("ENTERPRISE_APP_INSTALLATION_ID")?
                .parse::<u64>()
                .map(InstallationId)
                .context("Enterprise app installation ID is not a number")?;
            let enterprise_gh_app_secret_key = get_var("ENTERPRISE_APP_SECRET_KEY")?;

            let secret_key =
                jsonwebtoken::EncodingKey::from_rsa_pem(enterprise_gh_app_secret_key.as_bytes())
                    .context("Cannot load enterprise app secret key")?;

            // Client for the enterprise app
            let enterprise_app_client = OctocrabBuilder::new()
                .app(enterprise_gh_app_id, secret_key)
                .build()?;
            // Client for the enterprise app's installation in the enterprise... sigh
            let enterprise_installation_client =
                enterprise_app_client.installation(enterprise_gh_app_installation_id)?;

            // Token for finding which repositories are GH apps installed in
            // Create a 1 hour buffer for the token
            let enterprise_token = enterprise_installation_client
                .installation_token_with_buffer(Duration::hours(1))
                .await?;

            let mut enterprise_org_tokens = HashMap::new();
            for org in tokens.keys() {
                if config.independent_github_orgs.contains(org.as_str()) {
                    continue;
                }

                // Get the corresponding organization installation of the enterprise app
                let org_installation = enterprise_app_client
                    .apps()
                    .get_org_installation(org)
                    .await
                    .with_context(|| {
                        anyhow::anyhow!(
                            "Cannot get organization installation for `{org}` of the enterprise app"
                        )
                    })?;
                let org_client = enterprise_app_client.installation(org_installation.id)?;
                // Generate an enterprise app installation token for the given org
                let org_token = org_client
                    .installation_token_with_buffer(Duration::hours(1))
                    .await?;
                enterprise_org_tokens.insert(org.clone(), org_token);
            }

            Ok(GitHubTokens::App {
                org_tokens: tokens,
                enterprise_client_ctx: EnterpriseAppCtx {
                    enterprise_token,
                    org_tokens: enterprise_org_tokens,
                    enterprise_name,
                },
            })
        }
    }

    /// Get a token for a GitHub organization.
    /// Return an error if not present.
    pub fn get_token_for_org(
        &self,
        org: &str,
        token_type: &TokenType,
    ) -> anyhow::Result<&SecretString> {
        match self {
            GitHubTokens::App {
                org_tokens,
                enterprise_client_ctx,
            } => match token_type {
                TokenType::Organization => org_tokens.get(org).with_context(|| {
                    format!(
                        "failed to get the GitHub token environment variable for organization {org}"
                    )
                }),
                TokenType::EnterpriseOrganization => {
                    enterprise_client_ctx.org_tokens.get(org).with_context(|| {
                        format!(
                            "failed to get the GitHub token environment variable for organization `{org}` for the enterprise GH app"
                        )
                    })
                }
                TokenType::Enterprise => {
                    Ok(&enterprise_client_ctx.enterprise_token)
                }
            },
            GitHubTokens::Pat(pat) => Ok(pat),
        }
    }

    /// Return the name of the enterprise, if present.
    pub fn get_enterprise_name(&self) -> anyhow::Result<&str> {
        match self {
            GitHubTokens::App {
                enterprise_client_ctx,
                ..
            } => Ok(enterprise_client_ctx.enterprise_name.as_str()),
            GitHubTokens::Pat(_) => Err(anyhow::anyhow!(
                "No enterprise is configured when using a PAT"
            )),
        }
    }
}

fn org_name_from_env_var(env_var: &str) -> Option<String> {
    env_var.strip_prefix("GITHUB_TOKEN_").map(|org| {
        // GitHub environment variables can't contain `-`, while GitHub organizations
        // can't contain `_`.
        // Here we are retrieving the org name from the environment variable, so we replace `_` with `-`.
        // E.g. the token for the `rust-lang` organization, is stored
        // in the `GITHUB_TOKEN_RUST_LANG` environment variable.
        org.to_lowercase().replace('_', "-")
    })
}

fn get_var(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| anyhow::anyhow!("Environment variable `{name}` not found."))
}
