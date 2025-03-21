/// A URL to a GitHub API endpoint.
/// When using a GitHub App instead of a PAT, the token depends on the organization.
/// So storing the token together with the URL is convenient.
#[derive(Clone)]
pub struct GitHubUrl {
    url: String,
    org: String,
}

impl GitHubUrl {
    pub fn new(url: &str, org: &str) -> Self {
        let https = "https://";
        let url = if url.starts_with(https) {
            url.to_string()
        } else {
            format!("{https}api.github.com/{url}")
        };
        Self {
            url,
            org: org.to_string(),
        }
    }

    pub fn repos(org: &str, repo: &str, remaining_endpoint: &str) -> anyhow::Result<Self> {
        let remaining_endpoint = if remaining_endpoint.is_empty() {
            "".to_string()
        } else {
            validate_remaining_endpoint(remaining_endpoint)?;
            format!("/{remaining_endpoint}")
        };
        let url = format!("repos/{org}/{repo}{remaining_endpoint}");
        Ok(Self::new(&url, org))
    }

    pub fn orgs(org: &str, remaining_endpoint: &str) -> anyhow::Result<Self> {
        validate_remaining_endpoint(remaining_endpoint)?;
        let url = format!("orgs/{org}/{remaining_endpoint}");
        Ok(Self::new(&url, org))
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn org(&self) -> &str {
        &self.org
    }
}

fn validate_remaining_endpoint(endpoint: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !endpoint.starts_with('/'),
        "remaining endpoint {endpoint} should not start with a slash"
    );
    Ok(())
}
