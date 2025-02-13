use anyhow::Context as _;
use reqwest::header::HeaderValue;

/// A URL to a GitHub API endpoint.
/// When using a GitHub App instead of a PAT, the token depends on the organization.
/// So storing the token together with the URL is convenient.
#[derive(Clone)]
pub struct GitHubUrl {
    url: String,
    auth: HeaderValue,
}

impl GitHubUrl {
    pub fn new(url: String, auth: HeaderValue) -> Self {
        let https = "https://";
        let url = if url.starts_with(https) {
            url
        } else {
            format!("{https}api.github.com/{url}")
        };
        Self { url, auth }
    }

    pub fn new_with_org(url: String, org: &str) -> anyhow::Result<Self> {
        let auth = get_token_from_env(org).and_then(|token| auth_header(&token))?;
        Ok(Self::new(url, auth))
    }

    pub fn repos(org: &str, repo: &str, remaining_endpoint: &str) -> anyhow::Result<Self> {
        let remaining_endpoint = if remaining_endpoint.is_empty() {
            "".to_string()
        } else {
            validate_remaining_endpoint(remaining_endpoint)?;
            format!("/{remaining_endpoint}")
        };
        let url = format!("repos/{org}/{repo}{remaining_endpoint}");
        Self::new_with_org(url, org)
    }

    pub fn orgs(org: &str, remaining_endpoint: &str) -> anyhow::Result<Self> {
        validate_remaining_endpoint(remaining_endpoint)?;
        let url = format!("orgs/{org}/{remaining_endpoint}");
        Self::new_with_org(url, org)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn auth(&self) -> &HeaderValue {
        &self.auth
    }
}

fn validate_remaining_endpoint(endpoint: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !endpoint.starts_with('/'),
        "remaining endpoint {endpoint} should not start with a slash"
    );
    Ok(())
}

fn get_token_from_env(org: &str) -> anyhow::Result<String> {
    // GitHub environment variables can't contain `-`, while GitHub organizations
    // can't contain `_`. So we replace `-` with `_` in the organization name.
    // E.g. the token for the `rust-lang` organization, is stored
    // in the `GITHUB_TOKEN_RUST_LANG` environment variable.
    let org = org.to_uppercase().replace('-', "_");
    std::env::var(format!("GITHUB_TOKEN_{}", &org))
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .with_context(|| {
            format!("failed to get the GitHub token environment variable for org {org}")
        })
}

fn auth_header(token: &str) -> anyhow::Result<HeaderValue> {
    let mut auth = HeaderValue::from_str(&format!("token {}", token))?;
    auth.set_sensitive(true);
    Ok(auth)
}
