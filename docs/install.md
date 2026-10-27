# Install, connect, update, uninstall

How to install KnowCode, connect it to a coding agent, keep it current and remove it. The commands are summarised in
[usage](usage.md#setup-settings-and-updates); every flag is in `knowcode <command> --help`.

## Install

KnowCode runs on macOS (Apple silicon) and Linux (x86_64 or arm64). It needs `git`, and the client you connect: the
`claude` CLI for Claude Code, or Codex or Cursor. Windows is not supported yet.

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bubstackdev/know-code/releases/latest/download/knowcode-installer.sh | sh
```

The shell installer puts `knowcode` in `~/.local/bin` and adds that directory to your shell's `PATH` if it isn't
there; open a new terminal afterwards. Or with Homebrew:

```sh
brew install bubstackdev/tap/knowcode
```

**Installed 0.3.0 to 0.5.0 with the shell installer?** The repository moved from `bubstackdev/knowcode` to
`bubstackdev/know-code` after 0.5.0, and those versions accept releases only under the old name: they show no update
notice, and `self-update` fails ("installer URL is outside the expected KnowCode release"). Re-run the installer
above once; from then on `self-update` works. Homebrew installs are unaffected.

From source, with a Rust toolchain: `cargo build --release --locked`, then put `target/release/knowcode` on your
`PATH`.

## Connect a client

```sh
knowcode setup claude     # or: knowcode setup codex, knowcode setup cursor
```

`setup claude` installs the bundled Claude Code plugin (MCP server, Grep/Glob hook and skills) through the local
`claude` CLI. `setup codex` and `setup cursor` add the server to the client's user settings and keep unrelated
settings.

`--print` prints the configuration without changing anything, `--client-dir DIR` targets an isolated profile, and
`--replace` replaces a different server already registered as `knowcode` (Codex, Cursor). Start a new client session
afterwards.

No index command or configuration is needed afterwards: the first question indexes its repository. For answers
across repositories, add the directories holding your clones as workspace roots (`knowcode workspace add ~/Code`;
see [configuration](configuration.md#workspace-roots)). The [Claude Code plugin](../integrations/claude-code/README.md)
page describes what `setup claude` installs.

### Codex's session-start hook

`setup codex` also adds a user-level `[[hooks.SessionStart]]` entry to `config.toml` that runs KnowCode's
session-start hook, one line saying what KnowCode indexes here and which tools to call. Codex runs a hook only once
you trust it. That step is yours, in Codex, and KnowCode never takes it for you: start Codex, and at "Hooks need
review" choose "Review hooks" and trust the hook (or use `/hooks` later). Setup reads Codex's record of that and says
whether it is still to do, and `knowcode status` says the same for each Codex profile set up (`codex_hooks` in
`--json`): trusted, not yet trusted, changed since you trusted it, turned off in `/hooks`, or hooks turned off in
Codex's config.

You trust the hook once. Its command names only the binary and KnowCode's directory overrides, so a new KnowCode
version changes what it prints without asking for trust again. `self-update` replaces the binary at the same path, and
for a Homebrew install setup names `opt/knowcode/bin/knowcode`, which `brew upgrade` keeps, rather than the versioned
keg. Running setup with different directory overrides changes the hook, and Codex asks again. Codex also keys that
trust to the entry's place among the `SessionStart` entries, so setup adds it last and, run again, updates it where
it is. `knowcode uninstall` removes it and Codex's record of trusting it, and keeps your own hooks; if any follow it,
it leaves an empty entry in its place so that Codex still trusts them.

## Update

`self-update` updates a shell-installer installation. For a Homebrew installation it refuses and names
`brew upgrade bubstackdev/tap/knowcode`, which updates it instead.

Updating keeps the Claude Code plugin in step with the binary. After `self-update` installs a release, and whenever
`serve` starts and finds that the plugin bundle setup wrote is older than the binary, KnowCode rewrites the bundle
(manifest, hook and skills, keeping the binary path and overrides setup recorded) and runs
`claude plugin update knowcode@knowcode-local --scope user` for each Claude profile it was set up in, so Claude Code
picks up the new version in its next session. It touches only the bundle at the path setup recorded. If the `claude`
CLI is missing or the update fails, it says so once on stderr with the `knowcode setup claude` command that finishes
it; the update and the server go on regardless. `brew upgrade` doesn't run `self-update`, so for a Homebrew
installation the next `serve` start does the refresh. Codex and Cursor registrations name only the binary, so an update
needs nothing there.

The daily update check, and how to turn it off, is in [configuration](configuration.md#update-check).

## Uninstall

`uninstall` removes the registrations setup recorded, the bundled plugin files, the cached indexes, KnowCode's lock
and update-check files and the directories they leave empty, and keeps unrelated client settings and files. In a terminal it offers to remove configuration and notes (default no); otherwise pass
`--delete-config` or `--delete-notes`. It checks each removal against the client's own listing: a registration still
listed is reported, the command exits non-zero, and the registration stays recorded so running `uninstall` again
finishes it. Running clients are not stopped.

For Claude Code, the plugin is uninstalled with its data directory (`~/.claude/plugins/data/knowcode-knowcode-local`),
where KnowCode keeps nothing. Claude Code keeps its cached copy of the plugin, marked orphaned (`.orphaned_at`), so
sessions still running keep working, and deletes it itself later. KnowCode leaves that copy alone: it belongs to
Claude Code, and deleting it under a running session would break that session.

`uninstall` does not remove the binary. A package manager removes what it installed. The shell installer has no
uninstaller; for an installation it made, an `uninstall` that completes ends with the exact steps for that machine
(keep the binary until then). They are, in order:

1. Delete the line `. "$HOME/.config/knowcode/env.sh"` from each of `~/.profile`, `~/.bashrc`, `~/.bash_profile`,
   `~/.bash_login`, `~/.zshrc` and `~/.zshenv` that has it (`$ZDOTDIR` for the zsh files when set), and delete
   `~/.config/fish/conf.d/knowcode.env.fish`. Do this first: once `env.sh` is gone, every new shell that still
   sources it prints an error.
2. Delete the binary, `~/.local/bin/knowcode` by default.
3. Delete the installer's directory, `~/.config/knowcode` (`$XDG_CONFIG_HOME/knowcode` when set): its receipt
   `knowcode-receipt.json` and `env.sh` and `env.fish`.

KnowCode's own configuration directory is a different one; `--delete-config` removes the files KnowCode wrote there.
