# Labels used in the `team` repository

Some labels are setup to make it easier to figure out the status of an issue or PR, and allows
contributors to filter issues/PRs by labels.

## Labels are currently manually applied

[`triagebot`][triagebot] is not currently set up for the `team` repository, as it would need write
access (which needs to be tightly controlled). Thus, labels are currently manually applied and
adjusted. See [threat model](./threat-model.md).

## Meaning of labels

- `needs-{team-repo,infra}-admin-review`: needs one of the [`infra-admins` or `team-repo-admins` or
  one from both to review (and approve/reject)][team-repo-rules].
- `needs-team-lead-review`: needs a relevant team or Working Group (WG) or Project Group (PG) lead
  to approve.
- Statuses:
    - `S-waiting-on-{author,review,team}`: self-evident
    - `S-has-concerns`: outstanding concerns that must be addressed
    - `S-blocked`: blocked on *something*
- Team labels: only `T-{infra,leadership-council}`, the latter is intended to help council members
  to see what `team` PRs would need Council feedback or concerns the Council somehow.
- `E-{easy,medium,hard}` and `E-help-wanted`: call-for-participation labels copied from
  `rust-lang/rust`.
- `needs-triage`: someone should try to figure out what the issue/PR needs/is/affects/is blocked on.
- `needs-council-fcp`: something that requires the Council to FCP (outside `team` repo as rfcbot
  isn't enabled).


[triagebot]: https://forge.rust-lang.org/triagebot/index.html
[team-repo-rules]:
    https://forge.rust-lang.org/infra/team-maintenance.html#rules-for-changes-to-team-repo
