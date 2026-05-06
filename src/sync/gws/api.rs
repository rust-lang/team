use crate::sync::gws::RUST_LANG_GWS_DOMAIN;
use async_trait::async_trait;
use std::sync::LazyLock;

static SAML_GROUP_NAME_MARKER: &str = "-saml";
static SAML_GROUP_EMAIL_MARKER: LazyLock<String> =
    LazyLock::new(|| format!("{SAML_GROUP_NAME_MARKER}@{RUST_LANG_GWS_DOMAIN}"));

/// https://developers.google.com/workspace/admin/directory/reference/rest/v1/groups
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Group {
    pub name: String,
    pub email: String,
}

impl Group {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            email: format!("{name}{}", SAML_GROUP_EMAIL_MARKER.as_str()),
            name: format!("{name}{SAML_GROUP_NAME_MARKER}"),
        }
    }

    pub(crate) fn is_saml(&self) -> bool {
        self.email.ends_with(SAML_GROUP_EMAIL_MARKER.as_str())
    }
}

/// https://developers.google.com/workspace/admin/directory/reference/rest/v1/users#UserName
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct UserName {
    pub given_name: String,
    pub family_name: String,
}

/// https://developers.google.com/workspace/admin/directory/reference/rest/v1/users
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct User {
    pub name: UserName,
    pub primary_email: String,
}

#[async_trait]
pub(crate) trait GoogleWorkspaceApiClient {
    async fn get_users(&self) -> anyhow::Result<Vec<User>>;

    async fn get_groups(&self) -> anyhow::Result<Vec<Group>>;
}
