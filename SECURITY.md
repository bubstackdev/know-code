# Security

## Reporting a vulnerability

Report it privately through GitHub: on [bubstackdev/knowcode](https://github.com/bubstackdev/knowcode), open the
**Security** tab and choose **Report a vulnerability**. Please don't open a public issue for it.

Say what is affected, how to reproduce it, and which version (`knowcode --version`).

Security fixes go into the latest release only.

## What KnowCode does on your machine

These are the boundaries a report would most likely concern:

- Indexing and queries make no network calls. The only automatic network request is a daily check of public GitHub
  release metadata, which sends no paths, code or identifiers and can be turned off
  ([configuration](docs/configuration.md#update-check)). `self-update`, `fetch-deps` and `history deepen` use the
  network only when you run them.
- Nothing is written into indexed repositories. Indexes live in the user cache directory, notes and plugin files in
  the user data directory, settings in the user config directory.
- A repository's privacy file can block content from AI tools. Blocked content is never indexed, shown or searched
  ([privacy file](docs/configuration.md#privacy-file)). Content that reaches an answer despite a block is a
  vulnerability.
- `knowcode setup` edits only the client settings it names, and `knowcode uninstall` removes what setup recorded.
- Releases are built by GitHub Actions from the tagged commit, with SHA-256 checksums and GitHub artifact
  attestations; `self-update` checks an archive's checksum before installing it.
