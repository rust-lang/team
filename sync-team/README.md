# Team synchronization code

This crate contains code used to synchronize the contents of the
team data with some of the services the Rust Team uses.

| Service name | Description                                     | Environment variables                       |
|--------------|-------------------------------------------------|---------------------------------------------|
| github       | Synchronize GitHub teams and repo configuration | `GITHUB_TOKEN`                              |
| mailgun      | Synchronize mailing lists on Mailgun            | `MAILGUN_API_TOKEN`, `EMAIL_ENCRYPTION_KEY` |
| zulip        | Synchronize Zulip user groups                   | `ZULIP_USERNAME`, `ZULIP_API_TOKEN`         |
