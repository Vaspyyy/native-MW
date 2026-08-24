#!/usr/bin/env node

const unitsPerSide = Number(process.argv[2] || 2400);
if (!Number.isSafeInteger(unitsPerSide) || unitsPerSide <= 0) {
	throw new RangeError("units per side must be a positive safe integer");
}

const CELL_SIZE = 0.6;
const PAIR_STRIDE_CELLS = 2;
const COLUMNS = 80;
const ROWS = Math.ceil(unitsPerSide / COLUMNS);
if (ROWS * PAIR_STRIDE_CELLS + 20 >= 300) {
	throw new RangeError("requested stress fixture does not fit the tactical grid");
}

const units = [];
const orders = [];
const mask = Array(360 * 180).fill(1);

function cellCenter(x, y) {
	return {
		lat: -90 + (y + 0.5) * CELL_SIZE,
		lng: -180 + (x + 0.5) * CELL_SIZE,
	};
}

function addUnit(side, pairIndex, lat, lng, preferredTargetId) {
	const id = side * unitsPerSide + pairIndex;
	const armor = pairIndex % 17 === 0;
	units.push({
		id,
		side,
		sovereign: side + 1,
		kind: armor ? "armor" : "army",
		lat,
		lng,
		health: 100,
		maxHealth: 100,
		personnel: armor ? 0 : 1000,
		personnelCapacity: armor ? 0 : 1000,
		equipment: armor ? 100 : 0,
		maxEquipment: armor ? 100 : 0,
		quality: 40 + (pairIndex % 60),
		isTransport: false,
		isAtSea: false,
		armorSupported: pairIndex % 11 === 0,
		armorLandingPenaltyUntilTick: 0,
		lastCombatTick: 0,
		victoryBoostTicks: 0,
		dirLat: 0,
		dirLng: 0,
		coastStuckTicks: 0,
		isSupport: pairIndex % 13 === 0,
		allyWeight: 1,
	});
	orders.push({
		unitId: id,
		preferredTargetId,
		movementEnabled: true,
		dirLat: 0,
		dirLng: side === 0 ? 0.2 : -0.2,
		factors: {
			baseSpeed: 0.003,
			speedMult: 1,
			planSpeedMult: 1,
			neutralPenalty: 1,
			retreatBoost: 1,
			pushReadiness: 1,
		},
		combat: {
			damageDealtMult: 1,
			damageTakenMult: 1,
			defenseBonus: 1,
			longWarDefense: 1,
		},
	});
}

for (let pairIndex = 0; pairIndex < unitsPerSide; pairIndex++) {
	const column = pairIndex % COLUMNS;
	const row = Math.floor(pairIndex / COLUMNS);
	const base = cellCenter(
		20 + column * PAIR_STRIDE_CELLS,
		20 + row * PAIR_STRIDE_CELLS,
	);
	const side0Id = pairIndex;
	const side1Id = unitsPerSide + pairIndex;

	if (pairIndex % 3 === 0) {
		// Side 1's preferred target cancels its deterministic direct-combat
		// jitter. Both points remain well inside the same tactical cell.
		const jitterLat = Math.sin(side1Id * 100) * 0.08;
		const jitterLng = Math.cos(side1Id * 100) * 0.08;
		addUnit(0, pairIndex, base.lat - jitterLat, base.lng - jitterLng, side1Id);
		addUnit(1, pairIndex, base.lat, base.lng, side0Id);
	} else if (pairIndex % 3 === 1) {
		// 0.18 degrees is safely inside proximity range but always outside
		// direct range, including the direct kernel's 0.08-degree jitter.
		addUnit(0, pairIndex, base.lat, base.lng - 0.09, side1Id);
		addUnit(1, pairIndex, base.lat, base.lng + 0.09, side0Id);
	} else {
		// Adjacent cell centers are outside contact range, guaranteeing the
		// movement-only path without placing points on a cell boundary.
		addUnit(0, pairIndex, base.lat, base.lng, side1Id);
		addUnit(1, pairIndex, base.lat, base.lng + CELL_SIZE, side0Id);
	}
}

console.log(
	JSON.stringify({
		schema: "native-tick-v2",
		config: { tacticalCellSize: CELL_SIZE },
		grid: { gridRes: 1, width: 360, height: 180, landMask: mask },
		maxSides: 2,
		tick: 700,
		frame: 700,
		warGraceEnd: 600,
		units,
		orders,
		steps: 1,
	}),
);
