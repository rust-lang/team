use std::collections::HashMap;

use anyhow::Context as _;
use secrecy::SecretString;

#[derive(Clone)]
pub enum GitHubTokens {
    /// One token per organization (used with GitHub App).
    Orgs(HashMap<String, SecretString>),
    /// One token for all API calls (used with Personal Access Token).
    Pat(SecretString),
}

impl GitHubTokens {
    /// Returns a HashMap of GitHub organization names mapped to their API tokens.
    ///
    /// Parses environment variables in the format GITHUB_TOKEN_{ORG_NAME}
    /// to retrieve GitHub tokens.
    pub fn from_env() -> anyhow::Result<Self> {
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
            Ok(GitHubTokens::Orgs(tokens))
        }
    }

    /// Get a token for a GitHub organization.
    /// Return an error if not present.
    pub fn get_token(&self, org: &str) -> anyhow::Result<&SecretString> {
        match self {
            GitHubTokens::Orgs(orgs) => orgs.get(org).with_context(|| {
                format!(
                    "failed to get the GitHub token environment variable for organization {org}"
                )
            }),
            GitHubTokens::Pat(pat) => Ok(pat),
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
