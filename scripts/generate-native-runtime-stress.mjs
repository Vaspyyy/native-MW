#!/usr/bin/env node

// Deterministic, production-shaped native-runtime-checkpoint-v1 workload.
// The scenario path is supplied separately to mw-tools; this checkpoint is
// pinned to the repository's canonical Modern 2022 scenario bytes.

const [unitsText = "2400", stepsText = "3"] = process.argv.slice(2);
const unitsPerSide = Number(unitsText);
const steps = Number(stepsText);
if (
	!Number.isSafeInteger(unitsPerSide) ||
	unitsPerSide <= 0 ||
	!Number.isSafeInteger(steps) ||
	steps <= 0
) {
	throw new Error(
		"usage: generate-native-runtime-stress.mjs [units-per-side=2400] [steps=3]",
	);
}

const aiPolicy = Object.freeze({
	baseSpeed: 0.003,
	terrainSpeedMultiplier: 1,
	speedMultiplier: 1,
	planSpeedMultiplier: 1,
	neutralPenalty: 1,
	pushReadiness: 1,
	dealtMultiplier: 1,
	takenMultiplier: 1,
	defenseBonus: 1,
	longWarDefense: 1,
	mountain: false,
	urban: false,
	isReserve: false,
	reinforcementEligible: false,
	encircled: false,
	deployUntilTick: 0,
	garrisonExcluded: false,
});

function influencePolicy(countryId) {
	return {
		radius: 0.45,
		delta: 0.04,
		concentrationBonus: 1,
		beneficiaryCountryId: countryId,
		protectedOwnerIds: [],
		rebelDeJure: null,
		creditDeJure: null,
		creditDeJureByCountry: {},
		refusesOffense: false,
	};
}

function startingEconomy({
	countryId,
	gdp,
	initialCoreCells,
	initialCityPopulation,
	treasury,
}) {
	const economicStrength = Math.sqrt(gdp) * 2.5;
	const baseIncome = Math.max(economicStrength / 0.7, 3);
	return {
		countryId,
		economicStrength,
		baseIncome,
		treasury: treasury ?? baseIncome * 6,
		income: baseIncome,
		occupationYield: 0,
		payrollDue: 0,
		occupationDue: 0,
		payrollCoverage: 1,
		occupationCoverage: 1,
		arrearsCycles: 0,
		commandBand: "PAID",
		mutinyRecoveryCycles: 0,
		initialCoreCells,
		initialCityPopulation,
		coreControlRatio: 1,
		cityControlRatio: 1,
		capitalHeld: true,
		lastEventBand: "PAID",
		capitulated: false,
	};
}

function unit(side, index) {
	const countryId = side === 0 ? 136 : 31;
	const armor = index % 17 === 0;
	// Spread the cap-scale armies along the real eastern Russia-China front.
	// Parallel rows retain substantial contact/influence work without an
	// artificial all-in-one-cell quadratic pileup.
	const columns = 160;
	const column = index % columns;
	const row = Math.floor(index / columns);
	const along = column / (columns - 1);
	const lateral = (row - 7) * 0.35 + (side === 0 ? 0.1 : -0.1);
	const lat = 42 + along * 11 + lateral * 0.74;
	const lng = 131 - along * 12 + lateral * 0.67;
	return {
		id: countryId * 1_000_000 + index + 1,
		side,
		countryId,
		kind: armor ? "armor" : "army",
		lat,
		lng,
		health: 100,
		maxHealth: 100,
		personnel: armor ? 200 : 1000,
		personnelCapacity: armor ? 200 : 1000,
		equipment: armor ? 100 : 0,
		maxEquipment: armor ? 100 : 0,
		quality: 45 + (index % 46),
		transport: false,
		armorSupported: armor && index % 3 !== 0,
		landingPenaltyActive: false,
		atSea: false,
		lastCombatTick: 0,
		victoryBoostTicks: 0,
		dirLat: 0,
		dirLng: 0,
		coastStuckTicks: 0,
		armorLandingPenaltyUntilTick: 0,
		isSupport: index % 29 === 0,
		allyWeight: 1,
		aiPolicy: {
			...aiPolicy,
			isReserve: index % 53 === 0,
			reinforcementEligible: index % 11 === 0,
		},
		influencePolicy: influencePolicy(countryId),
	};
}

const units = [];
for (let index = 0; index < unitsPerSide; index++) {
	units.push(unit(0, index), unit(1, index));
}

const checkpoint = {
	schema: "native-runtime-checkpoint-v1",
	checkpointBoundary: "baselineReplay",
	scenario: {
		sha256: "e360e86fbcc5decb4a90e04b6d25369f8e0fb2b07a09fdb2258f7e90fd3de8fc",
		name: "My Custom Scenario",
		gridRes: 0.15,
	},
	sides: [
		{ sideIndex: 0, countryIds: [136] },
		{ sideIndex: 1, countryIds: [31] },
	],
	activeSides: [0, 1],
	hostilityMatrix: [0, 1, 1, 0],
	tick: 598,
	frame: 1000,
	warGraceEnd: 0,
	strategicCycle: 0,
	steps,
	units,
	economies: [
		startingEconomy({
			countryId: 31,
			gdp: 18_316.8,
			initialCoreCells: 42_583,
			initialCityPopulation: 214_253_161,
			// Deliberate benchmark reserve: repeated cap-scale pay cycles
			// should not enter the explicit strategic-effects gate.
			treasury: 1_000_000,
		}),
		startingEconomy({
			countryId: 136,
			gdp: 2291.6,
			initialCoreCells: 132_293,
			initialCityPopulation: 47_356_838,
			treasury: 1_000_000,
		}),
	],
	occupations: [],
	casualties: { 31: 0, 136: 0 },
};

process.stdout.write(`${JSON.stringify(checkpoint)}\n`);
