# Threat model
This document briefly describes the threat model of this repository.

The `team` repository manages various sensitive permissions:
- It provides access to GitHub users to rust-lang (and other organization's) repositories.
    - This includes `admin` access, which can be quite destructive.
- It provides access to private Zulip streams.
- It subscribes users to mailing lists.

## User categories
There are three groups of users that come into play here:
- Infra *admins*. This is a small group of users that essentially have all the permissions.
- Team repo *maintainers*. This is a larger set of users with `write` access to this repository, which can include e.g. team leads.
- *Unprivileged* users, which is everyone else. This includes people that send PRs to this repository.

On the one hand, we want to give benevolent access to team leads to merge "benign" changes, such as membership changes in teams without elevated privileges or modifications to the description or homepage URL of repositories. On the other hand, we must make sure that the maintainers cannot make more dangerous changes, such as giving accessing CI tokens (more on that below). Otherwise, their accounts could become a target.

## What we need to avoid
After a `team` PR is approved, it is put into a merge queue, which uses a GitHub environment to access a secret "root" token that can do pretty much anything across the rust-lang enterprise. This secret is then used to sync changes to GitHub/Zulip/Mailgun, based on the changes made in the PR.

There are two categories of "attacks" that we would like to prevent.

### Elevation of privileges
The first category is "elevation of privileges", which could be done by affecting code that gets executed on CI. In general, we need to ensure that except for the *admins*, no one (not even the maintainers) will be able to:

1) Access the root token
2) Modify code that runs on CI (as that can read the token)
3) Merge changes to code or CI
4) Give themselves elevated privileges by changing the TOML data files

As this would give them almost unrestricted access to anything in the rust-lang enterprise.

To give a few more specific examples, here is a non-exhaustive list of scenarios that must not be possible for anyone else other than the admins:
- Modifying the TOML file of an infra-admin, changing GitHub ID to a different one to add their account to the infra-admins team.
- Changing the access permissions of the `team` repository itself.
- Changing the code of `sync-team` or `team` to give themselves special permissions.
- Changing the code of CI workflows.
- Adding or modifying a file that affects what gets executed on CI. For example `.cargo/config.toml` (affects Cargo) or `rust-toolchain.toml` file (affects Rustup).

### Content attacks
The second category is "content attacks", which can be done without changing code, only by modifying the TOML data files. This kind of attack could be performed by a maintainer, unless we explicitly protect against it.

For example, a content attack done by a *maintainer* account could be:
- Changing the homepage of a repository to point to a malware/scam website.
- Giving themselves `admin` permissions on a repository and then renaming it or moving it out of the organization.
- Giving themselves access to a repository or a team that they should not have access to (e.g. a survey team lead maintainer gives themselves access to the `lang` team).
- Removing access of someone else from a repository, barring them from contributing to it.

## How to prevent attacks
We plan to mitigate "elevation of privileges" attacks with [CODEOWNERS](../.github/CODEOWNERS) and "content attacks" with custom checks.

The `CODEOWNERS` file protects CI (the directory `.github`), code (`src`, `sync-team`, etc.) and other related files that could affect the build (`.cargo/config.toml`, `build.rs`, `rust-toolchain.toml`). It also protects TOML data files that control the infra admins team and the `team` repository itself.

When a PR that modifies one of these files is created, it has to be approved at least by a single infra admin, otherwise it will not be possible to be merged (or even added to the merge queue).

To protect against "content attacks", we should further implement CI checks that will check that changes to the TOML files did not result in suspicious activity.
