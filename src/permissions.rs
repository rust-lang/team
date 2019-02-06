use crate::data::Data;
use failure::Error;
use std::collections::HashSet;

#[macro_export]
macro_rules! permissions {
    ($vis:vis struct $name:ident { $($key:ident,)* }) => {
        #[derive(serde_derive::Deserialize, Debug)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        $vis struct $name {
            $(
                #[serde(default)]
                $key: bool,
            )*
        }

        impl Default for $name {
            fn default() -> Self {
                $name {
                    $($key: false,)*
                }
            }
        }

        impl $name {
            $vis const AVAILABLE: &'static [&'static str] = &[$(stringify!($key),)*];

            $vis fn has(&self, permission: &str) -> bool {
                $(
                    if permission == stringify!($key) {
                        return self.$key;
                    }
                )*
                false
            }

            $vis fn has_any(&self) -> bool {
                false $(|| self.$key)*
            }
        }
    }
}

pub(crate) fn allowed_github_users(
    data: &Data,
    permission: &str,
) -> Result<HashSet<String>, Error> {
    let mut github_users = HashSet::new();
    for team in data.teams() {
        if team.permissions().has(permission) {
            for member in team.members(&data)? {
                github_users.insert(member.to_string());
            }
        }
    }
    for person in data.people() {
        if person.permissions().has(permission) {
            github_users.insert(person.github().to_string());
        }
    }
    Ok(github_users)
}
