#!/usr/bin/env bash
set -euo pipefail

native_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
web_root=${MW_WEB_ROOT:-"$native_root/../modern-wars"}

if [[ ! -f "$web_root/src/scenario-codec.js" ]]; then
	printf 'Modern Wars web checkout not found at %s\n' "$web_root" >&2
	exit 1
fi

cargo build --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools

scenarios=(
	"world-map-2022-v2.mwsc.gz"
	"world-war-1-1914-v2.mwsc.gz"
	"world-war-2-v2.mwsc.gz"
)
resolutions=("0.1" "0.15" "0.25")

for scenario in "${scenarios[@]}"; do
	path="$web_root/assets/maps/compiled/$scenario"
	for resolution in "${resolutions[@]}"; do
		rust_output=$("$native_root/target/debug/mw-tools" inspect "$path" --grid-res "$resolution" --json)
		js_output=$(node "$native_root/scripts/js-scenario-parity.mjs" "$web_root" "$path" "$resolution")
		if ! diff -u \
			<(jq --sort-keys 'del(.decode_ms)' <<<"$rust_output") \
			<(jq --sort-keys . <<<"$js_output"); then
			printf 'Parity failed: %s at %s degrees\n' "$scenario" "$resolution" >&2
			exit 1
		fi
		printf 'parity ok: %s at %s degrees\n' "$scenario" "$resolution"
	done
done

modern_path="$web_root/assets/maps/compiled/world-map-2022-v2.mwsc.gz"
for resolution in "${resolutions[@]}"; do
	rust_output=$("$native_root/target/debug/mw-tools" field-bench "$modern_path" --grid-res "$resolution" --repeat 1 --json)
	js_output=$(node "$native_root/scripts/js-direction-parity.mjs" "$web_root" "$modern_path" "$resolution")
	if ! diff -u \
		<(jq --sort-keys 'del(.median_ms)' <<<"$rust_output") \
		<(jq --sort-keys . <<<"$js_output"); then
		printf 'Direction-field parity failed at %s degrees\n' "$resolution" >&2
		exit 1
	fi
	printf 'direction parity ok: Russia vs China at %s degrees\n' "$resolution"
done

node "$native_root/scripts/compare-tactical-parity.mjs" \
	<("$native_root/target/debug/mw-tools" tactical-fixture \
		"$native_root/fixtures/tactical-grid-v1.json" --json) \
	<(node "$native_root/scripts/js-tactical-reference.mjs" report \
		"$web_root" "$native_root/fixtures/tactical-grid-v1.json")

node "$native_root/scripts/compare-unit-kernel-parity.mjs" \
	<(node "$native_root/scripts/js-unit-kernel-reference.mjs" report \
		"$web_root" "$native_root/fixtures/movement-combat-v1.json") \
	<("$native_root/target/debug/mw-tools" unit-fixture \
		"$native_root/fixtures/movement-combat-v1.json" --json)
