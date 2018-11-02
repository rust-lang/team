# Mailgun Mailmap

[![Build Status](https://travis-ci.org/rust-lang/mailgun-mailmap.svg?branch=master)](https://travis-ci.org/rust-lang/mailgun-mailmap)

Mail configuration for rust-lang domains.

> **Note**: This repository is still being tested. The words below describe in
> theory what happens if all testing goes well.

This repository contains mail configuration for all rust-lang domains. All our
mail is handled by [Mailgun](https://www.mailgun.com/). On Mailgun all our mail
goes through mailing lists. This means that any email send to an email address
for rust-lang is then broadcast to a list of members.

Configuration of mailing lists is done via this git repository. The
[`mailmap.toml`](https://github.com/rust-lang/mailgun-mailmap/blob/master/mailmap.toml)
file contains a description of all mailing lists for the rust-lang domains. Each
mailing list has a list of members as well.

Updates to this repository should be done through pull requests. Anyone can send
a pull request!

When a pull requests is merged Travis will run and will sync the state of
`mailmap.toml` to Mailgun itself.
