#!/usr/bin/env bash
set -euo pipefail

native_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
web_root=${MW_WEB_ROOT:-"$native_root/../modern-wars"}

if [[ ! -f "$web_root/src/scenario-codec.js" ]]; then
	printf 'Modern Wars web checkout not found at %s\n' "$web_root" >&2
	exit 1
fi

if [[ ! -f "$web_root/scripts/native-runtime-checkpoint-v2-smoke.mjs" ]]; then
	printf 'Modern Wars checkpoint v2 smoke test not found at %s\n' "$web_root" >&2
	exit 1
fi

node "$web_root/scripts/native-runtime-checkpoint-smoke.mjs"
node "$web_root/scripts/native-runtime-checkpoint-v2-smoke.mjs"
node "$native_root/scripts/js-side-dynamics-reference.mjs"

cargo build --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools -p mw-native
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-core committed_state_restore
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools checkpoint_v2

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

node "$native_root/scripts/compare-front-layout-parity.mjs" \
	<("$native_root/target/debug/mw-tools" front-layout-fixture \
		"$native_root/fixtures/front-layout-v1.json" --json) \
	<(node "$native_root/scripts/js-front-layout-reference.mjs" report \
		"$web_root" "$native_root/fixtures/front-layout-v1.json")

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

node "$native_root/scripts/compare-native-tick-parity.mjs" \
	<(node "$native_root/scripts/js-native-tick-reference.mjs" report \
		"$web_root" "$native_root/fixtures/native-tick-v1.json") \
	<("$native_root/target/debug/mw-tools" native-tick-fixture \
		"$native_root/fixtures/native-tick-v1.json" --json)

node "$native_root/scripts/compare-ai-orders-parity.mjs" \
	<(node "$native_root/scripts/js-ai-orders-reference.mjs" \
		"$native_root/fixtures/ai-orders-v1.json" report) \
	<("$native_root/target/debug/mw-tools" ai-orders-fixture \
		"$native_root/fixtures/ai-orders-v1.json" --json)

node "$native_root/scripts/compare-territory-control-parity.mjs" \
	<(node "$native_root/scripts/js-territory-control-reference.mjs" \
		"$native_root/fixtures/territory-control-v1.json" report) \
	<("$native_root/target/debug/mw-tools" territory-control-fixture \
		"$native_root/fixtures/territory-control-v1.json" --json)

node "$native_root/scripts/compare-strategic-cycle-parity.mjs" \
	<(node "$native_root/scripts/js-strategic-cycle-reference.mjs" \
		"$native_root/fixtures/strategic-cycle-v1.json" report) \
	<("$native_root/target/debug/mw-tools" strategic-cycle-fixture \
		"$native_root/fixtures/strategic-cycle-v1.json" --json)

runtime_checkpoint="$native_root/fixtures/native-runtime-checkpoint-v1.json"
runtime_output_a=$("$native_root/target/debug/mw-tools" native-runtime-fixture \
	"$modern_path" "$runtime_checkpoint" --json)
runtime_output_b=$("$native_root/target/debug/mw-tools" native-runtime-fixture \
	"$modern_path" "$runtime_checkpoint" --json)

if ! diff -u \
	<(jq --sort-keys . <<<"$runtime_output_a") \
	<(jq --sort-keys . <<<"$runtime_output_b"); then
	printf 'Native runtime fixture is not deterministic\n' >&2
	exit 1
fi

if ! jq -e '
	.schema == "native-runtime-checkpoint-v1"
	and .runtimeSchema == "native-runtime-v2"
	and .checkpointBoundary.kind == "baselineReplay"
	and .checkpointBoundary.resumable == false
	and .requestedSteps == 3
	and .completedSteps == 3
	and (.steps | length) == 3
	and .initial.tick == 598
	and ([.steps[].tick] == [599, 600, 601])
	and ([.initial.state.kind, .steps[].state.kind] | all(. == "running"))
	and (.steps[1].strategic.cycle == 1)
	and (.checksum | type == "string" and length == 16)
' >/dev/null <<<"$runtime_output_a"; then
	printf 'Native runtime fixture contract assertion failed\n' >&2
	exit 1
fi

printf 'native runtime deterministic fixture ok: ticks 598 through 601\n'

save_tmp=$(mktemp -d)
trap 'rm -rf "$save_tmp"' EXIT
native_bin="$native_root/target/debug/mw-native"
"$native_bin" --side Germany,France --side Poland,Belgium --headless --ticks 20 --tick-ms 1 --save-checkpoint "$save_tmp/part.json" "$modern_path" >/dev/null
"$native_bin" --runtime-checkpoint "$save_tmp/part.json" --headless --ticks 20 --tick-ms 1 --save-checkpoint "$save_tmp/resumed.json" "$modern_path" >/dev/null
"$native_bin" --side Germany,France --side Poland,Belgium --headless --ticks 40 --tick-ms 1 --save-checkpoint "$save_tmp/full.json" "$modern_path" >/dev/null
for checkpoint in "$save_tmp/part.json" "$save_tmp/resumed.json" "$save_tmp/full.json"; do
	jq -e '
		.schema == "native-runtime-checkpoint-v4"
		and .sideDynamics.schema == "native-side-dynamics-v1"
		and (.sideDynamics.sides | length) == (.sides | length)
	' "$checkpoint" >/dev/null
done
node "$native_root/scripts/js-browser-v4-wire.mjs" \
	"$web_root" "$save_tmp/part.json" "$save_tmp/browser-wire.json"
"$native_root/target/debug/mw-tools" native-runtime-fixture \
	"$modern_path" "$save_tmp/browser-wire.json" --ticks 1 --json >/dev/null
printf 'browser v4 wire to native loader gate ok\n'
diff -u <(jq -S 'del(.steps)' "$save_tmp/resumed.json") <(jq -S 'del(.steps)' "$save_tmp/full.json")
printf 'native save/reload checkpoint gate ok: Germany+France/Poland+Belgium 20+20 == 40\n'
