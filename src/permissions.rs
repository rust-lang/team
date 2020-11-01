use crate::data::Data;
use crate::schema::Person;
use failure::{bail, Error};
use std::collections::HashSet;

macro_rules! permissions {
    (
        booleans {
            $($boolean:ident,)*
        }
        bors_repos {
            $($bors:ident,)*
        }
        crates_io_ops_apps {
            $($crates_io_ops_app:ident,)*
        }
    ) => {
        #[derive(serde_derive::Deserialize, Debug)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct BorsACL {
            #[serde(default)]
            review: bool,
            #[serde(rename = "try", default)]
            try_: bool,
        }

        impl Default for BorsACL {
            fn default() -> Self {
                BorsACL {
                    review: false,
                    try_: false,
                }
            }
        }

        #[derive(serde_derive::Deserialize, Debug)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct BorsPermissions {
            $(
                #[serde(default)]
                $bors: BorsACL,
            )*
        }

        impl Default for BorsPermissions {
            fn default() -> Self {
                BorsPermissions {
                    $($bors: BorsACL::default(),)*
                }
            }
        }

        #[derive(serde_derive::Deserialize, Debug)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        pub(crate) struct Permissions {
            $(
                #[serde(default)]
                $boolean: bool,
            )*
            #[serde(default)]
            bors: BorsPermissions,
            #[serde(default)]
            crates_io_ops_bot: CratesIoOps,
        }

        impl Default for Permissions {
            fn default() -> Self {
                Permissions {
                    $($boolean: false,)*
                    bors: BorsPermissions::default(),
                    crates_io_ops_bot: CratesIoOps::default(),
                }
            }
        }

        impl Permissions {
            pub(crate) const AVAILABLE: &'static [&'static str] = &[
                $(stringify!($boolean),)*
                $(concat!("bors.", stringify!($bors), ".review"),)*
                $(concat!("bors.", stringify!($bors), ".try"),)*
                $(concat!("crates_io_ops_bot.", stringify!($crates_io_ops_app)),)*
            ];

            pub(crate) const REQUIRES_DISCORD: &'static [&'static str] = &[
                $(concat!("crates_io_ops_bot.", stringify!($crates_io_ops_app)),)*
            ];

            pub(crate) fn has(&self, permission: &str) -> bool {
                self.has_directly(permission) || self.has_indirectly(permission)
            }

            pub(crate) fn has_directly(&self, permission: &str) -> bool {
                $(
                    if permission == stringify!($boolean) {
                        return self.$boolean;
                    }
                )*
                $(
                    if permission == concat!("bors.", stringify!($bors), ".review") {
                        return self.bors.$bors.review;
                    }
                    if permission == concat!("bors.", stringify!($bors), ".try") {
                        return self.bors.$bors.try_
                    }
                )*
                $(
                    if permission == concat!("crates_io_ops_bot.", stringify!($crates_io_ops_app)) {
                        return self.crates_io_ops_bot.$crates_io_ops_app
                    }
                )*
                false
            }

            pub fn has_indirectly(&self, permission: &str) -> bool {
                $(
                    if permission == concat!("bors.", stringify!($bors), ".try") {
                        return self.bors.$bors.review;
                    }
                )*
                false
            }

            pub(crate) fn has_any(&self) -> bool {
                false
                $(|| self.$boolean)*
                $(|| self.bors.$bors.review)*
                $(|| self.bors.$bors.try_)*
                $(|| self.crates_io_ops_bot.$crates_io_ops_app)*
            }

            pub(crate) fn validate(&self, what: String) -> Result<(), Error> {
                $(
                    if self.bors.$bors.try_ == true && self.bors.$bors.review == true {
                        bail!(
                            "{} has both the `bors.{}.review` and `bors.{}.try` permissions",
                            what,
                            stringify!($bors),
                            stringify!($bors),
                        );
                    }
                )*
                Ok(())
            }
        }

        #[derive(serde_derive::Deserialize, Debug)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct CratesIoOps {
            $(
                #[serde(default)]
                $crates_io_ops_app: bool,
            )*
        }

        impl Default for CratesIoOps {
            fn default() -> Self {
                CratesIoOps {
                    $($crates_io_ops_app: false,)*
                }
            }
        }
    }
}

permissions! {
    booleans {
        perf,
        crater,
    }
    bors_repos {
        cargo,
        chalk,
        clippy,
        compiler_builtins,
        crater,
        crates_io,
        hashbrown,
        miri,
        libc,
        regex,
        rls,
        rust,
        rustlings,
        rustup_rs,
        semverver,
        stdarch,
        team,
    }
    crates_io_ops_apps {
        crates_io,
        staging_crates_io,
    }
}

pub(crate) fn allowed_people<'a>(
    data: &'a Data,
    permission: &str,
) -> Result<Vec<&'a Person>, Error> {
    let mut members_with_perms = HashSet::new();
    for team in data.teams() {
        if team.permissions().has(permission) {
            for member in team.members(&data)? {
                members_with_perms.insert(member);
            }
        }
    }
    Ok(data
        .people()
        .filter(|p| members_with_perms.contains(p.github()) || p.permissions().has(permission))
        .collect())
}
