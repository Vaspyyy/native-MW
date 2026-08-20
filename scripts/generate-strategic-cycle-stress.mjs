#!/usr/bin/env node
const [countriesText = "512", occupationsText = "256", cyclesText = "100"] =
	process.argv.slice(2);
const countries = Number(countriesText);
const occupations = Number(occupationsText);
const cycleCount = Number(cyclesText);
if (
	![countries, occupations, cycleCount].every(Number.isInteger) ||
	countries < 2 ||
	occupations < 0 ||
	cycleCount <= 0
)
	throw new Error(
		"usage: generate-strategic-cycle-stress.mjs [countries=512] [occupations=256] [cycles=100]",
	);
const economySeeds = Array.from({ length: countries }, (_, i) => ({
	countryId: i + 1,
	gdp: 100 + (i % 31) * 17,
	population: 100000 + i * 13,
	territoryUnits: 10 + (i % 7),
	initialCoreCells: 100 + (i % 31),
	initialCityPop: 50000 + i * 97,
}));
const occupationStates = Array.from(
	{ length: Math.min(occupations, countries - 1) },
	(_, i) => ({
		victimId: i + 2,
		annexerId: 1,
		baseIncome: 20 + (i % 13),
		coreCells: 100 + (i % 17),
		expectedArmyUnits: 10 + (i % 30),
		resistance: i % 70,
		occupationCoverage: 1,
		garrisonCoverage: 1,
		garrisonAssigned: 3,
		requiredGarrison: 3,
		heldRatio: 1,
		activeRebellion: false,
		queuedAtCycle: 0,
		cooldownUntilCycle: 0,
	}),
);
const cycles = Array.from({ length: cycleCount }, (_, tick) => ({
	tick: (tick + 1) * 600,
	force: false,
	territoryGeneration: tick + 1,
	territoryCommitSequence: tick + 1,
	territoryFresh: true,
	countries: economySeeds.map((seed, i) => ({
		countryId: seed.countryId,
		side: i % 4,
		ownedCells: seed.initialCoreCells,
		controlledCells: seed.initialCoreCells - ((tick + i) % 3),
		coreControlled: seed.initialCoreCells - ((tick + i) % 3),
		initialCells: seed.initialCoreCells,
		cityPopulationControlled: seed.initialCityPop - ((tick * 17 + i) % 100),
		unitCount: 5 + (i % 9),
		payrollDue: 10 + (i % 19),
		capitalHeld: (tick + i) % 23 !== 0,
		isRebel: false,
		active: true,
	})),
	occupations: occupationStates.map((state, i) => ({
		victimId: state.victimId,
		heldCells: state.coreCells - ((tick + i) % 5),
		garrisonStrength: 2 + (i % 6),
		casualtyPressure: ((tick + i) % 10) / 10,
	})),
	activeSides: [0, 1],
	activeHostilePairs: [[0, 1]],
}));
console.log(
	JSON.stringify({
		schemaVersion: "strategic-cycle-v1",
		economySeeds,
		occupationStates,
		cycles,
	}),
);
