#!/bin/sh
# Installs Personal Teams Assistant on macOS or Linux from this repository's prebuilt release
# archive, without Rust or a compiler, for the current user only (no administrator rights).
#
#   curl --proto '=https' --tlsv1.2 -fsSL \
#     https://github.com/elvisbrevi/personal-teams-assistant/releases/latest/download/install.sh \
#     | sh -s -- --origin https://your-portal.example
#
# What it does, in order: detects the target; resolves the version (the repository's latest
# release unless --version is given); downloads that release's archive and its .sha256 over HTTPS
# from the release, following only the release host's own redirect to its asset storage; checks
# the digest before anything is unpacked or run; verifies build provenance with `gh attestation
# verify` when gh is installed and signed in; unpacks into a temporary directory inside the
# installation root and moves it into place (a failure leaves the previous installation working);
# writes the archive installation record; puts `pta` on the path of new shells; registers the host
# to start at login (`pta service install`); and, with --origin, starts the browser sign-in
# (`pta auth machine login`) so the approval page opens for "Trust this machine".
#
# The command carries no secret: only the coordinator's public HTTPS origin. Running it again
# repairs or upgrades the same installation; the profile (settings, credentials, Microsoft
# session, machine identity) is never created, replaced or removed by this script.
#
# Every statement runs inside main, called on the last line, so a truncated download runs nothing.

REPOSITORY="elvisbrevi/personal-teams-assistant"
PACKAGE="personal-teams-assistant"
CLI="pta"
RECORD="install.json"
BEGIN_MARK="# >>> personal-teams-assistant (install.sh) >>>"
END_MARK="# <<< personal-teams-assistant (install.sh) <<<"

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: %s\n' "$*" >&2; }
fail() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  --origin URL     Your coordinator portal's HTTPS origin: after installing, start the browser
                   sign-in so this computer can be trusted by your account.
  --version X.Y.Z  Install this release instead of the latest one.
  --root DIR       Installation root (default: ~/.local/share/personal-teams-assistant).
  --integrations   Also install the skill and register `pta mcp` with the coding CLIs found
                   (Claude Code, Codex). Without it the commands are printed instead.
  --no-service     Do not register the host to start at login.
  --no-browser     Print the approval link instead of opening the browser.
  --uninstall      Remove the service entry, the binaries and the PATH entry; keep the profile.
  -h, --help       Show this help.
EOF
}

# ---------------------------------------------------------------------------------------------
# Inputs.

# The coordinator origin, validated as the host validates it: https, a host, no credentials,
# path, query or fragment. Prints the normalized origin.
check_origin() {
    set -- "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
    case "$1" in
        https://*) ;;
        *) fail "Use the HTTPS origin of your coordinator portal, for example https://assistant.example.com." ;;
    esac
    rest=${1#https://}
    rest=${rest%/}
    case "$rest" in
        "" | *[!A-Za-z0-9.:-]* | .* | :*)
            fail "Use the HTTPS origin of your coordinator portal (no path, query, credentials or spaces)."
            ;;
    esac
    printf 'https://%s\n' "$rest"
}

check_version() {
    printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' ||
        fail "Use a release version such as 0.11.4."
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Darwin)
            # A shell under Rosetta reports x86_64 on Apple silicon; the native build is better.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then
                arch=arm64
            fi
            case "$arch" in
                arm64 | aarch64) echo aarch64-apple-darwin ;;
                x86_64) echo x86_64-apple-darwin ;;
                *) return 1 ;;
            esac
            ;;
        Linux)
            [ "$arch" = x86_64 ] || return 1
            if ldd --version 2>&1 | grep -qi musl; then
                return 1
            fi
            echo x86_64-unknown-linux-gnu
            ;;
        *) return 1 ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fail "Neither sha256sum nor shasum is available to check the download."
    fi
}

# One field of pta's compact JSON reply: a string or a literal (true, false, null, a number).
json_string() { sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p" | head -n 1; }
json_literal() { sed -n "s/.*\"$1\":\\([a-z0-9]*\\).*/\\1/p" | head -n 1; }

# ---------------------------------------------------------------------------------------------
# Downloads. A release asset is served by github.com with one redirect to its own asset storage;
# only that redirect is followed, and only over HTTPS. The test source is a local directory that
# only PTA_INSTALL_TEST=1 with --test-source enables (CI runs the script against the archive it
# just built); the portal's command never sets either.

curl_https() { curl --proto '=https' --tlsv1.2 --silent --show-error "$@"; }

asset_redirect() {
    case "$1" in
        https://objects.githubusercontent.com/* | https://release-assets.githubusercontent.com/* | https://github-releases.githubusercontent.com/*) return 0 ;;
        *) return 1 ;;
    esac
}

fetch() {
    url="https://github.com/$REPOSITORY/releases/download/v$VERSION/$1"
    if [ -n "$TEST_SOURCE" ]; then
        [ -f "$TEST_SOURCE/$1" ] || return 1
        cp "$TEST_SOURCE/$1" "$2"
        return 0
    fi
    answer=$(curl_https --output /dev/null --write-out '%{http_code} %{redirect_url}' "$url") || return 1
    code=${answer%% *}
    location=${answer#* }
    case "$code" in
        301 | 302 | 303 | 307 | 308) ;;
        *) return 1 ;;
    esac
    asset_redirect "$location" || fail "The release host redirected to an unexpected address; nothing was downloaded."
    curl_https --fail --max-redirs 0 --output "$2" "$location"
}

latest_version() {
    answer=$(curl_https --output /dev/null --write-out '%{http_code} %{redirect_url}' \
        "https://github.com/$REPOSITORY/releases/latest") ||
        fail "GitHub could not be reached to find the latest release. Check the network connection, or pass --version."
    location=${answer#* }
    case "$location" in
        "https://github.com/$REPOSITORY/releases/tag/v"*) printf '%s\n' "${location##*/tag/v}" ;;
        *) fail "The latest release could not be found. Pass --version, or try again later." ;;
    esac
}

# ---------------------------------------------------------------------------------------------
# What is already on this computer.

cargo_record() {
    for records in "$1/.crates2.json" "$1/.crates.toml"; do
        if [ -f "$records" ] && grep -q "\"$PACKAGE " "$records"; then
            return 0
        fi
    done
    return 1
}

# A Cargo (or cargo binstall) installation of this package would be a second copy: two hosts for
# one profile, updated two ways. Refuse and say how to switch.
refuse_cargo_copy() {
    found=
    cargo_home=${CARGO_HOME:-$HOME/.cargo}
    if cargo_record "$cargo_home"; then
        found=$cargo_home
    fi
    if on_path=$(command -v "$CLI" 2>/dev/null) && [ "$on_path" != "$BIN/$CLI" ] &&
        [ "$(resolve "$on_path")" != "$BIN/$CLI" ]; then
        other=$(dirname "$(dirname "$on_path")")
        if cargo_record "$other"; then
            found=$other
        else
            warn "Another $CLI is on your PATH at $on_path; the one installed here may be shadowed by it."
        fi
    fi
    [ -z "$found" ] && return 0
    cat >&2 <<EOF
install.sh: $PACKAGE is already installed with Cargo in $found.
Installing a second copy next to it would give one profile two hosts. To switch to this
installation, which needs no Rust toolchain, run:

    pta service uninstall --confirmed
    pta app quit
    cargo uninstall $PACKAGE

and then this command again. Your profile, credentials and Microsoft session are kept.
To stay with Cargo instead, update with: cargo install $PACKAGE --locked
EOF
    exit 1
}

resolve() {
    if [ -L "$1" ]; then
        target=$(readlink "$1")
        case "$target" in
            /*) printf '%s\n' "$target" ;;
            *) printf '%s/%s\n' "$(dirname "$1")" "$target" ;;
        esac
    else
        printf '%s\n' "$1"
    fi
}

profile_dir() {
    if [ -n "${PTA_PROFILE_DIR:-}" ]; then
        printf '%s\n' "$PTA_PROFILE_DIR"
    elif [ "$(uname -s)" = Darwin ]; then
        printf '%s\n' "$HOME/Library/Application Support/dev.personalteams.assistant"
    else
        printf '%s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/dev.personalteams.assistant"
    fi
}

# ---------------------------------------------------------------------------------------------
# The host.

run_pta() { "$BIN/$CLI" "$@" </dev/null; }

host_running() { run_pta --json status 2>/dev/null | grep -q '"host_running":true'; }

wait_host() {
    tries=0
    while [ "$tries" -lt 60 ]; do
        if host_running; then
            [ "$1" = up ] && return 0
        else
            [ "$1" = down ] && return 0
        fi
        sleep 1
        tries=$((tries + 1))
    done
    return 1
}

quit_host() {
    if [ -x "$BIN/$CLI" ] && host_running; then
        say "Stopping the running host (the assistant's last state is kept)..."
        run_pta --json app quit >/dev/null 2>&1 || true
        wait_host down || warn "The host did not stop within a minute."
    fi
}

# A host started from this shell, in the user's own session (one that pta starts for an isolated
# profile cannot register the user's login entry).
start_host() {
    host_running && return 0
    nohup "$BIN/$PACKAGE" --host >/dev/null 2>&1 </dev/null &
    wait_host up
}

manager_start() {
    case "$(uname -s)" in
        Darwin) launchctl kickstart "gui/$(id -u)/$1" >/dev/null 2>&1 ;;
        *)
            systemctl --user reset-failed "$1" >/dev/null 2>&1 || true
            systemctl --user start "$1" >/dev/null 2>&1
            ;;
    esac
}

install_service() {
    start_host || {
        warn "The host did not start; run '$CLI status' to see why. It was not registered to start at login."
        return 1
    }
    reply=$(run_pta --json service install --confirmed 2>&1) || {
        message=$(printf '%s' "$reply" | json_string message)
        warn "The host was not registered to start at login: ${message:-$reply}"
        warn "It runs now; start it later with '$PACKAGE --host', or fix the cause and run this command again."
        return 1
    }
    name=$(printf '%s' "$reply" | json_string name)
    # Hand the host over to the service manager, so it runs now as it will after a login.
    quit_host
    if [ -n "$name" ] && manager_start "$name" && wait_host up; then
        say "The host runs as a login service ($name)."
    else
        warn "The service manager did not start the host now; it starts at your next login."
        start_host || true
    fi
}

# ---------------------------------------------------------------------------------------------
# PATH for new shells: a link in a per-user bin directory already on PATH, or a marked block in
# the shell's own startup files that is replaced, never duplicated, on every run.

# The line is written literally: $PATH expands in the shell that reads it.
# shellcheck disable=SC2016
path_line() { printf 'case ":$PATH:" in *":%s:"*) ;; *) export PATH="%s:$PATH" ;; esac\n' "$BIN" "$BIN"; }

remove_block() {
    [ -f "$1" ] && grep -qxF "$BEGIN_MARK" "$1" || return 0
    kept=$(mktemp "${TMPDIR:-/tmp}/pta-rc.XXXXXX")
    awk -v b="$BEGIN_MARK" -v e="$END_MARK" '$0 == b {skip = 1; next} $0 == e {skip = 0; next} !skip' "$1" >"$kept"
    # Written in place, so a startup file that is a link (a dotfiles checkout) stays a link.
    cat "$kept" >"$1"
    rm -f "$kept"
}

put_block() {
    line=$(path_line)
    if [ -f "$1" ] && grep -qxF "$BEGIN_MARK" "$1" && grep -qxF "$line" "$1"; then
        return 0
    fi
    remove_block "$1"
    if [ -s "$1" ] && [ "$(tail -c 1 "$1" | wc -l)" -eq 0 ]; then
        printf '\n' >>"$1"
    fi
    printf '%s\n%s\n%s\n' "$BEGIN_MARK" "$line" "$END_MARK" >>"$1"
    say "Added $BIN to PATH in $1."
}

startup_files() {
    for file in .profile .bash_profile .bash_login .bashrc .zprofile .zshrc; do
        [ -f "$HOME/$file" ] && printf '%s\n' "$HOME/$file"
    done
    # The user's own shell reads one of these even when none exists yet.
    case "$(basename "${SHELL:-sh}")" in
        zsh) printf '%s\n' "$HOME/.zshrc" ;;
        bash) [ "$(uname -s)" = Darwin ] && printf '%s\n' "$HOME/.bash_profile" || printf '%s\n' "$HOME/.bashrc" ;;
        *) printf '%s\n' "$HOME/.profile" ;;
    esac
}

fish_file() { printf '%s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/$PACKAGE.fish"; }

link_dir_on_path() {
    dir="$HOME/.local/bin"
    [ -d "$dir" ] || return 1
    case ":$PATH:" in *":$dir:"*) ;; *) return 1 ;; esac
    if [ -e "$dir/$CLI" ] || [ -L "$dir/$CLI" ]; then
        [ "$(resolve "$dir/$CLI")" = "$BIN/$CLI" ] || return 1
    fi
    printf '%s\n' "$dir"
}

setup_path() {
    case "$BIN" in *\'* | *\"* | *\$* | *\\*) fail "The installation root may not contain quotes, \$ or backslashes." ;; esac
    if dir=$(link_dir_on_path); then
        ln -sf "$BIN/$CLI" "$dir/$CLI"
        say "Linked $dir/$CLI, already on your PATH."
    else
        startup_files | sort -u | while read -r file; do put_block "$file"; done
    fi
    fish=$(fish_file)
    if [ -d "$(dirname "$(dirname "$fish")")" ]; then
        mkdir -p "$(dirname "$fish")"
        # shellcheck disable=SC2016
        printf '%s\ncontains "%s" $PATH; or set -gx PATH "%s" $PATH\n' "$BEGIN_MARK" "$BIN" "$BIN" >"$fish"
    fi
}

remove_path() {
    link="$HOME/.local/bin/$CLI"
    if [ -L "$link" ] && [ "$(resolve "$link")" = "$BIN/$CLI" ]; then
        rm -f "$link"
    fi
    for file in .profile .bash_profile .bash_login .bashrc .zprofile .zshrc; do
        remove_block "$HOME/$file"
    done
    fish=$(fish_file)
    if [ -f "$fish" ] && grep -qxF "$BEGIN_MARK" "$fish"; then
        rm -f "$fish"
    fi
}

# ---------------------------------------------------------------------------------------------
# Coding CLIs: the skill and `pta mcp`, with consent; the exact commands otherwise.

register_mcp() {
    if [ "$1" = claude ]; then
        claude mcp add --scope user "$PACKAGE" -- "$2" mcp >/dev/null 2>&1 </dev/null
    else
        codex mcp add "$PACKAGE" -- "$2" mcp >/dev/null 2>&1 </dev/null
    fi
}

integrations() {
    config=$(run_pta --json mcp config 2>/dev/null) || config=
    command=$(printf '%s' "$config" | json_string command)
    [ -n "$command" ] || command="$BIN/$CLI"
    found=
    for tool in claude codex; do
        command -v "$tool" >/dev/null 2>&1 || continue
        found=yes
        skill="$HOME/.$tool/skills/$PACKAGE"
        if [ "$tool" = claude ]; then
            register="claude mcp add --scope user $PACKAGE -- '$command' mcp"
        else
            register="codex mcp add $PACKAGE -- '$command' mcp"
        fi
        if [ "$INTEGRATIONS" != yes ]; then
            say "  $tool: $CLI skill install $skill"
            say "  $tool: $register"
            continue
        fi
        if [ -e "$skill" ]; then
            say "$tool: the skill is already at $skill (updates refresh it while unmodified)."
        elif mkdir -p "$(dirname "$skill")" && run_pta skill install "$skill" >/dev/null; then
            say "$tool: installed the skill at $skill."
        else
            warn "$tool: the skill could not be installed at $skill."
        fi
        if "$tool" mcp get "$PACKAGE" >/dev/null 2>&1 </dev/null; then
            say "$tool: $PACKAGE is already registered as an MCP server."
        elif register_mcp "$tool" "$command"; then
            say "$tool: registered '$command mcp' as the MCP server $PACKAGE."
        else
            warn "$tool: registering the MCP server failed; run: $register"
        fi
    done
    if [ -z "$found" ]; then
        say "No coding CLI (Claude Code, Codex) was found. For any agent, '$CLI mcp config' prints how to add '$CLI mcp'."
    elif [ "$INTEGRATIONS" != yes ]; then
        say "Run them yourself, or run this command again with --integrations."
    fi
}

# ---------------------------------------------------------------------------------------------
# Joining the account.

open_url() {
    [ "$NO_BROWSER" = yes ] && return 1
    case "$(uname -s)" in
        Darwin) open "$1" >/dev/null 2>&1 ;;
        *)
            [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ] && command -v xdg-open >/dev/null 2>&1 &&
                xdg-open "$1" >/dev/null 2>&1
            ;;
    esac
}

join_account() {
    settings=$(printf '{}' | "$BIN/$CLI" --json tools call machine_settings 2>/dev/null) || settings=
    current=$(printf '%s' "$settings" | json_string coordinator)
    if [ -n "$current" ]; then
        current=$(printf '%s' "$current" | sed 's#/*$##')
        if [ "$current" = "$ORIGIN" ]; then
            say "This computer is already connected to $ORIGIN; its machine identity is kept."
        else
            warn "This computer is connected to $current. Disconnect it there first ('$CLI tools call machine_disconnect'), then run this command again."
        fi
        return 0
    fi
    reply=$(run_pta --json auth machine login "$ORIGIN" --no-browser 2>&1) || true
    url=$(printf '%s' "$reply" | json_string approval_url)
    code=$(printf '%s' "$reply" | json_string code)
    case "$url" in
        "$ORIGIN"/login\?enroll=*) ;;
        *)
            message=$(printf '%s' "$reply" | json_string message)
            warn "The sign-in could not start: ${message:-$reply}"
            warn "Run '$CLI auth machine login $ORIGIN' when it is fixed."
            return 1
            ;;
    esac
    say ""
    say "Approve this computer in your browser: $url"
    say "Check that the page shows the code $code, then choose Trust this machine."
    open_url "$url" || say "Open that link on any device where you are signed in to the portal."
    say "Waiting for the approval (up to ten minutes)..."
    finished=$(run_pta --json auth machine finish --wait 2>&1) || true
    if printf '%s' "$finished" | grep -q '"connected":true'; then
        say "Connected to $ORIGIN. Choose this computer's allowed directories in the portal (Settings > Machines) before running work on it."
    else
        message=$(printf '%s' "$finished" | json_string message)
        warn "Not connected yet: ${message:-no approval}. After approving, run '$CLI auth machine finish --wait'."
    fi
}

# ---------------------------------------------------------------------------------------------
# Install and uninstall.

write_record() {
    cat >"$1" <<EOF
{
  "schema": 1,
  "kind": "archive",
  "package": "$PACKAGE",
  "version": "$VERSION",
  "target": "$TARGET",
  "bins": ["$PACKAGE", "$CLI"],
  "repository": "https://github.com/$REPOSITORY",
  "archive": "$ARCHIVE",
  "sha256": "$DIGEST",
  "installed_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
}

do_install() {
    if [ -n "$TEST_SOURCE" ] && [ -n "${PTA_INSTALL_TEST_TARGET:-}" ]; then
        # The Intel macOS archive is tested on an Apple-silicon runner, under Rosetta.
        TARGET=$PTA_INSTALL_TEST_TARGET
    else
        TARGET=$(detect_target) || fail "There is no prebuilt archive for $(uname -s) $(uname -m). Install with Cargo instead: cargo install $PACKAGE --locked (needs the Rust toolchain from https://rustup.rs)."
    fi
    command -v curl >/dev/null 2>&1 || [ -n "$TEST_SOURCE" ] || fail "curl is needed to download the release."
    command -v tar >/dev/null 2>&1 || fail "tar is needed to unpack the release."
    refuse_cargo_copy
    if [ -z "$VERSION" ]; then
        if [ -n "$TEST_SOURCE" ]; then
            VERSION=$(find "$TEST_SOURCE" -name "$PACKAGE-$TARGET-v*.tgz" | sed -n 's/.*-v\([0-9.]*\)\.tgz$/\1/p' | head -n 1)
        else
            VERSION=$(latest_version)
        fi
    fi
    check_version "$VERSION"
    ARCHIVE="$PACKAGE-$TARGET-v$VERSION.tgz"
    say "Installing $PACKAGE $VERSION for $TARGET into $ROOT."

    mkdir -p "$ROOT"
    STAGE=$(mktemp -d "$ROOT/.stage.XXXXXX") || fail "$ROOT is not writable."
    trap 'rm -rf "$STAGE"' EXIT
    trap 'exit 130' INT TERM
    if ! fetch "$ARCHIVE" "$STAGE/$ARCHIVE" || ! fetch "$ARCHIVE.sha256" "$STAGE/$ARCHIVE.sha256"; then
        fail "Release $VERSION has no archive for $TARGET, or it could not be downloaded. Nothing was changed. Check the version and the network connection and try again."
    fi
    expected=$(awk 'NR == 1 {print tolower($1)}' "$STAGE/$ARCHIVE.sha256")
    named=$(awk 'NR == 1 {sub(/^\*/, "", $2); print $2}' "$STAGE/$ARCHIVE.sha256")
    DIGEST=$(sha256_of "$STAGE/$ARCHIVE")
    if [ "$named" != "$ARCHIVE" ] || [ "$DIGEST" != "$expected" ]; then
        fail "The download does not match the release's published SHA-256. Nothing was changed. Try again; if it keeps failing, report it at https://github.com/$REPOSITORY/issues."
    fi
    say "The archive matches its published SHA-256."
    if [ -n "$TEST_SOURCE" ]; then
        say "Build provenance not checked (test source)."
    elif command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        gh attestation verify "$STAGE/$ARCHIVE" --repo "$REPOSITORY" >/dev/null 2>&1 ||
            fail "GitHub could not verify the archive's build provenance. Nothing was changed. Run 'gh attestation verify' on the archive to see why."
        say "Build provenance verified with gh attestation verify."
    else
        say "Build provenance not checked: install the GitHub CLI (gh) and sign in to verify it. The SHA-256 matched."
    fi

    mkdir "$STAGE/unpacked" "$STAGE/bin"
    tar -xzf "$STAGE/$ARCHIVE" -C "$STAGE/unpacked" || fail "The archive could not be unpacked. Nothing was changed."
    for name in "$PACKAGE" "$CLI"; do
        file="$STAGE/unpacked/$PACKAGE-$TARGET-v$VERSION/$name"
        [ -f "$file" ] && [ ! -L "$file" ] || fail "The archive does not contain $name. Nothing was changed."
        cp "$file" "$STAGE/bin/$name"
        chmod 755 "$STAGE/bin/$name"
    done
    reported=$("$STAGE/bin/$CLI" --version --json </dev/null 2>/dev/null) || reported=
    printf '%s' "$reported" | grep -q "\"cli_version\":\"$VERSION\"" ||
        fail "The unpacked pta does not run on this computer or reports another version. Nothing was changed."

    if [ -f "$(profile_dir)/config.toml" ] || [ -d "$(profile_dir)" ]; then
        say "Using the existing profile at $(profile_dir); it is kept as it is."
    fi
    # Only now does anything installed change: stop the old host, swap the bin directory.
    quit_host
    if [ -d "$BIN" ]; then
        mv "$BIN" "$STAGE/previous" || fail "The previous installation could not be replaced. Nothing was changed."
    fi
    if ! mv "$STAGE/bin" "$BIN"; then
        [ -d "$STAGE/previous" ] && mv "$STAGE/previous" "$BIN"
        fail "The new binaries could not be moved into place; the previous installation was restored."
    fi
    write_record "$STAGE/$RECORD"
    mv "$STAGE/$RECORD" "$ROOT/$RECORD"
    say "Installed $("$BIN/$CLI" --version </dev/null) in $BIN."

    setup_path
    if [ "$NO_SERVICE" = yes ]; then
        say "Not registered to start at login (--no-service)."
    else
        install_service || true
    fi
    if [ -n "$ORIGIN" ]; then
        join_account || true
    fi
    say ""
    say "Skill and MCP for coding CLIs on this computer:"
    integrations
    say ""
    say "Open a new terminal to use '$CLI'. To remove the binaries later: install.sh --uninstall (the profile is kept)."
}

do_uninstall() {
    if ! grep -q '"kind": "archive"' "$ROOT/$RECORD" 2>/dev/null; then
        fail "No installation by this script was found in $ROOT."
    fi
    if [ -x "$BIN/$CLI" ]; then
        if start_host; then
            run_pta --json service uninstall --confirmed >/dev/null 2>&1 ||
                warn "The login entry could not be removed; remove it with '$CLI service uninstall --confirmed'."
        fi
        quit_host
    fi
    remove_path
    rm -rf "${ROOT:?}/bin" "${ROOT:?}/$RECORD"
    rmdir "$ROOT" 2>/dev/null || true
    say "Removed $PACKAGE from $ROOT. The profile at $(profile_dir) was kept."
    say "A skill copy or MCP registration made for a coding CLI stays; remove it there (for example 'claude mcp remove $PACKAGE')."
}

main() {
    set -eu
    ORIGIN=
    VERSION=
    ROOT=${PTA_INSTALL_ROOT:-}
    INTEGRATIONS=no
    NO_SERVICE=no
    NO_BROWSER=no
    UNINSTALL=no
    TEST_SOURCE=
    while [ $# -gt 0 ]; do
        case "$1" in
            --origin) [ $# -ge 2 ] || fail "--origin needs a value."; ORIGIN=$2; shift ;;
            --origin=*) ORIGIN=${1#*=} ;;
            --version) [ $# -ge 2 ] || fail "--version needs a value."; VERSION=$2; shift ;;
            --version=*) VERSION=${1#*=} ;;
            --root) [ $# -ge 2 ] || fail "--root needs a value."; ROOT=$2; shift ;;
            --root=*) ROOT=${1#*=} ;;
            --integrations) INTEGRATIONS=yes ;;
            --no-service) NO_SERVICE=yes ;;
            --no-browser) NO_BROWSER=yes ;;
            --uninstall) UNINSTALL=yes ;;
            --test-source) [ $# -ge 2 ] || fail "--test-source needs a value."; TEST_SOURCE=$2; shift ;;
            -h | --help) usage; exit 0 ;;
            *) usage >&2; fail "Unknown option $1." ;;
        esac
        shift
    done
    [ -n "${HOME:-}" ] || fail "HOME is not set."
    if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ]; then
        fail "Run this without sudo: it installs for the current user only and needs no administrator rights."
    fi
    if [ -n "$TEST_SOURCE" ]; then
        [ "${PTA_INSTALL_TEST:-}" = 1 ] || fail "--test-source is only for the installer's own tests."
        [ -d "$TEST_SOURCE" ] || fail "The test source must be a local directory."
        TEST_SOURCE=$(cd "$TEST_SOURCE" && pwd)
        warn "TEST MODE: installing from $TEST_SOURCE instead of the release."
    fi
    [ -z "$ORIGIN" ] || ORIGIN=$(check_origin "$ORIGIN")
    [ -z "$VERSION" ] || check_version "$VERSION"
    [ -n "$ROOT" ] || ROOT="$HOME/.local/share/$PACKAGE"
    case "$ROOT" in /*) ;; *) ROOT="$(pwd)/$ROOT" ;; esac
    ROOT=${ROOT%/}
    BIN="$ROOT/bin"
    if [ "$UNINSTALL" = yes ]; then
        do_uninstall
    else
        do_install
    fi
}

main "$@"
