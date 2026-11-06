#!/usr/bin/env bash
# A development host for GUI work: a debug build on a disposable profile with synthetic data that
# serves desktop/ui from this checkout on every request (PTA_DEV_UI_DIR), so editing a GUI file and
# refreshing the browser is the whole loop. See docs/ui-design-conventions.md (verification).
#
#   scripts/dev-host.sh            build, start the host, print where to open it; Ctrl-C stops it
#                                  and deletes the profile
#   scripts/dev-host.sh open       print a new sign-in address for the running development host
#
# It never touches a real profile, credential or Microsoft session: the host runs with an emptied
# environment (no inherited credential variables), PTA_PROFILE_DIR, HOME and the XDG directories
# inside a new temporary directory, and file credentials there; it stops if any of that fails.
# Instead of a browser, the host's opener is a stand-in that records the single-use sign-in
# address (valid for a minute) so this script can print it; open it in any browser that reaches
# this computer's 127.0.0.1. Unix only (Linux and macOS).
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
bin=$target/debug
state=$target/dev-host/current

fail() { echo "dev-host: $*" >&2; exit 1; }

case "$(uname -s)" in
  Linux | Darwin) ;;
  *) fail "Unix only (Linux and macOS)." ;;
esac

# The host and pta with nothing inherited but the search path, the locale and proxy settings: every
# directory either of them may read or write is inside $root.
in_profile() {
  env -i \
    PATH="$root/bin:$PATH" LANG="${LANG:-C.UTF-8}" TERM=dumb \
    ${HTTPS_PROXY:+HTTPS_PROXY="$HTTPS_PROXY"} ${NO_PROXY:+NO_PROXY="$NO_PROXY"} \
    ${SSL_CERT_FILE:+SSL_CERT_FILE="$SSL_CERT_FILE"} \
    HOME="$root/home" TMPDIR="$root/tmp" \
    XDG_CONFIG_HOME="$root/home/.config" XDG_DATA_HOME="$root/home/.local/share" \
    XDG_CACHE_HOME="$root/home/.cache" XDG_STATE_HOME="$root/home/.local/state" \
    DISPLAY=:pta-dev-host \
    PTA_PROFILE_DIR="$root/profile" \
    "$@"
}

# Refuses any root that is not a directory this script created, and the real profile.
check_root() {
  [ -n "${root:-}" ] && [ -d "$root" ] && [ -f "$root/.pta-dev-host" ] || fail "no development profile at '${root:-}'."
  case "$(basename "$root")" in pta-dev-host.??????) ;; *) fail "refusing a profile this script did not create: $root" ;; esac
  for real in "${XDG_CONFIG_HOME:-$HOME/.config}/dev.personalteams.assistant" \
              "$HOME/Library/Application Support/dev.personalteams.assistant"; do
    case "$root/profile" in "$real" | "$real"/*) fail "refusing the real profile." ;; esac
  done
}

# Asks the host for a new single-use sign-in and prints its address.
sign_in() {
  rm -f "$root/opened"
  in_profile "$bin/pta" app open > /dev/null || fail "the host refused to open its page; see $root/host.log."
  for _ in $(seq 50); do [ -s "$root/opened" ] && break; sleep 0.1; done
  [ -s "$root/opened" ] || fail "the host did not hand over a sign-in; see $root/host.log."
  local handoff
  handoff=$(sed 's|^file://||' "$root/opened")
  case "$handoff" in "$root/profile/"*) ;; *) fail "unexpected sign-in handoff." ;; esac
  url=$(grep -o 'http://127\.0\.0\.1:[0-9]*/open?code=[0-9a-f]*' "$handoff" | head -1)
  [ -n "$url" ] || fail "the sign-in handoff holds no address."
  echo "Open within a minute (single use): $url"
  echo "A new address: scripts/dev-host.sh open"
}

if [ "${1:-start}" = open ]; then
  root=$(cat "$state" 2>/dev/null) || fail "no development host is running (start it with scripts/dev-host.sh)."
  check_root
  sign_in
  exit 0
fi
[ "${1:-start}" = start ] || fail "usage: scripts/dev-host.sh [open]"

(cd "$repo" && cargo build --bins --quiet)
[ -x "$bin/personal-teams-assistant" ] && [ -x "$bin/pta" ] || fail "debug binaries missing in $bin."

root=$(mktemp -d "${TMPDIR:-/tmp}/pta-dev-host.XXXXXX")
root=$(cd "$root" && pwd -P)
touch "$root/.pta-dev-host"
check_root
mkdir -p "$root/bin" "$root/home" "$root/tmp"
chmod 700 "$root"
host=
cleanup() {
  if [ -n "$host" ] && kill -0 "$host" 2> /dev/null; then
    in_profile "$bin/pta" app quit > /dev/null 2>&1 || kill "$host" 2> /dev/null || true
    wait "$host" 2> /dev/null || true
  fi
  [ "$(cat "$state" 2> /dev/null)" != "$root" ] || rm -f "$state"
  rm -rf "$root"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# The browser opener the host calls with its handoff file's address (xdg-open on Linux, open on
# macOS): recorded for sign_in instead of opening a window.
for opener in xdg-open open; do
  printf '#!/bin/sh\nprintf "%%s\\n" "$1" > "%s/opened"\n' "$root" > "$root/bin/$opener"
  chmod 700 "$root/bin/$opener"
done

in_profile PTA_DEV_UI_DIR="$repo/desktop/ui" "$bin/personal-teams-assistant" --host > "$root/host.log" 2>&1 &
host=$!
for _ in $(seq 100); do
  [ -f "$root/profile/control.json" ] && break
  kill -0 "$host" 2> /dev/null || fail "the host stopped; see $root/host.log."
  sleep 0.1
done
[ -f "$root/profile/control.json" ] || fail "the host did not start on the development profile."
grep -q 'gui_from_disk' "$root/host.log" || fail "the host is not serving desktop/ui from disk; see $root/host.log."

# Synthetic data only.
for note in \
  '{"title":"Synthetic release window","text":"Releases of the example service go out on Tuesdays after the demo review.","keywords":["release","example"],"contexts":[]}' \
  '{"title":"Synthetic on-call rotation","text":"The example team rotates on-call weekly; the handover happens on Monday mornings.","keywords":["on-call","example"],"contexts":[]}'; do
  printf '%s' "$note" | in_profile "$bin/pta" notes add > /dev/null || fail "could not add synthetic notes."
done

mkdir -p "$(dirname "$state")"
printf '%s\n' "$root" > "$state"
echo "Development host on $root (log: $root/host.log), serving $repo/desktop/ui from disk."
sign_in
echo "Edit desktop/ui and refresh the browser. Ctrl-C stops the host and deletes the profile."
wait "$host"
