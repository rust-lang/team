# Rust teams structure

This repository contains the structure of the Rust teams. The repository is
automatically synchronized with:

* [Governance section on the website](https://www.rust-lang.org/governance)
* [Crater and @craterbot](https://github.com/rust-lang-nursery/crater)
* Mailing lists and aliases for `@rust-lang.org` and `@crates.io`

If you need to add or remove a person from a team send a PR to this repository,
and after it's merged their account will be added/removed from all the
supported services.

## Using the CLI tool

It's possible to interact with this repository through its CLI tool.

### Verifying the integrity of the repository

This repository contains some sanity checks to avoid having stale or broken
data. You can run the checks locally with the `check` command:

```
$ cargo run check
```

### Querying information out of the repository

There are a few CLI commands that allow you to get some information generated
from the data in the repository.

You can get a list of all the people in a team:

```
$ cargo run dump-team all
```

You can get a list of all the email addresses subscribed to a list:

```
$ cargo run dump-list all@rust-lang.org
```

You can get a list of all the users with a permission:

```
$ cargo run dump-permission perf
```

## Schema

### People

Every member of a Rust team is represented by a file in the `people` directory.
The file structure is this:

```toml
name = "John Doe"  # Real name of the person (required)
github = "johndoe"  # GitHub username of the person (required)
# You can also set `email = false` to explicitly disable the email for the user.
# This will, for example, avoid adding the person to the mailing lists.
email = "john@doe.com"  # Email address used for mailing lists (optional)
irc-nickname = "jdoe"  # Nickname of the person on IRC, if different than the GitHub one (optional)

[permissions]
# Optional, see the permissions documentation
```

The file must be named the same as the GitHub username.

### Teams

Each Rust team or working group is represented by a file in the `teams`
directory. The structure of the file is this:

```toml
name = "overlords"  # Name of the team, used for GitHub (required)
subteam-of = "gods"  # Name of the parent team of this team (optional)

[people]
# Leads of the team, can be more than one and must be members of the team.
# Required, but it can be empty
leads = ["bors"]
# Members of the team, can be empty
members = [
    "bors",
    "rust-highfive",
    "rfcbot",
    "craterbot",
    "rust-timer",
]

[permissions]
# Optional, see the permissions documentation

# Define the mailing lists used by the team
# It's optional, and there can be more than one
[[lists]]
# The email address of the list (required)
address = "overlords@rust-lang.org"
# Access level of the list (required)
# - readonly: only users authenticated with Mailgun can send mails
# - members: only members of the list can send mails
# - everyone: everyone can send mails
access-level = "everyone"
# This can be set to false to avoid including all the team members in the list
# It's useful if you want to create the list with a different set of members
# It's optional, and the default is `true`.
include-team-members = true
# Include the following extra people in the mailing list. Their email address
# will be fetched from theirs TOML in people/ (optional).
extra-people = [
    "alexcrichton",
]
# Include the following email addresses in the mailing list (optional).
extra-emails = [
    "noreply@rust-lang.org",
]
# Include all the memebrs of the following teams in the mailing list
# (optional).
extra-teams = [
    "bots-nursery",
]
```

### Permissions

Permissions can be applied either to a single person or to a whole team, and
they grant access to some pieces of rust-lang tooling. The following
permissions are available:

```toml
[permissions]
# Optional, grants access to the @rust-timer GitHub bot
perf = true
```
