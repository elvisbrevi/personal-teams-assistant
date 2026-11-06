#!/bin/sh
# Runs install.sh against an archive this workflow just built, the way a person would, into a
# temporary installation root and a temporary profile (PTA_PROFILE_DIR): never a real profile, and
# never the network (the script's test source, enabled only with PTA_INSTALL_TEST=1).
#
#   scripts/install/test-install.sh DIST VERSION
#
# DIST holds personal-teams-assistant-<target>-v<VERSION>.tgz and its .sha256. With
# PTA_INSTALL_TEST_NO_SERVICE=1 the login service is neither registered nor checked (a computer
# without a per-user service manager, such as a container); PTA_INSTALL_TEST_RELEASE=X.Y.Z also
# installs that published release from GitHub through the real download path.
# The sh -c checks take their values as arguments, so they are quoted on purpose.
# shellcheck disable=SC2016
set -eu

dist=$(cd "$1" && pwd)
version=$2
here=$(cd "$(dirname "$0")" && pwd)
script="$here/install.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/pta-install-test.XXXXXX")
root="$work/root"
export PTA_INSTALL_TEST=1
export PTA_PROFILE_DIR="$work/profile"
# A clean Cargo home: a Cargo installation is tested on purpose below.
export CARGO_HOME="$work/cargo"
service=yes
extra=
if [ "${PTA_INSTALL_TEST_NO_SERVICE:-}" = 1 ]; then
    service=no
    extra=--no-service
fi
failures=0

pass() { printf 'ok   - %s\n' "$1"; }
bad() {
    printf 'FAIL - %s\n' "$1"
    failures=$((failures + 1))
}
check() {
    description=$1
    shift
    if "$@"; then pass "$description"; else bad "$description"; fi
}
run() { sh "$script" --root "$root" ${extra:+"$extra"} "$@" >"$work/out" 2>&1; }
said() { grep -q "$1" "$work/out"; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'; else shasum -a 256 "$1" | awk '{print $1}'; fi; }
pta() { "$root/bin/pta" "$@" </dev/null; }

cleanup() {
    if [ -x "$root/bin/pta" ]; then
        sh "$script" --root "$root" --uninstall >/dev/null 2>&1 || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

archive=$(cd "$dist" && ls personal-teams-assistant-*-v"$version".tgz)
mkdir -p "$work/source" "$work/tampered" "$work/empty" "$work/fake" "$PTA_PROFILE_DIR" "$CARGO_HOME"
cp "$dist/$archive" "$dist/$archive.sha256" "$work/source/"
cp "$dist/$archive" "$dist/$archive.sha256" "$work/tampered/"
printf 'X' | dd of="$work/tampered/$archive" bs=1 seek=4096 conv=notrunc 2>/dev/null
# An existing profile is adopted as it is.
printf 'synthetic profile marker\n' >"$PTA_PROFILE_DIR/adopted.txt"

echo "# Refusals that change nothing"
check "a digest mismatch is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/tampered" "$work/out"
check "  and says what happened" said "does not match the release's published SHA-256"
check "  and installs nothing" test ! -e "$root/bin"
check "a missing archive is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" --version "$3" >"$4" 2>&1' "$script" "$root" "$work/empty" "$version" "$work/out"
check "  and says what to do" said "has no archive for"
printf '#!/bin/sh\ncase "$1" in -m) echo riscv64 ;; *) echo Linux ;; esac\n' >"$work/fake/uname"
chmod +x "$work/fake/uname"
check "an unsupported target is refused" sh -c '! PATH="$0:$PATH" PTA_INSTALL_TEST_TARGET= sh "$1" --root "$2" --test-source "$3" >"$4" 2>&1' "$work/fake" "$script" "$root" "$work/source" "$work/out"
check "  and points to Cargo" said "There is no prebuilt archive"
printf '{"installs":{"personal-teams-assistant 0.11.2 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["personal-teams-assistant","pta"]}}}\n' >"$CARGO_HOME/.crates2.json"
check "a second copy next to a Cargo installation is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "  and says how to switch" said "cargo uninstall personal-teams-assistant"
rm -f "$CARGO_HOME/.crates2.json"
check "the test source needs the test mode" sh -c '! PTA_INSTALL_TEST= sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "a coordinator origin must be HTTPS" sh -c '! sh "$0" --root "$1" --test-source "$2" --origin http://portal.example >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "nothing was installed by any refusal" test ! -e "$root/bin"

echo "# Install"
if run --test-source "$work/source"; then pass "install.sh installs"; else bad "install.sh installs"; cat "$work/out"; fi
check "it checked the digest" said "matches its published SHA-256"
check "it says the existing profile is used" said "Using the existing profile"
check "pta --version from the installed path" sh -c '"$0/bin/pta" --version | grep -q "$1"' "$root" "$version"
check "both binaries are installed" test -x "$root/bin/personal-teams-assistant"
record="$root/install.json"
check "the record marks an archive installation" grep -q '"kind": "archive"' "$record"
check "  of this package" grep -q '"package": "personal-teams-assistant"' "$record"
check "  and version" grep -q "\"version\": \"$version\"" "$record"
target=${archive#personal-teams-assistant-}
target=${target%-v"$version".tgz}
check "  and target ($target)" grep -q "\"target\": \"$target\"" "$record"
check "  with the archive's digest" grep -q "\"sha256\": \"$(sha "$dist/$archive")\"" "$record"
update=$(pta --json update status 2>&1 || true)
check "the updater recognizes the archive kind" sh -c 'printf "%s" "$0" | grep -q "\"kind\":\"archive\""' "$update"
check "  as a supported installation it updates itself" sh -c 'printf "%s" "$0" | grep -q "\"supported\":true"' "$update"
check "  with unattended updates off by default" sh -c 'printf "%s" "$0" | grep -q "\"unattended\":{[^}]*\"enabled\":false"' "$update"
check "  and says how to turn them on" said "update unattended on --confirmed"
case "${SHELL:-/bin/sh}" in
    */zsh) shell_flags=-lic ;;
    *) shell_flags=-lc ;;
esac
found=$(env -i HOME="$HOME" USER="${USER:-}" SHELL="${SHELL:-/bin/sh}" PATH=/usr/bin:/bin "${SHELL:-/bin/sh}" $shell_flags 'command -v pta' 2>/dev/null | tail -n 1 || true)
check "a new shell finds pta ($found)" sh -c '[ "$0" = "$1/bin/pta" ] || [ "$(readlink "$0" 2>/dev/null)" = "$1/bin/pta" ]' "$found" "$root"
check "the profile was adopted, not replaced" grep -q "synthetic profile marker" "$PTA_PROFILE_DIR/adopted.txt"
machine=$(printf '{}' | "$root/bin/pta" --json tools call machine_settings 2>/dev/null | sed -n 's/.*"id":"\([0-9a-f-]*\)".*/\1/p')
check "the installation has a machine identity" test -n "$machine"
if [ "$service" = yes ]; then
    status=$(pta --json service status)
    name=$(printf '%s' "$status" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
    check "the login service is installed ($name)" sh -c 'printf "%s" "$0" | grep -q "\"installed\":true"' "$status"
    check "  and starts this installation" sh -c 'printf "%s" "$0" | grep -q "\"current\":true"' "$status"
    check "  and runs the host now" sh -c 'printf "%s" "$0" | grep -q "\"running\":true"' "$status"
fi

echo "# Again: repair or upgrade in place"
before=$(sha "$record")
cp "$root/bin/pta" "$work/pta.before"
check "a failed upgrade is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/tampered" "$work/out"
check "  and leaves the installation working" sh -c '"$0/bin/pta" --version | grep -q "$1"' "$root" "$version"
check "  and its record unchanged" test "$(sha "$record")" = "$before"
rm -f "$root/bin/pta"
if run --test-source "$work/source" --unattended-updates; then pass "running it again succeeds"; else bad "running it again succeeds"; cat "$work/out"; fi
check "it repaired a missing binary" cmp -s "$root/bin/pta" "$work/pta.before"
update=$(pta --json update status 2>&1 || true)
check "--unattended-updates turned unattended updates on" sh -c 'printf "%s" "$0" | grep -q "\"unattended\":{[^}]*\"enabled\":true"' "$update"
for file in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.zprofile"; do
    [ -f "$file" ] || continue
    count=$(grep -c "personal-teams-assistant (install.sh) >>>" "$file" || true)
    check "at most one PATH block in $file" test "$count" -le 1
done
again=$(printf '{}' | "$root/bin/pta" --json tools call machine_settings 2>/dev/null | sed -n 's/.*"id":"\([0-9a-f-]*\)".*/\1/p')
check "the same machine identity is kept" test "$again" = "$machine"
check "the profile is still the adopted one" grep -q "synthetic profile marker" "$PTA_PROFILE_DIR/adopted.txt"
if [ "$service" = yes ]; then
    status=$(pta --json service status)
    check "the service is still installed and running" sh -c 'printf "%s" "$0" | grep -q "\"running\":true" && printf "%s" "$0" | grep -q "\"current\":true"' "$status"
fi

echo "# Uninstall"
if sh "$script" --root "$root" --uninstall >"$work/out" 2>&1; then pass "--uninstall succeeds"; else bad "--uninstall succeeds"; cat "$work/out"; fi
check "the binaries are removed" test ! -e "$root/bin"
check "the record is removed" test ! -e "$record"
check "the profile is kept" grep -q "synthetic profile marker" "$PTA_PROFILE_DIR/adopted.txt"
for file in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.zprofile"; do
    [ -f "$file" ] || continue
    check "no PATH block left in $file" sh -c '! grep -q "personal-teams-assistant (install.sh)" "$0"' "$file"
done
check "no link left in ~/.local/bin" sh -c '! [ -L "$0/.local/bin/pta" ] || [ -e "$0/.local/bin/pta" ]' "$HOME"
if [ "$service" = yes ] && [ -n "${name:-}" ]; then
    case "$(uname -s)" in
        Darwin) entry="$HOME/Library/LaunchAgents/$name.plist" ;;
        *) entry="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$name" ;;
    esac
    check "the login entry is removed" test ! -e "$entry"
fi

# The real download path (release redirect, asset host, digest), with PTA_INSTALL_TEST_RELEASE set to
# a version already published with archives: no test source, no service.
if [ -n "${PTA_INSTALL_TEST_RELEASE:-}" ]; then
    echo "# A published release, downloaded from GitHub"
    net="$work/release-root"
    if PTA_INSTALL_TEST_TARGET='' sh "$script" --root "$net" --no-service --version "$PTA_INSTALL_TEST_RELEASE" >"$work/out" 2>&1; then
        pass "release $PTA_INSTALL_TEST_RELEASE installs from GitHub"
    else
        bad "release $PTA_INSTALL_TEST_RELEASE installs from GitHub"
        cat "$work/out"
    fi
    check "  after matching its published SHA-256" said "matches its published SHA-256"
    check "  and runs" sh -c '"$0/bin/pta" --version | grep -q "$1"' "$net" "$PTA_INSTALL_TEST_RELEASE"
    check "  and uninstalls" sh -c 'sh "$0" --root "$1" --uninstall >/dev/null 2>&1 && [ ! -e "$1/bin" ]' "$script" "$net"
fi

if [ "$failures" -gt 0 ]; then
    echo "$failures installer check(s) failed."
    exit 1
fi
echo "Every installer check passed."
