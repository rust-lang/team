# Rust teams structure

This repository contains the structure of the Rust teams. It's automatically
synchronized with the following services:

* Mailing lists (through MailGun)

## Using the CLI tool

It's possible to interact with this repository through its CLI tool.

### Synchronizing with third-party providers

The `sync` subcommand synchronizes the repository with all the third-party
providers we support:

```
$ cargo run sync
```

You need to have the following environment variables:

* `MAILGUN_API_TOKEN`: the authentication token for the Rust MailGun account

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

## Schema

### People

Every member of a Rust team is represented by a file in the `people` directory.
The file structure is this:

```toml
name = "John Doe"  # Real name of the person (required)
github = "johndoe"  # GitHub username of the person (required)
email = "john@doe.com"  # Email address used for mailing lists (optional)
irc-nickname = "jdoe"  # Nickname of the person on IRC, if different than the GitHub one (optional)
```

The file must be named the same as the GitHub username.

### Teams

Each Rust team or working group is represented by a file in the `teams`
directory. The structure of the file is this:

```toml
name = "overlords"  # Name of the team, used for GitHub (required)
# Include all the members of the listed teams as members of this team (optional)
inherit = [
    "kings",
]

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
