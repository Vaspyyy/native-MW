#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import {
	computeCurrentIncome,
	computeRequiredGarrison,
	computeResistanceDelta,
	createEconomyState,
	desertionRate,
	settleEconomyCycle,
} from "../../modern-wars/src/economy.js";
import {
	evaluateCountryCapitulation,
	evaluateGlobalConflict,
} from "../../modern-wars/src/surrender.js";

export const SCHEMA_VERSION = "strategic-cycle-v1";
const bad = (condition, message) => {
	if (condition) throw new Error(message);
};
const number = (value, fallback = 0) =>
	Number.isFinite(Number(value)) ? Number(value) : fallback;
const countryId = (value) => Math.max(0, Math.trunc(number(value)));
const sorted = (values, key) => [...values].sort((a, b) => key(a) - key(b));
const clone = (value) => JSON.parse(JSON.stringify(value));
const canonicalError = (error) =>
	String(error?.message || error).replace(/^strategic cycle: /, "");

function economyFromSeed(seed) {
	bad(countryId(seed.countryId) === 0, "country id must be positive");
	const state = createEconomyState({
		countryId: countryId(seed.countryId),
		gdp: number(seed.gdp),
		pop: number(seed.population),
		territoryUnits: number(seed.territoryUnits),
		initialCoreCells: countryId(seed.initialCoreCells),
		initialCityPop: number(seed.initialCityPop ?? seed.initialCityPopulation),
	});
	return state;
}
function normalizeOccupation(raw) {
	const state = {
		victimId: countryId(raw.victimId),
		annexerId: countryId(raw.annexerId),
		baseIncome: number(raw.baseIncome),
		coreCells: Math.max(1, countryId(raw.coreCells)),
		expectedArmyUnits: number(raw.expectedArmyUnits),
		resistance: number(raw.resistance),
		occupationCoverage: number(raw.occupationCoverage, 1),
		garrisonCoverage: number(raw.garrisonCoverage),
		garrisonAssigned: number(raw.garrisonAssigned),
		requiredGarrison: countryId(raw.requiredGarrison),
		heldRatio: number(raw.heldRatio, 1),
		activeRebellion: raw.activeRebellion === true,
		queuedAtCycle: Math.max(0, countryId(raw.queuedAtCycle)),
		cooldownUntilCycle: Math.max(0, countryId(raw.cooldownUntilCycle)),
	};
	bad(
		!state.victimId || !state.annexerId || state.victimId === state.annexerId,
		"country ids must be positive and distinct",
	);
	return state;
}
function stateFromFixture(fixture) {
	bad(
		fixture?.schemaVersion !== SCHEMA_VERSION,
		`unsupported schemaVersion '${fixture?.schemaVersion}'`,
	);
	const economies = new Map();
	for (const seed of fixture.economySeeds || []) {
		const state = economyFromSeed(seed);
		bad(
			economies.has(state.countryId),
			`duplicate country id ${state.countryId}`,
		);
		economies.set(state.countryId, state);
	}
	const occupations = new Map();
	for (const source of fixture.occupationStates || []) {
		const state = normalizeOccupation(source);
		bad(!economies.has(state.victimId), `unknown country id ${state.victimId}`);
		bad(
			!economies.has(state.annexerId),
			`unknown country id ${state.annexerId}`,
		);
		bad(
			occupations.has(state.victimId),
			`duplicate occupation victim id ${state.victimId}`,
		);
		occupations.set(state.victimId, state);
	}
	return { cycle: 0, economies, occupations, latest: null };
}
function validateCycle(state, input) {
	bad(
		!input.force && number(input.tick) % 600 !== 0,
		"strategic cycle is not due",
	);
	const tick = countryId(input.tick);
	const generation = countryId(input.territoryGeneration);
	const commit = countryId(input.territoryCommitSequence);
	if (state.latest) {
		bad(
			tick <= state.latest.tick,
			`strategic tick ${tick} must be greater than previous tick ${state.latest.tick}`,
		);
		bad(
			generation < state.latest.territoryGeneration,
			`territory generation ${generation} must not be less than previous generation ${state.latest.territoryGeneration}`,
		);
		bad(
			commit < state.latest.territoryCommitSequence,
			`territory commit sequence ${commit} must not be less than previous sequence ${state.latest.territoryCommitSequence}`,
		);
	}
	const countries = new Map();
	for (const country of input.countries || []) {
		const id = countryId(country.countryId);
		bad(
			!id ||
				!state.economies.has(id) ||
				!Number.isFinite(Number(country.payrollDue)) ||
				!Number.isFinite(Number(country.cityPopulationControlled)) ||
				number(country.payrollDue) < 0 ||
				number(country.cityPopulationControlled) < 0,
			!state.economies.has(id)
				? `unknown country id ${id}`
				: "country input contains invalid numeric data",
		);
		bad(countries.has(id), `duplicate country id ${id}`);
		countries.set(id, { ...country, countryId: id });
	}
	const occupations = new Map();
	for (const record of input.occupations || []) {
		const id = countryId(record.victimId);
		bad(
			!id ||
				!state.occupations.has(id) ||
				!Number.isFinite(Number(record.garrisonStrength)) ||
				!Number.isFinite(Number(record.casualtyPressure)) ||
				number(record.garrisonStrength) < 0,
			!state.occupations.has(id)
				? `unknown country id ${id}`
				: "country input contains invalid numeric data",
		);
		bad(occupations.has(id), `duplicate occupation victim id ${id}`);
		occupations.set(id, { ...record, victimId: id });
	}
	return { countries, occupations };
}
function event(
	kind,
	countryId = null,
	relatedCountryId = null,
	previousBand = null,
	nextBand = null,
	value = null,
) {
	return { kind, countryId, relatedCountryId, previousBand, nextBand, value };
}
function cycle(state, input) {
	const { countries, occupations } = validateCycle(state, input);
	const nextEconomies = new Map(
		[...state.economies].map(([id, value]) => [id, clone(value)]),
	);
	const nextOccupations = new Map(
		[...state.occupations].map(([id, value]) => [id, clone(value)]),
	);
	const due = new Map();
	const yields = new Map();
	const prepared = [];
	for (const record of sorted(
		occupations.values(),
		(value) => value.victimId,
	)) {
		const occupation = nextOccupations.get(record.victimId);
		const heldRatio = Math.max(
			0,
			Math.min(1, number(record.heldCells) / Math.max(1, occupation.coreCells)),
		);
		const requiredGarrison = computeRequiredGarrison(
			occupation.expectedArmyUnits,
		);
		const garrisonCoverage = Math.max(
			0,
			Math.min(
				1,
				Math.max(0, number(record.garrisonStrength)) / requiredGarrison,
			),
		);
		const occupationDue = occupation.baseIncome * 0.15 * heldRatio;
		const occupationYield = occupation.baseIncome * 0.25 * heldRatio;
		due.set(
			occupation.annexerId,
			(due.get(occupation.annexerId) || 0) + occupationDue,
		);
		yields.set(
			occupation.annexerId,
			(yields.get(occupation.annexerId) || 0) + occupationYield,
		);
		prepared.push({
			record,
			heldRatio,
			requiredGarrison,
			garrisonCoverage,
			occupationDue,
			occupationYield,
		});
	}
	const events = [];
	for (const country of sorted(
		countries.values(),
		(value) => value.countryId,
	)) {
		const old = nextEconomies.get(country.countryId);
		if (old.capitulated || country.active === false) continue;
		const coreControlRatio =
			number(country.coreControlled) / Math.max(1, old.initialCoreCells);
		const cityControlRatio =
			old.initialCityPop > 0
				? number(country.cityPopulationControlled) / old.initialCityPop
				: coreControlRatio;
		const income = computeCurrentIncome(old.baseIncome, {
			coreControlRatio,
			cityControlRatio,
			capitalHeld: country.capitalHeld !== false,
		});
		const previousBand = old.commandBand;
		const previousCoverage = old.payrollCoverage;
		const settled = settleEconomyCycle(old, {
			income,
			occupationYield: yields.get(country.countryId) || 0,
			payrollDue: number(country.payrollDue),
			occupationDue: due.get(country.countryId) || 0,
		});
		settled.coreControlRatio = Math.max(0, Math.min(1, coreControlRatio));
		settled.cityControlRatio = Math.max(0, Math.min(1, cityControlRatio));
		settled.capitalHeld = country.capitalHeld !== false;
		if (previousCoverage >= 0.999 && settled.payrollCoverage < 0.999)
			events.push(
				event(
					"BUDGET_DEFICIT",
					country.countryId,
					null,
					previousBand,
					settled.commandBand,
					settled.payrollCoverage,
				),
			);
		if (previousBand !== settled.commandBand)
			events.push(
				event(
					"COMMAND_BAND_CHANGED",
					country.countryId,
					null,
					previousBand,
					settled.commandBand,
					settled.arrearsCycles,
				),
			);
		nextEconomies.set(country.countryId, settled);
	}
	const assessments = [];
	for (const item of prepared) {
		const occupation = nextOccupations.get(item.record.victimId);
		const previous = occupation.resistance;
		const coverage =
			nextEconomies.get(occupation.annexerId)?.occupationCoverage || 0;
		const resistanceDelta = computeResistanceDelta({
			occupationCoverage: coverage,
			garrisonCoverage: item.garrisonCoverage,
			casualtyPressure: number(item.record.casualtyPressure),
		});
		occupation.heldRatio = item.heldRatio;
		occupation.requiredGarrison = item.requiredGarrison;
		occupation.garrisonAssigned = Math.max(
			0,
			number(item.record.garrisonStrength),
		);
		occupation.garrisonCoverage = item.garrisonCoverage;
		occupation.occupationCoverage = coverage;
		occupation.resistance = Math.max(
			0,
			Math.min(100, Math.max(0, occupation.resistance) + resistanceDelta),
		);
		if (previous < 75 && occupation.resistance >= 75)
			events.push(
				event(
					"RESISTANCE_WARNING",
					occupation.victimId,
					occupation.annexerId,
					null,
					null,
					occupation.resistance,
				),
			);
		assessments.push({
			victimId: occupation.victimId,
			annexerId: occupation.annexerId,
			heldRatio: item.heldRatio,
			requiredGarrison: item.requiredGarrison,
			garrisonCoverage: item.garrisonCoverage,
			occupationDue: item.occupationDue,
			occupationYield: item.occupationYield,
			resistanceDelta,
			resistance: occupation.resistance,
			rebellionReady:
				occupation.resistance >= 100 && !occupation.activeRebellion,
		});
	}
	const activeSides = new Set((input.activeSides || []).map(countryId));
	const hostileSides = new Set();
	const pairKeys = new Set();
	for (const [leftSource, rightSource] of input.activeHostilePairs || []) {
		const left = countryId(leftSource);
		const right = countryId(rightSource);
		if (left !== right && activeSides.has(left) && activeSides.has(right)) {
			const low = Math.min(left, right);
			const high = Math.max(left, right);
			pairKeys.add(`${low}:${high}`);
			hostileSides.add(low);
			hostileSides.add(high);
		}
	}
	const activeHostilePairs = [...pairKeys]
		.map((value) => value.split(":").map(Number))
		.sort((left, right) => left[0] - right[0] || left[1] - right[1]);
	const decisions = new Map();
	for (const country of sorted(
		countries.values(),
		(value) => value.countryId,
	)) {
		const decision = evaluateCountryCapitulation({
			hasFreshTerritoryData: input.territoryFresh === true,
			isRebel: country.isRebel === true,
			unitCount: countryId(country.unitCount),
			ownedCells: number(country.ownedCells),
			controlledCells: number(country.controlledCells),
			initialCells: number(country.initialCells),
		});
		if (!("threshold" in decision)) decision.threshold = null;
		decisions.set(country.countryId, decision);
	}
	const candidateOrder = [...countries.values()]
		.map((country, index) => ({ country, index }))
		.sort(
			(left, right) =>
				countryId(left.country.side) - countryId(right.country.side) ||
				right.index - left.index,
		);
	const surrenderCandidate = candidateOrder.find(
		({ country }) =>
			country.active !== false &&
			activeSides.has(countryId(country.side)) &&
			hostileSides.has(countryId(country.side)) &&
			!nextEconomies.get(country.countryId).capitulated &&
			decisions.get(country.countryId).capitulate,
	);
	const surrenderCountryId = surrenderCandidate?.country.countryId ?? null;
	const snapshots = [];
	const desertions = [];
	const surrenders = [];
	for (const country of sorted(
		countries.values(),
		(value) => value.countryId,
	)) {
		const economy = nextEconomies.get(country.countryId);
		const capitulation = decisions.get(country.countryId);
		if (country.countryId === surrenderCountryId) {
			economy.capitulated = true;
			surrenders.push({
				countryId: country.countryId,
				side: countryId(country.side),
				decision: capitulation,
			});
			events.push(
				event(
					"CAPITULATION_TRIGGERED",
					country.countryId,
					null,
					null,
					null,
					capitulation.controlPercent,
				),
			);
		}
		const rate = desertionRate(economy.commandBand);
		if (rate > 0 && countryId(country.unitCount) > 0)
			desertions.push({ countryId: country.countryId, rate });
		snapshots.push({
			countryId: country.countryId,
			side: countryId(country.side),
			economy,
			capitulation,
		});
	}
	const conflictResolution =
		surrenderCountryId == null
			? evaluateGlobalConflict(
					[...activeSides].sort((a, b) => a - b),
					activeHostilePairs,
				)
			: null;
	if (conflictResolution) events.push(event("TREATY_RESOLVED"));
	state.cycle++;
	state.economies = nextEconomies;
	state.occupations = nextOccupations;
	const snapshot = {
		schemaVersion: SCHEMA_VERSION,
		cycle: state.cycle,
		tick: countryId(input.tick),
		territoryGeneration: countryId(input.territoryGeneration),
		territoryCommitSequence: countryId(input.territoryCommitSequence),
		countries: snapshots,
		occupations: sorted(nextOccupations.values(), (value) => value.victimId),
		occupationAssessments: assessments,
		desertions,
		surrenders,
		events,
		conflictResolution: conflictResolution
			? {
					kind: conflictResolution.type,
					winnerSide: conflictResolution.winnerSideIdx,
				}
			: null,
	};
	state.latest = snapshot;
	return {
		snapshot,
		counters: {
			countriesProcessed: snapshots.length,
			occupationsProcessed: assessments.length,
			capitulations: surrenders.length,
			desertionCommands: desertions.length,
			events: events.length,
		},
	};
}
function outputEconomy(state) {
	const { initialCityPop, ...rest } = state;
	return { ...rest, initialCityPopulation: initialCityPop };
}
function canonical(value) {
	if (Array.isArray(value)) return value.map(canonical);
	if (!value || typeof value !== "object") return value;
	const result = {};
	for (const [key, item] of Object.entries(value))
		result[key] = canonical(item);
	if (result.economy) result.economy = outputEconomy(result.economy);
	return result;
}
export function runFixture(fixture) {
	const state = stateFromFixture(fixture);
	const cycles = [];
	for (let index = 0; index < (fixture.cycles || []).length; index++) {
		const before = {
			cycle: state.cycle,
			economies: sorted(
				state.economies.values(),
				(value) => value.countryId,
			).map(outputEconomy),
			occupations: sorted(
				state.occupations.values(),
				(value) => value.victimId,
			),
		};
		try {
			cycles.push({
				cycleIndex: index,
				...cycle(state, fixture.cycles[index]),
			});
		} catch (error) {
			const message = canonicalError(error);
			if (
				fixture.cycles[index].expectedError &&
				fixture.cycles[index].expectedError === message
			) {
				cycles.push({
					cycleIndex: index,
					error: message,
					atomic:
						JSON.stringify(before) ===
						JSON.stringify({
							cycle: state.cycle,
							economies: sorted(
								state.economies.values(),
								(value) => value.countryId,
							).map(outputEconomy),
							occupations: sorted(
								state.occupations.values(),
								(value) => value.victimId,
							),
						}),
				});
				continue;
			}
			throw error;
		}
	}
	return canonical({
		schemaVersion: SCHEMA_VERSION,
		cycles,
		final: {
			cycle: state.cycle,
			economies: sorted(
				state.economies.values(),
				(value) => value.countryId,
			).map(outputEconomy),
			occupations: sorted(
				state.occupations.values(),
				(value) => value.victimId,
			),
			latest: state.latest,
		},
	});
}
const FNV64_OFFSET = 14695981039346656037n;
const FNV64_PRIME = 1099511628211n;
const CHECKSUM_SCALE = 1_000_000;
const U64_MASK = (1n << 64n) - 1n;
function checksumU64(hash, value) {
	let current = BigInt.asUintN(64, BigInt(value));
	for (let i = 0; i < 8; i++) {
		hash ^= current & 0xffn;
		hash = (hash * FNV64_PRIME) & U64_MASK;
		current >>= 8n;
	}
	return hash;
}
const checksumBool = (hash, value) => checksumU64(hash, value ? 1 : 0);
function checksumFloat(hash, value) {
	hash = checksumBool(hash, value < 0 || Object.is(value, -0));
	const magnitude = Math.min(
		Number.MAX_SAFE_INTEGER,
		Math.floor(Math.abs(value) * CHECKSUM_SCALE + 0.5),
	);
	return checksumU64(hash, BigInt(magnitude));
}
function commandBandCode(value) {
	return { PAID: 0, STRAINED: 1, UNPAID: 2, BREAKDOWN: 3, MUTINY: 4 }[value];
}
function semanticChecksum(state, stats) {
	let hash = FNV64_OFFSET;
	for (const value of [
		stats.attempted,
		stats.completed,
		stats.expectedErrors,
		stats.countriesProcessed,
		stats.occupationsProcessed,
		stats.capitulations,
		stats.desertionCommands,
		stats.events,
		state.cycle,
		state.economies.size,
	])
		hash = checksumU64(hash, value);
	for (const economy of sorted(
		state.economies.values(),
		(value) => value.countryId,
	)) {
		hash = checksumU64(hash, economy.countryId);
		for (const value of [
			economy.economicStrength,
			economy.baseIncome,
			economy.treasury,
			economy.income,
			economy.occupationYield,
			economy.payrollDue,
			economy.occupationDue,
			economy.payrollCoverage,
			economy.occupationCoverage,
			economy.arrearsCycles,
		])
			hash = checksumFloat(hash, value);
		hash = checksumU64(hash, commandBandCode(economy.commandBand));
		hash = checksumU64(hash, economy.mutinyRecoveryCycles);
		hash = checksumU64(hash, economy.initialCoreCells);
		hash = checksumFloat(hash, economy.initialCityPop);
		hash = checksumFloat(hash, economy.coreControlRatio);
		hash = checksumFloat(hash, economy.cityControlRatio);
		hash = checksumBool(hash, economy.capitalHeld);
		hash = checksumU64(hash, commandBandCode(economy.lastEventBand));
		hash = checksumBool(hash, economy.capitulated);
	}
	hash = checksumU64(hash, state.occupations.size);
	for (const occupation of sorted(
		state.occupations.values(),
		(value) => value.victimId,
	)) {
		hash = checksumU64(hash, occupation.victimId);
		hash = checksumU64(hash, occupation.annexerId);
		hash = checksumFloat(hash, occupation.baseIncome);
		hash = checksumU64(hash, occupation.coreCells);
		hash = checksumFloat(hash, occupation.expectedArmyUnits);
		hash = checksumFloat(hash, occupation.resistance);
		hash = checksumFloat(hash, occupation.occupationCoverage);
		hash = checksumFloat(hash, occupation.garrisonCoverage);
		hash = checksumFloat(hash, occupation.garrisonAssigned);
		hash = checksumU64(hash, occupation.requiredGarrison);
		hash = checksumFloat(hash, occupation.heldRatio);
		hash = checksumBool(hash, occupation.activeRebellion);
		hash = checksumU64(hash, occupation.queuedAtCycle);
		hash = checksumU64(hash, occupation.cooldownUntilCycle);
	}
	if (state.latest) {
		hash = checksumBool(hash, true);
		hash = checksumU64(hash, state.latest.cycle);
		hash = checksumU64(hash, state.latest.tick);
		hash = checksumU64(hash, state.latest.territoryGeneration);
		hash = checksumU64(hash, state.latest.territoryCommitSequence);
		for (const count of [
			state.latest.countries.length,
			state.latest.occupations.length,
			state.latest.occupationAssessments.length,
			state.latest.desertions.length,
			state.latest.surrenders.length,
			state.latest.events.length,
		])
			hash = checksumU64(hash, count);
		if (state.latest.conflictResolution) {
			hash = checksumBool(hash, true);
			hash = checksumU64(
				hash,
				state.latest.conflictResolution.kind === "WHITE_PEACE" ? 0 : 1,
			);
			hash = checksumU64(
				hash,
				state.latest.conflictResolution.winnerSide == null
					? U64_MASK
					: state.latest.conflictResolution.winnerSide,
			);
		} else hash = checksumBool(hash, false);
	} else hash = checksumBool(hash, false);
	return hash.toString(16).padStart(16, "0");
}
function executeBenchmarkCycles(state, inputs) {
	const stats = {
		attempted: 0,
		completed: 0,
		expectedErrors: 0,
		countriesProcessed: 0,
		occupationsProcessed: 0,
		capitulations: 0,
		desertionCommands: 0,
		events: 0,
	};
	for (const input of inputs) {
		stats.attempted++;
		try {
			const result = cycle(state, input);
			if (input.expectedError)
				throw new Error(
					`expected strategic cycle error '${input.expectedError}', but the cycle succeeded`,
				);
			stats.completed++;
			stats.countriesProcessed += result.counters.countriesProcessed;
			stats.occupationsProcessed += result.counters.occupationsProcessed;
			stats.capitulations += result.counters.capitulations;
			stats.desertionCommands += result.counters.desertionCommands;
			stats.events += result.counters.events;
		} catch (error) {
			const message = canonicalError(error);
			if (input.expectedError && input.expectedError === message) {
				stats.expectedErrors++;
				continue;
			}
			throw error;
		}
	}
	return stats;
}
const percentile = (values, p) => {
	const sortedValues = [...values].sort((a, b) => a - b);
	return (
		sortedValues[
			Math.min(sortedValues.length - 1, Math.ceil(sortedValues.length * p) - 1)
		] || 0
	);
};
export function benchmarkFixture(fixture, { repeat = 7, warmup = 2 } = {}) {
	bad(
		!Number.isInteger(repeat) ||
			repeat < 1 ||
			!Number.isInteger(warmup) ||
			warmup < 0,
		"repeat must be positive and warmup must be non-negative",
	);
	const inputs = fixture.cycles || [];
	for (let i = 0; i < warmup; i++) {
		const state = stateFromFixture(fixture);
		executeBenchmarkCycles(state, inputs);
	}
	const samples = [];
	let finalSample = null;
	for (let i = 0; i < repeat; i++) {
		const state = stateFromFixture(fixture);
		const started = performance.now();
		const stats = executeBenchmarkCycles(state, inputs);
		const elapsed = performance.now() - started;
		samples.push(elapsed);
		finalSample = { state, stats };
	}
	return {
		schemaVersion: SCHEMA_VERSION,
		mode: "bench",
		repeat,
		warmup,
		cycles: inputs.length,
		countries: fixture.economySeeds?.length || 0,
		occupations: fixture.occupationStates?.length || 0,
		medianMs: percentile(samples, 0.5),
		p95Ms: percentile(samples, 0.95),
		stats: finalSample.stats,
		checksum: semanticChecksum(finalSample.state, finalSample.stats),
	};
}
async function main() {
	const [file, mode = "report", ...rest] = process.argv.slice(2);
	if (!file)
		throw new Error(
			"usage: js-strategic-cycle-reference.mjs <fixture.json> [report|bench] [--repeat=N --warmup=N]",
		);
	const fixture = JSON.parse(await readFile(file, "utf8"));
	const options = Object.fromEntries(
		rest
			.map((item) => item.match(/^--([^=]+)=(.+)$/))
			.filter(Boolean)
			.map(([, key, value]) => [key, Number(value)]),
	);
	console.log(
		JSON.stringify(
			mode === "bench"
				? benchmarkFixture(fixture, options)
				: runFixture(fixture),
			null,
			2,
		),
	);
}
if (import.meta.url === `file://${process.argv[1]}`)
	main().catch((error) => {
		console.error(error.stack || error.message);
		process.exitCode = 1;
	});
