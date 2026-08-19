import { readFileSync } from "node:fs";
import { performance } from "node:perf_hooks";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [mode, webRoot, fixturePath, repeatText = "100", warmupText = "20"] =
	process.argv.slice(2);
const repeat = Number(repeatText);
const warmup = Number(warmupText);
if (
	!(mode === "report" || mode === "bench") ||
	!webRoot ||
	!fixturePath ||
	!Number.isSafeInteger(repeat) ||
	repeat < 1 ||
	!Number.isSafeInteger(warmup) ||
	warmup < 0
) {
	throw new Error(
		"usage: node scripts/js-tactical-reference.mjs <report|bench> <web-root> <fixture> [repeat] [warmup]",
	);
}

const tacticalUrl = pathToFileURL(resolve(webRoot, "src/tactical-grid.js"));
const {
	createTacticalGrid,
	forEachNeighborCell,
	forEachUnorderedNeighborPair,
	rebuildTacticalGrid,
} = await import(tacticalUrl.href);
const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));
const units = fixture.units.map((unit) => ({ ...unit }));
const accessors = {
	getSide: (unit) => unit.side,
	getLat: (unit) => unit.lat,
	getLng: (unit) => unit.lng,
	getStrength: (unit) => unit.strength,
	getAllyWeight: (unit) => unit.allyWeight,
	isArmor: (unit) => unit.armor,
	isSupport: (unit) => unit.support,
};
const grid = createTacticalGrid({ cellSize: fixture.cellSize, ...accessors });

function counters(value) {
	return {
		input_units: value.inputUnits,
		inserted_units: value.insertedUnits,
		skipped_units: value.skippedUnits,
		side_count: value.sideCount,
		cell_count: value.cellCount,
		max_bucket_occupancy: value.maxBucketOccupancy,
		candidate_pairs: value.candidatePairs,
		accepted_pairs: value.acceptedPairs,
	};
}

function execute() {
	rebuildTacticalGrid(grid, units);
	const initialCounters = counters(grid.counters);
	const sides = [...grid.bySide.entries()]
		.map(([side, cells]) => ({
			side: Number(side),
			cells: [...cells.values()]
				.sort((left, right) => left.key - right.key)
				.map((cell) => ({
					key: cell.key,
					x: cell.x,
					y: cell.y,
					side: Number(cell.sideKey),
					count: cell.count,
					total_strength: cell.totalStrength,
					total_ally_weight: cell.totalAllyWeight,
					weighted_strength: cell.weightedStrength,
					centroid_lat: cell.centroidLat,
					centroid_lng: cell.centroidLng,
					armor_count: cell.armorCount,
					support_count: cell.supportCount,
					has_armor: cell.hasArmor,
					has_support: cell.hasSupport,
					unit_ids: cell.units.map((unit) => unit.id),
				})),
		}))
		.sort((left, right) => left.side - right.side);
	const neighbors = (fixture.neighborQueries || []).map((query) => {
		const keys = [];
		forEachNeighborCell(
			grid,
			query.side,
			{ lat: query.lat, lng: query.lng },
			(cell) => keys.push(cell.key),
			{ radiusCells: query.radiusCells },
		);
		return { side: query.side, keys };
	});
	const pairQueries = (fixture.pairQueries || []).map((query) => {
		const visits = [];
		const options = {
			radiusCells: query.radiusCells,
			radiusSq: query.radiusSq == null ? undefined : query.radiusSq,
		};
		if (query.rejectIdSumModulo != null) {
			options.acceptPair = (left, right) =>
				(left.id + right.id) % query.rejectIdSumModulo !== 0;
		}
		const stats = forEachUnorderedNeighborPair(
			grid,
			query.side,
			(left, right, distanceSq, leftCell, rightCell) => {
				visits.push({
					left_id: left.id,
					right_id: right.id,
					distance_sq: distanceSq,
					left_key: leftCell.key,
					right_key: rightCell.key,
				});
			},
			options,
		);
		return {
			side: query.side,
			candidate_pairs: stats.candidatePairs,
			accepted_pairs: stats.acceptedPairs,
			visits,
		};
	});
	return {
		schema_version: grid.schemaVersion,
		dimensions: {
			cell_size: grid.cellSize,
			columns: grid.columns,
			rows: grid.rows,
		},
		initial_counters: initialCounters,
		sides,
		neighbors,
		pair_queries: pairQueries,
		cumulative_pair_counters: {
			candidate_pairs: grid.counters.candidatePairs,
			accepted_pairs: grid.counters.acceptedPairs,
		},
	};
}

const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const FNV_MASK = (1n << 64n) - 1n;
function visitHash(visits) {
	let hash = FNV_OFFSET;
	const bytes = new Uint8Array(8);
	const view = new DataView(bytes.buffer);
	const addU64 = (value) => {
		view.setBigUint64(0, BigInt(value), true);
		for (const byte of bytes) hash = ((hash ^ BigInt(byte)) * FNV_PRIME) & FNV_MASK;
	};
	const addF32 = (value) => {
		view.setFloat32(0, value, true);
		for (let index = 0; index < 4; index++) {
			hash = ((hash ^ BigInt(bytes[index])) * FNV_PRIME) & FNV_MASK;
		}
	};
	for (const visit of visits) {
		addU64(visit.left_id);
		addU64(visit.right_id);
		addF32(visit.distance_sq);
		addU64(visit.left_key);
		addU64(visit.right_key);
	}
	return hash.toString(16).padStart(16, "0");
}

function runBenchPairQueries(stable) {
	let candidatePairs = 0;
	let acceptedPairs = 0;
	let checksum = 0;
	const visits = stable ? [] : null;
	for (const query of fixture.pairQueries || []) {
		const options = {
			radiusCells: query.radiusCells,
			radiusSq: query.radiusSq == null ? undefined : query.radiusSq,
		};
		if (query.rejectIdSumModulo != null) {
			options.acceptPair = (left, right) =>
				(left.id + right.id) % query.rejectIdSumModulo !== 0;
		}
		const stats = forEachUnorderedNeighborPair(
			grid,
			query.side,
			(left, right, distanceSq, leftCell, rightCell) => {
				if (stable) {
					visits.push({
						left_id: left.id,
						right_id: right.id,
						distance_sq: distanceSq,
						left_key: leftCell.key,
						right_key: rightCell.key,
					});
				} else {
					checksum = (
						checksum +
						Math.imul(left.id >>> 0, 0x9e3779b1) +
						Math.imul(right.id >>> 0, 0x85ebca6b) +
						(leftCell.key >>> 0) +
						Math.imul(rightCell.key >>> 0, 31) +
						(Math.trunc(distanceSq * 0x100000) >>> 0)
					) >>> 0;
				}
			},
			options,
		);
		candidatePairs += stats.candidatePairs;
		acceptedPairs += stats.acceptedPairs;
	}
	return { candidatePairs, acceptedPairs, checksum, visits };
}

function sampleSummary(samples) {
	const sorted = [...samples].sort((left, right) => left - right);
	return {
		median: sorted[Math.floor(sorted.length / 2)],
		p95: sorted[Math.ceil(0.95 * sorted.length) - 1],
	};
}

if (mode === "report") {
	console.log(JSON.stringify(execute()));
} else {
	let consumedChecksum = 0;
	for (let iteration = 0; iteration < warmup; iteration++) {
		rebuildTacticalGrid(grid, units);
		consumedChecksum ^= runBenchPairQueries(false).checksum;
	}
	const rebuildSamples = [];
	const pairSamples = [];
	for (let iteration = 0; iteration < repeat; iteration++) {
		const started = performance.now();
		rebuildTacticalGrid(grid, units);
		rebuildSamples.push(performance.now() - started);

		const pairsStarted = performance.now();
		const result = runBenchPairQueries(false);
		pairSamples.push(performance.now() - pairsStarted);
		consumedChecksum ^= result.checksum;
	}
	if (consumedChecksum === -1) process.stderr.write("");

	rebuildTacticalGrid(grid, units);
	const verification = runBenchPairQueries(true);
	console.log(
		JSON.stringify({
			input: fixturePath,
			repeat,
			warmup,
			dimensions: {
				cell_size: grid.cellSize,
				columns: grid.columns,
				rows: grid.rows,
			},
			counters: counters(grid.counters),
			rebuild_ms: sampleSummary(rebuildSamples),
			pairs_ms: sampleSummary(pairSamples),
			candidate_pairs: verification.candidatePairs,
			accepted_pairs: verification.acceptedPairs,
			visit_hash: visitHash(verification.visits),
		}),
	);
}
