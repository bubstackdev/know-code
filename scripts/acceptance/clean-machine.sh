#!/usr/bin/env bash
# Clean-machine acceptance for KnowCode on macOS (Apple silicon) and Linux (x86_64, arm64).
#
# Run it as the operator, in a terminal, on a clean machine (a fresh user account or container) with Claude Code
# installed and signed in. It installs KnowCode into the real home directory with the single shell command, sets up
# Claude Code in a clone of each pinned public case, asks the operator to run two real client sessions per case,
# uninstalls, and checks that what remains is only what the operator had before. Every step is recorded in a report
# directory, which is archived at the end. Session numbers are recorded, never gated; knowcode-use.json holds the
# KnowCode calls per case and session (unprompted and named) in machine-readable form.
#
#   scripts/acceptance/clean-machine.sh --commit SHA [--artifacts DIR] [options]
#   scripts/acceptance/clean-machine.sh --dry-run --commit SHA   # read-only checks, then the planned commands
#   scripts/acceptance/clean-machine.sh --self-test              # the parser and settings-comparison tests
#
# Works with the bash 3.2 that macOS ships. Parsing is done by clean_machine.py beside this script (Python 3.9+,
# standard library only). Commands that could read the terminal get stdin from /dev/null; new shells are login
# shells started without -i, so none can take the terminal.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
HELPER="$HERE/clean_machine.py"
REPO_URL=https://github.com/bubstackdev/knowcode.git
RELEASES=https://github.com/bubstackdev/knowcode/releases
COMMIT=""
ARTIFACTS=""
RELEASE=latest
WORK="$HOME/knowcode-acceptance"
CASES=""
QUIET=""
NOTES=""
HIDE_LOCAL_BIN=0
SKIP_SESSIONS=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: clean-machine.sh --commit SHA [options]

  --commit SHA        KnowCode commit whose acceptance runner and manifest are used (required)
  --artifacts DIR     install from this local artifact mirror (KNOWCODE_DOWNLOAD_URL=file://DIR) instead of the
                      public release; DIR holds knowcode-installer.sh and the archives
  --release TAG       public release to install when --artifacts is not given (default: latest)
  --repo-url URL      KnowCode repository to clone (default: the public repository)
  --workdir DIR       clones and report go here (default: ~/knowcode-acceptance); must not exist yet
  --cases "A B"       cases from scripts/acceptance/public-repos.json (default: all)
  --quiet yes|no      operator's answer to "is this machine quiet?" (asked when omitted)
  --notes TEXT        operator notes: emulation, competing work, anything that affects timings
  --hide-local-bin    run the installer with ~/.local/bin removed from PATH, so it must add it to the profile
  --skip-sessions     skip the two client sessions per case (recorded as skipped)
  --dry-run           record the machine and run the read-only clean-machine checks, then print the plan
  --self-test         run the tests of the transcript parser and the settings comparison, then exit
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --commit) COMMIT=${2:-}; shift 2 ;;
    --artifacts) ARTIFACTS=${2:-}; shift 2 ;;
    --release) RELEASE=${2:-}; shift 2 ;;
    --repo-url) REPO_URL=${2:-}; shift 2 ;;
    --workdir) WORK=${2:-}; shift 2 ;;
    --cases) CASES=${2:-}; shift 2 ;;
    --quiet) QUIET=${2:-}; shift 2 ;;
    --notes) NOTES=${2:-}; shift 2 ;;
    --hide-local-bin) HIDE_LOCAL_BIN=1; shift ;;
    --skip-sessions) SKIP_SESSIONS=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --self-test) exec python3 -B -m unittest discover -s "$HERE" -p 'test_clean_machine.py' -v ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[ -n "$COMMIT" ] || { usage >&2; exit 2; }
if [ -n "$ARTIFACTS" ]; then
  [ -f "$ARTIFACTS/knowcode-installer.sh" ] || { echo "no knowcode-installer.sh in $ARTIFACTS" >&2; exit 2; }
  ARTIFACTS=$(cd "$ARTIFACTS" && pwd)
fi
case "$QUIET" in ""|yes|no) ;; *) echo "--quiet takes yes or no" >&2; exit 2 ;; esac
python3 -c 'import sys; sys.exit(sys.version_info < (3, 9))' 2>/dev/null </dev/null \
  || { echo "python3 3.9 or newer is required" >&2; exit 2; }
[ ! -e "$WORK" ] || { echo "$WORK already exists; pass a new --workdir" >&2; exit 2; }

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS/$ARCH" in
  Darwin/arm64) TRIPLE=aarch64-apple-darwin ;;
  Linux/x86_64) TRIPLE=x86_64-unknown-linux-gnu ;;
  Linux/aarch64|Linux/arm64) TRIPLE=aarch64-unknown-linux-gnu ;;
  *) echo "unsupported platform: $OS $ARCH" >&2; exit 2 ;;
esac
if [ "$RELEASE" = latest ]; then RELEASE_URL="$RELEASES/latest/download"; else RELEASE_URL="$RELEASES/download/$RELEASE"; fi

CLAUDE_DIR=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
SETTINGS="$CLAUDE_DIR/settings.json"
LOCAL_BIN="$HOME/.local/bin"
INSTALLER_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/knowcode"
if [ "$OS" = Darwin ]; then
  KC_DIRS="$HOME/Library/Caches/dev.bubstack.knowcode
$HOME/Library/Application Support/dev.bubstack.knowcode"
else
  KC_DIRS="${XDG_CACHE_HOME:-$HOME/.cache}/dev.bubstack.knowcode
${XDG_CONFIG_HOME:-$HOME/.config}/dev.bubstack.knowcode
${XDG_DATA_HOME:-$HOME/.local/share}/dev.bubstack.knowcode"
fi
PROFILES=".profile .bashrc .bash_profile .bash_login .zshrc .zshenv .config/fish/conf.d/knowcode.env.fish"

REPORT="$WORK/report"
mkdir -p "$REPORT" "$WORK/repos"
STEPS="$REPORT/steps.tsv"
: > "$STEPS"
STARTED=$(date +%s)

# step NAME PASS|FAIL|INFO|SKIP DETAIL: one line of the report.
step() {
  printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$STEPS"
  printf '[%s] %s: %s\n' "$2" "$1" "$3"
}
helper() { python3 -B "$HELPER" "$@" </dev/null; }
ask() { # ask PROMPT: the operator's answer in REPLY (empty at end of input)
  printf '%s' "$1"
  REPLY=""
  read -r REPLY || REPLY=""
}
quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
heading() { printf '\n== %s\n' "$1"; }

# A new terminal starts an interactive login shell. -i is never used here: an interactive shell takes the terminal
# (it suspended the 0.1.0 runs). So each shell is a login shell (-l) with stdin from /dev/null and a fresh minimal
# environment, which then sources the rc file an interactive shell would read (.zshrc, .bashrc).
# The rc lines and commands are expanded by the new shell, not here.
new_shell() { # new_shell SHELL COMMAND OUT ERR
  local rc
  case "$(basename "$1")" in
    zsh) rc="[ -f \"\${ZDOTDIR:-\$HOME}/.zshrc\" ] && . \"\${ZDOTDIR:-\$HOME}/.zshrc\";" ;;
    bash) rc="[ -f \"\$HOME/.bashrc\" ] && . \"\$HOME/.bashrc\";" ;;
    *) rc='' ;;
  esac
  env -i HOME="$HOME" USER="${USER:-$(id -un)}" LOGNAME="${LOGNAME:-$(id -un)}" SHELL="$1" TERM=dumb \
    LANG="${LANG:-C}" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$1" -lc "$rc $2" </dev/null >"$3" 2>"$4"
}
# The operator's login shell, then zsh and bash when installed: one of each.
SHELLS=""
for s in "${SHELL:-}" /bin/zsh /bin/bash; do
  if [ -z "$s" ] || [ ! -x "$s" ]; then continue; fi
  case " $SHELLS " in *"/$(basename "$s") "*) ;; *) SHELLS="$SHELLS $s" ;; esac
done
LOGIN_SHELL=${SHELL:-/bin/bash}

profiles_snapshot() { # profiles_snapshot DIR: a copy of each shell profile the installer may edit
  mkdir -p "$1"
  for p in $PROFILES; do
    [ -f "$HOME/$p" ] && mkdir -p "$1/$(dirname "$p")" && cp "$HOME/$p" "$1/$p"
  done
  return 0
}
profiles_diff() { # profiles_diff BEFORE AFTER: unified diff per changed profile
  for p in $PROFILES; do
    a="$1/$p"; b="$2/$p"
    [ -f "$a" ] || a=/dev/null
    [ -f "$b" ] || b=/dev/null
    [ "$a" = /dev/null ] && [ "$b" = /dev/null ] && continue
    diff -u "$a" "$b" | sed "s|$1/||; s|$2/||"
  done
}
snapshot_settings() { if [ -f "$SETTINGS" ]; then cp "$SETTINGS" "$1"; else echo null > "$1"; fi; }
registrations() { # registrations TAG: KnowCode's registrations in Claude Code, one per line (exit 1 if any)
  claude plugin list --json </dev/null >"$REPORT/$1-plugins.json" 2>"$REPORT/$1-plugins.err" || echo null >"$REPORT/$1-plugins.json"
  claude plugin marketplace list --json </dev/null >"$REPORT/$1-marketplaces.json" 2>"$REPORT/$1-marketplaces.err" \
    || echo null >"$REPORT/$1-marketplaces.json"
  helper registrations "$REPORT/$1-plugins.json" "$REPORT/$1-marketplaces.json" "$SETTINGS"
}

# ------------------------------------------------------------------------------------------------- machine record
heading "Machine"
CLIENT_VERSION=$(claude --version </dev/null 2>/dev/null || true)
if [ -z "$QUIET" ]; then
  ask "Is this machine quiet: no builds, indexing, or other heavy work running? [y/n] "
  case "$REPLY" in y|Y|yes) QUIET=yes ;; *) QUIET=no ;; esac
fi
if [ -z "$NOTES" ] && [ "$DRY_RUN" = 0 ]; then
  ask "Notes for the report (emulation, competing work; Enter for none): "
  NOTES=$REPLY
fi
helper machine --quiet "$QUIET" --notes "$NOTES" --client-version "$CLIENT_VERSION" > "$REPORT/machine.json"
ENVIRONMENT=$(helper environment)
uptime > "$REPORT/uptime.txt" 2>&1 </dev/null
step machine INFO "$OS $ARCH, quiet: $QUIET, client: ${CLIENT_VERSION:-missing}, environment label: $ENVIRONMENT"
if [ -n "$CLIENT_VERSION" ]; then step client-installed PASS "$CLIENT_VERSION"
else step client-installed FAIL "claude is not on PATH"; fi

# --------------------------------------------------------------------------------------------- clean-machine check
heading "Clean-machine check"
clean=1
if command -v knowcode >/dev/null 2>&1; then step clean-path FAIL "knowcode on PATH: $(command -v knowcode)"; clean=0
else step clean-path PASS "no knowcode on PATH"; fi
[ -e "$LOCAL_BIN/knowcode" ] && { step clean-binary FAIL "$LOCAL_BIN/knowcode exists"; clean=0; }
leftover=""
while IFS= read -r d; do [ -e "$d" ] && leftover="$leftover $d"; done <<EOF
$KC_DIRS
$INSTALLER_DIR
EOF
if [ -n "$leftover" ]; then step clean-dirs FAIL "present:$leftover"; clean=0; else step clean-dirs PASS "no KnowCode cache, config, data or installer directory"; fi
overrides=$(env | sed -n 's/^\(KNOWCODE_[A-Z_]*\)=.*/\1/p' | tr '\n' ' ')
if [ -n "$overrides" ]; then step clean-env FAIL "KnowCode overrides set: $overrides"; clean=0; else step clean-env PASS "no KNOWCODE_* overrides"; fi
if found=$(registrations clean); then step clean-client PASS "no KnowCode plugin, marketplace or settings entry"
else step clean-client FAIL "$(echo "$found" | tr '\n' ';')"; clean=0; fi
snapshot_settings "$REPORT/settings-start.json"
for s in $SHELLS; do
  n=$(basename "$s")
  new_shell "$s" "printf '%s\\n' \"\$PATH\"" "$REPORT/shell-before-$n.out" "$REPORT/shell-before-$n.err"
done
n=$(basename "$LOGIN_SHELL")
case ":$(head -n 1 "$REPORT/shell-before-$n.out" 2>/dev/null):" in
  *":$LOCAL_BIN:"*) LOCAL_BIN_BEFORE=yes ;;
  *) LOCAL_BIN_BEFORE=no ;;
esac
case ":$PATH:" in *":$LOCAL_BIN:"*) LOCAL_BIN_HERE=yes ;; *) LOCAL_BIN_HERE=no ;; esac
step path-before INFO "$LOCAL_BIN on PATH in a new $n login shell: $LOCAL_BIN_BEFORE; in this shell: $LOCAL_BIN_HERE"

if [ "$DRY_RUN" = 1 ]; then
  heading "Plan (dry run: nothing below is run)"
  if [ -n "$ARTIFACTS" ]; then
    INSTALL="KNOWCODE_DOWNLOAD_URL=file://$ARTIFACTS sh $ARTIFACTS/knowcode-installer.sh"
  else
    INSTALL="curl --proto '=https' --tlsv1.2 -LsSf $RELEASE_URL/knowcode-installer.sh | sh"
  fi
  cat <<EOF
git clone $REPO_URL $WORK/repos/knowcode && git checkout --detach $COMMIT
for each case in scripts/acceptance/public-repos.json${CASES:+ ($CASES)}: git clone URL $WORK/repos/CASE at its commit
  python3 scripts/check-public-first-run.py CASE --repo ... --environment $ENVIRONMENT --machine-notes ...
install: $INSTALL$( [ "$HIDE_LOCAL_BIN" = 1 ] && echo "  (with ~/.local/bin removed from PATH)")
new login shells (${SHELLS# }): command -v knowcode && knowcode --version
in each case clone: knowcode setup claude; then two client sessions per case (unprompted, named)
snapshot $SETTINGS; knowcode uninstall --delete-config --delete-notes; compare settings per entry
pause for uninstall's removal steps; check knowcode is gone and new shells start without new errors
tar the report: $WORK/clean-machine-report.tar.gz
EOF
  helper summary "$REPORT" > "$REPORT/summary.md"
  echo "Dry run; report in $REPORT"
  grep -q "	FAIL	" "$STEPS" && exit 1
  exit 0
fi
if [ "$clean" = 0 ]; then
  ask "This machine is not clean (see FAIL above). Continue anyway? [y/N] "
  case "$REPLY" in y|Y|yes) step clean-override INFO "operator continued on an unclean machine" ;; *) exit 1 ;; esac
fi

# ------------------------------------------------------------------------------------------------------- clones
heading "Clones"
KC_REPO="$WORK/repos/knowcode"
if git clone -q "$REPO_URL" "$KC_REPO" </dev/null && git -C "$KC_REPO" checkout -q --detach "$COMMIT" </dev/null; then
  step clone-knowcode PASS "$REPO_URL at $(git -C "$KC_REPO" rev-parse HEAD)"
else
  step clone-knowcode FAIL "could not clone $REPO_URL at $COMMIT"
  helper summary "$REPORT" > "$REPORT/summary.md"; exit 1
fi
MANIFEST="$KC_REPO/scripts/acceptance/public-repos.json"
[ -n "$CASES" ] || CASES=$(helper manifest "$MANIFEST" | tr '\n' ' ')
for c in $CASES; do
  url=$(helper manifest "$MANIFEST" "$c" url)
  sha=$(helper manifest "$MANIFEST" "$c" commit)
  branch=$(helper manifest "$MANIFEST" "$c" branch)
  if git clone -q ${branch:+--branch "$branch"} "$url" "$WORK/repos/$c" </dev/null \
    && git -C "$WORK/repos/$c" checkout -q --detach "$sha" </dev/null; then
    step "clone-$c" PASS "$url at $sha"
  else
    step "clone-$c" FAIL "could not clone $url at $sha"
  fi
done

# ----------------------------------------------------------------------------------------- isolated-profile runner
heading "Isolated-profile runner"
RUNNER_ARTIFACTS=$ARTIFACTS
if [ -z "$RUNNER_ARTIFACTS" ]; then
  # The runner installs from a directory: mirror this platform's release files.
  RUNNER_ARTIFACTS="$WORK/release-mirror"
  mkdir -p "$RUNNER_ARTIFACTS"
  for f in knowcode-installer.sh dist-manifest.json sha256.sum "knowcode-$TRIPLE.tar.gz" "knowcode-$TRIPLE.tar.gz.sha256"; do
    curl --proto '=https' --tlsv1.2 -LsSf -o "$RUNNER_ARTIFACTS/$f" "$RELEASE_URL/$f" </dev/null \
      || step runner-mirror FAIL "could not download $RELEASE_URL/$f"
  done
fi
RUNNER_NOTES="$NOTES"
[ "$ENVIRONMENT" = isolated-profile ] && RUNNER_NOTES="$NOTES (no clean-machine label for $OS $ARCH)"
for c in $CASES; do
  [ -d "$WORK/repos/$c" ] || { step "runner-$c" SKIP "no clone"; continue; }
  quiet_flag=""
  [ "$QUIET" = yes ] && quiet_flag=--quiet-confirmed
  if python3 -B "$KC_REPO/scripts/check-public-first-run.py" "$c" --repo "$WORK/repos/$c" \
      --artifacts "$RUNNER_ARTIFACTS" --output "$REPORT/runner-$c.json" --environment "$ENVIRONMENT" \
      ${quiet_flag:+"$quiet_flag"} --machine-notes "$RUNNER_NOTES" </dev/null >"$REPORT/runner-$c.log" 2>&1; then
    step "runner-$c" PASS "--environment $ENVIRONMENT; report runner-$c.json"
  else
    step "runner-$c" FAIL "see runner-$c.log and runner-$c.json"
  fi
done

# ------------------------------------------------------------------------------------------------------ install
heading "Install"
profiles_snapshot "$REPORT/profiles-before"
INSTALL_PATH=$PATH
if [ "$HIDE_LOCAL_BIN" = 1 ]; then
  INSTALL_PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$LOCAL_BIN" | grep -vxF "$LOCAL_BIN/" | paste -sd: -)
fi
case ":$INSTALL_PATH:" in *":$LOCAL_BIN:"*) LOCAL_BIN_AT_INSTALL=yes ;; *) LOCAL_BIN_AT_INSTALL=no ;; esac
if [ -n "$ARTIFACTS" ]; then
  INSTALL="KNOWCODE_DOWNLOAD_URL=file://$ARTIFACTS sh $ARTIFACTS/knowcode-installer.sh"
  SOURCE="local artifact mirror $ARTIFACTS (does not verify public download hosting)"
else
  INSTALL="curl --proto '=https' --tlsv1.2 -LsSf $RELEASE_URL/knowcode-installer.sh | sh"
  SOURCE="public release ($RELEASE)"
fi
printf '%s\n' "$INSTALL" > "$REPORT/install-command.txt"
if PATH=$INSTALL_PATH sh -c "$INSTALL" </dev/null >"$REPORT/install.log" 2>&1; then
  step install PASS "from $SOURCE: $INSTALL"
else
  step install FAIL "from $SOURCE (exit $?); see install.log"
fi
KC="$LOCAL_BIN/knowcode"
if [ -x "$KC" ]; then
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$KC" | cut -d' ' -f1 > "$REPORT/binary.sha256"
  else sha256sum "$KC" | cut -d' ' -f1 > "$REPORT/binary.sha256"; fi
  step binary INFO "$("$KC" --version </dev/null 2>&1), sha256 $(cat "$REPORT/binary.sha256")"
fi
profiles_snapshot "$REPORT/profiles-after-install"
profiles_diff "$REPORT/profiles-before" "$REPORT/profiles-after-install" > "$REPORT/profiles-install.diff"
if [ -s "$REPORT/profiles-install.diff" ]; then
  step profile-changes INFO "installer changed: $(grep '^+++ ' "$REPORT/profiles-install.diff" | cut -c5- | cut -f1 | tr '\n' ' ')"
else
  step profile-changes INFO "installer changed no shell profile"
fi
if [ "$LOCAL_BIN_AT_INSTALL" = no ]; then
  if grep -q 'knowcode/env' "$REPORT/profiles-install.diff"; then
    step path-added PASS "$LOCAL_BIN was not on the installer's PATH; it added it through env.sh"
  else
    step path-added FAIL "$LOCAL_BIN was not on the installer's PATH and no profile sources env.sh"
  fi
else
  step path-added INFO "$LOCAL_BIN was already on the installer's PATH: the installer did not need to add it (--hide-local-bin exercises that)"
fi
for s in $SHELLS; do
  n=$(basename "$s")
  if new_shell "$s" 'command -v knowcode && knowcode --version' "$REPORT/shell-installed-$n.out" "$REPORT/shell-installed-$n.err"; then
    result=PASS
  else
    result=FAIL
  fi
  [ "$s" = "$LOGIN_SHELL" ] || { [ "$result" = PASS ] || result=INFO; }
  step "new-shell-$n" "$result" "$(tr '\n' ' ' < "$REPORT/shell-installed-$n.out")"
done

# -------------------------------------------------------------------------------------------------------- setup
heading "Setup"
for c in $CASES; do
  [ -d "$WORK/repos/$c" ] || continue
  if (cd "$WORK/repos/$c" && "$KC" setup claude) </dev/null >"$REPORT/setup-$c.log" 2>&1; then
    step "setup-$c" PASS "knowcode setup claude"
  else
    step "setup-$c" FAIL "see setup-$c.log"
  fi
done
if found=$(registrations setup); then step registered FAIL "no KnowCode plugin registered after setup"
else step registered PASS "$(echo "$found" | tr '\n' ';')"; fi

# ----------------------------------------------------------------------------------------------------- sessions
heading "Client sessions"
SESSIONS_SINCE=$(date +%s)
for c in $CASES; do
  [ -d "$WORK/repos/$c" ] || continue
  question=$(helper manifest "$MANIFEST" "$c" question)
  if [ "$SKIP_SESSIONS" = 1 ]; then step "sessions-$c" SKIP "--skip-sessions"; continue; fi
  for label in unprompted named; do
    prompt=$question
    [ "$label" = named ] && prompt="Use KnowCode to answer: $question"
    cat <<EOF

Case $c, $label session. In a NEW terminal window run:

  cd $(quote "$WORK/repos/$c") && claude $(quote "$prompt")

Let it answer without further prompts, then leave the session with /exit.
EOF
    ask "Press Enter here when the session has ended (s to skip): "
    [ "$REPLY" = s ] && step "session-$c-$label" SKIP "operator skipped"
  done
  if helper sessions --projects "$CLAUDE_DIR/projects" --case-dir "$WORK/repos/$c" --question "$question" \
      --since "$SESSIONS_SINCE" > "$REPORT/sessions-$c.json"; then
    step "sessions-$c" INFO "$(helper session-line "$REPORT/sessions-$c.json")"
  else
    step "sessions-$c" INFO "transcripts could not be parsed"
  fi
done

# ---------------------------------------------------------------------------------------------------- uninstall
heading "Uninstall"
snapshot_settings "$REPORT/settings-before-uninstall.json"
"$KC" uninstall --delete-config --delete-notes </dev/null >"$REPORT/uninstall.log" 2>&1
code=$?
if [ "$code" = 0 ]; then step uninstall PASS "exit 0"; else step uninstall FAIL "exit $code; see uninstall.log"; fi
snapshot_settings "$REPORT/settings-after-uninstall.json"
if found=$(registrations uninstalled); then step registration-removed PASS "no KnowCode plugin, marketplace or settings entry"
else step registration-removed FAIL "$(echo "$found" | tr '\n' ';')"; fi
if helper compare-settings "$REPORT/settings-before-uninstall.json" "$REPORT/settings-after-uninstall.json" \
    > "$REPORT/settings-compare.json"; then
  step settings-preserved PASS "only KnowCode's own entries removed"
else
  step settings-preserved FAIL "see settings-compare.json"
fi
indexes=""
while IFS= read -r d; do
  [ -d "$d" ] && indexes="$indexes$(find "$d" -name '*.sqlite' 2>/dev/null | tr '\n' ' ')"
done <<EOF
$KC_DIRS
EOF
if [ -n "$indexes" ]; then step indexes-removed FAIL "left: $indexes"; else step indexes-removed PASS "no index left"; fi
# The machine started without these directories, and --delete-config --delete-notes leaves nothing of KnowCode's.
leftover=""
while IFS= read -r d; do [ -e "$d" ] && leftover="$leftover $d"; done <<EOF
$KC_DIRS
EOF
if [ -n "$leftover" ]; then step dirs-removed FAIL "still present:$leftover"
else step dirs-removed PASS "no KnowCode cache, configuration or data directory"; fi

heading "Removal steps"
cat "$REPORT/uninstall.log"
cat <<'EOF'

Follow the removal steps printed above (profile lines first, then the binary, then the installer's directory),
in another terminal.
EOF
ask "Press Enter here when done: "

# --------------------------------------------------------------------------------------------------- after
heading "After removal"
if [ -e "$KC" ]; then step binary-removed FAIL "$KC still exists"; else step binary-removed PASS "$KC removed"; fi
if [ -e "$INSTALLER_DIR" ]; then step installer-dir-removed FAIL "$INSTALLER_DIR still exists"
else step installer-dir-removed PASS "$INSTALLER_DIR removed"; fi
profiles_snapshot "$REPORT/profiles-after-removal"
profiles_diff "$REPORT/profiles-before" "$REPORT/profiles-after-removal" > "$REPORT/profiles-removal.diff"
if grep -q 'knowcode' "$REPORT/profiles-removal.diff"; then step profiles-restored FAIL "see profiles-removal.diff"
else step profiles-restored PASS "no KnowCode line left in a shell profile"; fi
for s in $SHELLS; do
  n=$(basename "$s")
  new_shell "$s" 'command -v knowcode || echo "knowcode: not found"' "$REPORT/shell-after-$n.out" "$REPORT/shell-after-$n.err"
  if [ "$(cat "$REPORT/shell-after-$n.out")" != "knowcode: not found" ]; then
    step "gone-shell-$n" FAIL "$(tr '\n' ' ' < "$REPORT/shell-after-$n.out")"
  elif ! cmp -s "$REPORT/shell-before-$n.err" "$REPORT/shell-after-$n.err"; then
    step "gone-shell-$n" FAIL "new $n shell prints errors it didn't before: $(tr '\n' ' ' < "$REPORT/shell-after-$n.err")"
  else
    step "gone-shell-$n" PASS "knowcode gone; the shell starts without new errors"
  fi
done

# --------------------------------------------------------------------------------------------------- report
helper summary "$REPORT" > "$REPORT/summary.md"
helper use "$REPORT" > "$REPORT/knowcode-use.json"
printf 'elapsed_seconds\t%s\n' "$(( $(date +%s) - STARTED ))" > "$REPORT/elapsed.tsv"
ARCHIVE="$WORK/clean-machine-report.tar.gz"
tar -czf "$ARCHIVE" -C "$WORK" report
heading "Summary"
cat "$REPORT/summary.md"
echo
echo "Report: $ARCHIVE (contains local paths and measurements: review before sharing)"
grep -q "	FAIL	" "$STEPS" && exit 1
exit 0
