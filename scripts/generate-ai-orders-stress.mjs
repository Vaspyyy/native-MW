#!/usr/bin/env node
// Deterministic production-shaped ai-orders-v1 workload.
const [unitsText = "4800", objectivesText = "32"] = process.argv.slice(2);
const unitCount = Number(unitsText);
const objectiveCount = Number(objectivesText);
if (
	!Number.isSafeInteger(unitCount) ||
	unitCount <= 0 ||
	!Number.isSafeInteger(objectiveCount) ||
	objectiveCount < 4
)
	throw new Error(
		"usage: generate-ai-orders-stress.mjs [units=4800] [objectives=32]",
	);

const maxSides = 4;
const gridWidth = 360;
const gridHeight = 180;
const cells = gridWidth * gridHeight;
const gridRes = 1;
const landMask = new Array(cells);
const dominantSideMap = new Array(cells);
const frontlineLatitude = new Array(cells).fill(0);
const frontlineLongitude = new Array(cells).fill(0);
for (let y = 0; y < gridHeight; y++)
	for (let x = 0; x < gridWidth; x++) {
		const index = y * gridWidth + x;
		const water = y < 8 || y > 171 || (x * 17 + y * 13) % 97 === 0;
		landMask[index] = water ? 0 : 1;
		dominantSideMap[index] = water ? -1 : Math.floor(x / 90);
		if (!water && x % 90 === 44) {
			frontlineLatitude[index] = ((y % 5) - 2) * 0.1;
			frontlineLongitude[index] = 1;
		}
	}
const hostilityMatrix = [0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0, 0];
const objectives = [];
for (let index = 0; index < objectiveCount; index++) {
	const side = index % maxSides;
	const slot = Math.floor(index / maxSides);
	objectives.push({
		id: 1000 + index,
		sidePair: [side, (side + 1) % maxSides],
		segmentId: slot,
		lat: -70 + ((slot * 19 + side * 7) % 140),
		lng: -157.5 + side * 90 + (slot % 3) * 2,
		capacity: Math.ceil(unitCount / objectiveCount) + 16,
		priority: 20 - (slot % 7),
	});
}
const movement = {
	terrainSpeedMultiplier: 1,
	speedMultiplier: 1,
	planSpeedMultiplier: 1,
	neutralPenalty: 1,
	pushReadiness: 1,
};
const combat = {
	dealtMultiplier: 1,
	takenMultiplier: 1,
	defenseBonus: 1,
	longWarDefense: 1,
	mountain: false,
	urban: false,
};
const units = [];
for (let index = 0; index < unitCount; index++) {
	const side = index % maxSides;
	const latticeX = (index * 73 + 19) % 360;
	const latticeY = 10 + ((index * 37 + 11) % 160);
	let lat = latticeY - 89.5;
	let lng = latticeX - 179.5;
	const cluster = Math.floor(index / 100);
	const clusterOffset = index % 100;
	if (clusterOffset < 4) {
		lat = -70 + ((cluster * 17) % 140) + clusterOffset * 0.08;
		lng = -170 + ((cluster * 29) % 340) + clusterOffset * 0.08;
	}
	const eligible = index % 10 === 0;
	const health = eligible ? 35 + (index % 10) : 70 + (index % 31);
	const ownedObjectives = objectives.filter(
		(objective) => objective.sidePair[0] === side,
	);
	const prior =
		index % 3 === 0 && ownedObjectives.length
			? { objectiveId: ownedObjectives[index % ownedObjectives.length].id }
			: null;
	units.push({
		id: index + 1,
		side,
		sovereign: side + 1,
		kind: index % 7 === 0 ? "armor" : "army",
		lat,
		lng,
		health,
		maxHealth: 100,
		combatPower: index % 7 === 0 ? 1 : 0.75 + (index % 5) * 0.25,
		allyWeight: index % 13 === 0 ? 1.4 : 1,
		atSea: false,
		transport: false,
		baseSpeed: index % 7 === 0 ? 0.0033 : 0.003,
		movement: { ...movement, terrainSpeedMultiplier: 0.8 + (index % 5) * 0.05 },
		combat: {
			...combat,
			dealtMultiplier: 0.9 + (index % 3) * 0.1,
			mountain: index % 29 === 0,
		},
		previousAssignment: prior,
		isReserve: index % 37 === 0,
		reinforcementEligible: eligible,
		encircled: index % 211 === 0,
	});
}
const config = {
	contactScanRadius: 0.6,
	retreatMinHostilePower: 5,
	retreatMultiple: 8,
	retreatBoost: 5.5,
	encircledRetreatMultiplier: 0.25,
	priorAssignmentStickiness: 8,
	reinforcementReadinessThreshold: 0.45,
	contactPlanSpeedMultiplier: 1,
	frontPlanSpeedMultiplier: 1,
	reinforcementPlanSpeedMultiplier: 0.75,
	fieldPlanSpeedMultiplier: 1,
	maxUnits: 100000,
	maxObjectives: 10000,
	maxGridCells: 5000000,
	maxAssignmentEdges: 5000000,
};
const fixture = {
	schema: "ai-orders-v1",
	cases: [
		{
			name: "stress",
			config,
			world: {
				gridWidth,
				gridHeight,
				gridRes,
				landMask,
				dominantSideMap,
				maxSides,
				hostilityMatrix,
				frontlineLatitude,
				frontlineLongitude,
				objectives,
			},
			units,
			verifyPermutationInvariance: false,
			expectedError: null,
		},
	],
};
process.stdout.write(`${JSON.stringify(fixture)}\n`);
