# Resolve credentials from private files during local development, so ad-hoc
# rebuilds never trigger Keychain permission dialogs.
#
# Usage (before cargo run / cargo tauri dev, in the same shell):
#   source scripts/dev-env.sh
#
# Exports NAME_FILE for every non-empty regular file in
# ${PTA_DEV_CREDENTIALS_DIR:-$HOME/.config/pta}. Bootstrap and rationale:
# docs/architecture.md, section 4. Nothing here contains or prints secrets.

dev_dir="${PTA_DEV_CREDENTIALS_DIR:-$HOME/.config/pta}"

if [ ! -d "$dev_dir" ]; then
	echo "dev-env: $dev_dir does not exist; see docs/architecture.md (section 4)" >&2
	return 1 2>/dev/null || exit 1
fi

for file in "$dev_dir"/*; do
	[ -f "$file" ] && [ -s "$file" ] || continue
	name=${file##*/}
	case "$name" in
		[A-Z]*) ;;
		*)
			echo "dev-env: ignoring $name (invalid credential name)" >&2
			continue
			;;
	esac
	case "$name" in
		*[!A-Z0-9_]*)
			echo "dev-env: ignoring $name (invalid credential name)" >&2
			continue
			;;
	esac
	export "${name}_FILE=$file"
done
