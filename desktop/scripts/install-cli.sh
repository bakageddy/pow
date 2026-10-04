#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/.."
source_binary="src-tauri/target/release/pow"
if [ ! -f "$source_binary" ]; then
  echo "Missing $source_binary; run 'npm run tauri -- build --no-bundle' first." >&2
  exit 1
fi
install_dir="${HOME}/.local/bin"
mkdir -p "$install_dir"
install -m 755 "$source_binary" "$install_dir/pow"
printf 'Installed %s/pow\n' "$install_dir"
case ":${PATH}:" in
  *":${install_dir}:"*) printf 'Run: pow\n' ;;
  *) printf 'Add %s to your PATH, then run: pow\n' "$install_dir" ;;
esac
