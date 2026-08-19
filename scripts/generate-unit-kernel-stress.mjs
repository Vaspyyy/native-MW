#!/usr/bin/env node

const DEFAULT_UNITS_PER_SIDE = 2400;
const SEED = 0x4d575031;
const COMBAT_CASE_COUNT = 8;

function parseUnitsPerSide(value) {
	if (value === undefined) return DEFAULT_UNITS_PER_SIDE;
	const parsed = Number(value);
	if (!Number.isSafeInteger(parsed) || parsed <= 0) {
		throw new Error("units-per-side must be a positive integer");
	}
	return parsed;
}

function createRandom(seed) {
	let state = seed >>> 0;
	return () => {
		state ^= state << 13;
		state ^= state >>> 17;
		state ^= state << 5;
		return (state >>> 0) / 0x1_0000_0000;
	};
}

function wrapLongitude(longitude) {
	let wrapped = longitude;
	if (wrapped > 180) wrapped -= 360;
	if (wrapped < -180) wrapped += 360;
	return wrapped;
}

function movementCases(count, random) {
	const allLand = [1, 1, 1, 1, 1, 1, 1, 1];
	const allWater = [0, 0, 0, 0, 0, 0, 0, 0];
	const cases = [];
	for (let index = 0; index < count; index++) {
		const transport = index % 7 === 0;
		const coastBlocked = !transport && index % 11 === 0;
		const angle = random() * Math.PI * 2;
		cases.push({
			name: `stress movement ${index + 1}`,
			grid: {
				gridRes: 90,
				width: 4,
				height: 2,
				landMask: coastBlocked || transport ? allWater : allLand,
			},
			state: {
				lat: -75 + random() * 150,
				lng: -179.5 + random() * 359,
				dirLat: Math.sin(angle),
				dirLng: Math.cos(angle),
				isTransport: transport,
				isAtSea: transport && index % 14 === 0,
				coastStuckTicks: coastBlocked ? index % 61 : index % 5,
			},
			factors: {
				baseSpeed: transport ? 0.025 : 0.003,
				speedMult: 0.7 + random() * 1.1,
				planSpeedMult: 0.6 + random() * 0.7,
				neutralPenalty: 0.55 + random() * 0.45,
				retreatBoost: 0.8 + random() * 1.2,
				pushReadiness: 0.3 + random() * 0.7,
			},
		});
	}
	return cases;
}

function makeUnit(id, side, kind, lat, lng, random, flags = {}) {
	const armor = kind === "armor";
	const health = 55 + random() * 45;
	const personnel = armor ? 0 : 500 + Math.floor(random() * 1001);
	const equipment = armor ? 30 + Math.floor(random() * 91) : 0;
	return {
		id,
		sideIndex: side,
		sovereignId: side + 1,
		kind,
		lat,
		lng,
		health,
		maxHealth: 100,
		personnel,
		personnelCapacity: armor ? 0 : Math.max(1000, personnel),
		equipment,
		maxEquipment: equipment,
		quality: 20 + random() * 80,
		isTransport: !!flags.transport,
		isAtSea: !!flags.atSea,
		armorSupported: armor && !!flags.supported,
		armorLandingPenaltyUntilTick: armor && flags.landingPenalty ? 5000 : 0,
		lastCombatTick: 0,
		victoryBoostTicks: 0,
	};
}

function combatContext(caseIndex) {
	return {
		simTick: 1200 + caseIndex * 1000,
		simFrame: 1200 + caseIndex * 1000,
		warGraceEnd: 600,
		damageDealtMult: 0.75 + caseIndex * 0.1,
		damageTakenMult: 0.8 + (caseIndex % 4) * 0.12,
		defenseBonus: [1, 0.85, 0.65, 0.45][caseIndex % 4],
		longWarDefense: caseIndex >= 6 ? 0.75 : 1,
		mountain: caseIndex % 4 === 2,
		urban: caseIndex % 4 === 3,
	};
}

function combatCases(pairCount, random) {
	const cases = Array.from({ length: Math.min(COMBAT_CASE_COUNT, pairCount) }, (_, index) => ({
		name: `stress combat ${index + 1}`,
		context: combatContext(index),
		units: [],
		operations: [],
	}));
	let nextId = 1;
	for (let pair = 0; pair < pairCount; pair++) {
		const testCase = cases[pair % cases.length];
		const caseIndex = pair % cases.length;
		const layer = pair % 2 === 0 ? "direct" : "proximity";
		const attackerId = nextId++;
		const targetId = nextId++;
		const targetLat = -70 + random() * 140;
		const targetLng = -179 + random() * 358;
		let attackerLat;
		let attackerLng;
		if (layer === "direct") {
			const phase = attackerId * 100;
			attackerLat = targetLat + Math.sin(phase) * 0.08;
			attackerLng = wrapLongitude(targetLng + Math.cos(phase) * 0.08);
		} else {
			const angle = random() * Math.PI * 2;
			const radius = 0.02 + random() * 0.25;
			attackerLat = targetLat + Math.sin(angle) * radius;
			attackerLng = wrapLongitude(targetLng + Math.cos(angle) * radius);
		}
		const attackerKind = pair % 5 === 0 ? "armor" : "army";
		const targetKind = pair % 7 === 0 ? "armor" : "army";
		const sea = layer === "proximity" && pair % 9 === 0;
		const transport = sea && pair % 18 === 9;
		testCase.units.push(
			makeUnit(attackerId, 0, attackerKind, attackerLat, attackerLng, random, {
				transport,
				atSea: sea,
				supported: pair % 3 === 0,
				landingPenalty: pair % 13 === 0,
			}),
			makeUnit(targetId, 1, targetKind, targetLat, targetLng, random, {
				transport: sea && !transport && pair % 5 === 0,
				atSea: sea,
				supported: pair % 4 === 0,
				landingPenalty: pair % 17 === 0,
			}),
		);
		testCase.operations.push({ layer, attackerId, targetId });
	}
	return cases;
}

const unitsPerSide = parseUnitsPerSide(process.argv[2]);
const random = createRandom(SEED);
const fixture = {
	schema: "movement-combat-v1",
	movementCases: movementCases(unitsPerSide * 2, random),
	combatCases: combatCases(unitsPerSide, random),
};
process.stdout.write(`${JSON.stringify(fixture)}\n`);
