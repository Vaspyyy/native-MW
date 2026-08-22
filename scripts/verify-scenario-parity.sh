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
cargo build --quiet --release --manifest-path "$native_root/Cargo.toml" -p mw-native
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-core committed_state_restore
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools checkpoint_v2
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools checkpoint_v7
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools checkpoint_v8
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools checkpoint_v9
cargo test --quiet --manifest-path "$native_root/Cargo.toml" -p mw-tools checkpoint_v10

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
naval_bin="$native_root/target/release/mw-native"
"$native_bin" --side Germany,France --side Poland,Belgium --headless --ticks 20 --tick-ms 1 --save-checkpoint "$save_tmp/part.json" "$modern_path" >/dev/null
"$native_bin" --runtime-checkpoint "$save_tmp/part.json" --headless --ticks 20 --tick-ms 1 --save-checkpoint "$save_tmp/resumed.json" "$modern_path" >/dev/null
"$native_bin" --side Germany,France --side Poland,Belgium --headless --ticks 40 --tick-ms 1 --save-checkpoint "$save_tmp/full.json" "$modern_path" >/dev/null
for checkpoint in "$save_tmp/part.json" "$save_tmp/resumed.json" "$save_tmp/full.json"; do
	jq -e '
		.schema == "native-runtime-checkpoint-v10"
		and .gameplayRng.schema == "native-gameplay-rng-v1"
		and .gameplayRng.algorithm == "mulberry32"
		and (.gameplayRng.state | type == "number" and . >= 0 and . <= 4294967295)
		and (.personnelReserves | length) == (.sides | length)
		and ([.personnelReserves[]] | all(type == "number" and . >= 0))
		and .sideDynamics.schema == "native-side-dynamics-v1"
		and .operationalAi.schema == "native-operational-ai-v1"
		and .operationalExecution.schema == "native-operational-execution-v1"
		and .airPower.schema == "native-air-v2"
		and .navalPlanning.schema == "native-naval-planning-v1"
		and .reinforcement.schema == "native-reinforcement-v1"
		and (.reinforcement.countries | length) == (.economies | length)
		and (.navalPlanning.sideStates | length) == (.sides | length)
		and ([.navalPlanning.sideStates[].side] ==
			([.sides[].sideIndex] | sort))
		and (.navalPlanning.nextOperationSequence | type == "number" and . >= 1)
		and (.airPower.countryCoverage | length) == (.economies | length)
		and ([.airPower.countryCoverage[].countryId] ==
			([.economies[].countryId] | sort))
		and ([.airPower.countryCoverage[].operationsCoverage]
			| all(type == "number" and . >= 0 and . <= 1))
		and (.sideDynamics.sides | length) == (.sides | length)
	' "$checkpoint" >/dev/null
done
jq '.schema = "native-runtime-checkpoint-v5"
	| del(.operationalExecution, .airPower, .navalPlanning, .gameplayRng, .personnelReserves, .reinforcement)
	| del(.battlefield.units[].supplyCollapsedTick)' \
	"$save_tmp/part.json" >"$save_tmp/part-v5.json"
node "$native_root/scripts/js-browser-v5-wire.mjs" \
	"$web_root" "$save_tmp/part-v5.json" "$save_tmp/browser-v5-wire.json"
"$native_root/target/debug/mw-tools" native-runtime-fixture \
	"$modern_path" "$save_tmp/browser-v5-wire.json" --ticks 1 --json >/dev/null
printf 'browser v5 operationalAi wire to native loader gate ok\n'
jq '.schema = "native-runtime-checkpoint-v6"
	| del(.navalPlanning, .gameplayRng, .personnelReserves, .reinforcement)
	| del(.battlefield.units[].supplyCollapsedTick)' \
	"$save_tmp/part.json" >"$save_tmp/part-v6.json"
"$native_bin" --runtime-checkpoint "$save_tmp/part-v6.json" --headless --ticks 1 --tick-ms 1 \
	--save-checkpoint "$save_tmp/resaved-v6.json" "$modern_path" >/dev/null
jq -e '
	.schema == "native-runtime-checkpoint-v6"
	and (has("navalPlanning") | not)
	and .operationalExecution.schema == "native-operational-execution-v1"
	and .airPower.schema == "native-air-v2"
' "$save_tmp/resaved-v6.json" >/dev/null
printf 'native v6 execution and air-power fallback save gate ok\n'
node "$native_root/scripts/js-browser-v6-wire.mjs" \
	"$web_root" "$save_tmp/part-v6.json" "$save_tmp/browser-v6-wire.json"
jq -e '
	.schema == "native-runtime-checkpoint-v6"
	and .operationalExecution.schema == "native-operational-execution-v1"
	and ([.operationalExecution.navalOperations[].kind]
		| sort == (["INVASION", "SUPPLY", "FAST_TRANSPORT"] | sort))
	and (.operationalExecution.defenderReactions | length) == 1
	and .airPower.schema == "native-air-v2"
	and (.airPower.countryCoverage | length) == (.economies | length)
	and ([.airPower.countryCoverage[].operationsCoverage] | any(. < 1))
	and (.airPower.airfields | length) > 0
	and (.airPower.wings | length) > 0
' "$save_tmp/browser-v6-wire.json" >/dev/null
"$native_root/target/debug/mw-tools" native-runtime-fixture \
	"$modern_path" "$save_tmp/browser-v6-wire.json" --ticks 1 --json >/dev/null
printf 'browser v6 execution and air-power wire to native loader gate ok\n'
diff -u <(jq -S 'del(.steps)' "$save_tmp/resumed.json") <(jq -S 'del(.steps)' "$save_tmp/full.json")
printf 'native v10 save/reload checkpoint gate ok: Germany+France/Poland+Belgium 20+20 == 40\n'

"$naval_bin" --side "United Kingdom" --side Iceland --headless --ticks 1000 --tick-ms 1 \
	--save-checkpoint "$save_tmp/naval-part.json" "$modern_path" >/dev/null
jq -e '
	.schema == "native-runtime-checkpoint-v10"
	and .gameplayRng.schema == "native-gameplay-rng-v1"
	and (.personnelReserves | length) == (.sides | length)
	and .navalPlanning.schema == "native-naval-planning-v1"
	and .reinforcement.schema == "native-reinforcement-v1"
	and .navalPlanning.nextOperationSequence > 1
	and (.operationalExecution.navalOperations | length) >= 1
	and ([.operationalExecution.navalOperations[]
		| select(.kind == "INVASION")
		| (.route | length) > 0] | any)
	and ([.operationalExecution.navalOperations[].phase]
		| any(. == "TRANSIT" or . == "LANDING" or . == "DELIVERED" or . == "COMPLETE"))
	and (.operationalExecution.defenderReactions | length) >= 1
' "$save_tmp/naval-part.json" >/dev/null
"$naval_bin" --runtime-checkpoint "$save_tmp/naval-part.json" --headless --ticks 100 \
	--tick-ms 1 --save-checkpoint "$save_tmp/naval-resumed.json" "$modern_path" >/dev/null
"$naval_bin" --side "United Kingdom" --side Iceland --headless --ticks 1100 --tick-ms 1 \
	--save-checkpoint "$save_tmp/naval-full.json" "$modern_path" >/dev/null
diff -u \
	<(jq -S 'del(.steps)' "$save_tmp/naval-resumed.json") \
	<(jq -S 'del(.steps)' "$save_tmp/naval-full.json")
printf 'native naval origination gate ok: routed invasions, defender reaction, and 1000+100 == 1100\n'
