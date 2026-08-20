#!/usr/bin/env node
/** Exact deterministic JavaScript oracle for the native ai-orders-v1 contract. */
import { readFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";

export const SCHEMA = "ai-orders-v1";
const SAFE_ID = Number.MAX_SAFE_INTEGER;
const REASONS = new Set([
	"contact",
	"retreat",
	"front",
	"reinforce",
	"field",
	"hold",
]);

class OracleError extends Error {
	constructor(code, message) {
		super(`ai-orders-v1: ${message}`);
		this.code = code;
	}
}
const fail = (condition, code, message) => {
	if (!condition) throw new OracleError(code, message);
};
const finite = (value) => typeof value === "number" && Number.isFinite(value);
const integer = (value) => Number.isSafeInteger(value);
const exactKeys = (value, keys, name) => {
	fail(
		value !== null && typeof value === "object" && !Array.isArray(value),
		"invalid_input",
		`${name} must be an object`,
	);
	const actual = Object.keys(value).sort();
	const expected = [...keys].sort();
	fail(
		actual.length === expected.length &&
			actual.every((key, index) => key === expected[index]),
		"invalid_input",
		`${name} keys must be ${expected.join(", ")}`,
	);
};
const wrap = (lng) => ((((lng + 180) % 360) + 360) % 360) - 180;
function wrappedDelta(from, to) {
	let delta = to - from;
	if (delta > 180) delta -= 360;
	else if (delta < -180) delta += 360;
	return delta;
}
function normalize(lat, lng) {
	const magnitude = Math.hypot(lat, lng);
	return magnitude > 0 && Number.isFinite(magnitude)
		? [lat / magnitude, lng / magnitude]
		: null;
}
function directionTo(fromLat, fromLng, toLat, toLng) {
	return normalize(toLat - fromLat, wrappedDelta(fromLng, toLng));
}

const CONFIG_KEYS = [
	"contactScanRadius",
	"retreatMinHostilePower",
	"retreatMultiple",
	"retreatBoost",
	"encircledRetreatMultiplier",
	"priorAssignmentStickiness",
	"reinforcementReadinessThreshold",
	"contactPlanSpeedMultiplier",
	"frontPlanSpeedMultiplier",
	"reinforcementPlanSpeedMultiplier",
	"fieldPlanSpeedMultiplier",
	"maxUnits",
	"maxObjectives",
	"maxGridCells",
	"maxAssignmentEdges",
];
const MOVEMENT_KEYS = [
	"terrainSpeedMultiplier",
	"speedMultiplier",
	"planSpeedMultiplier",
	"neutralPenalty",
	"pushReadiness",
];
const COMBAT_KEYS = [
	"dealtMultiplier",
	"takenMultiplier",
	"defenseBonus",
	"longWarDefense",
	"mountain",
	"urban",
];
const UNIT_KEYS = [
	"id",
	"side",
	"sovereign",
	"kind",
	"lat",
	"lng",
	"health",
	"maxHealth",
	"combatPower",
	"allyWeight",
	"atSea",
	"transport",
	"baseSpeed",
	"movement",
	"combat",
	"previousAssignment",
	"isReserve",
	"reinforcementEligible",
	"encircled",
];
const OBJECTIVE_KEYS = [
	"id",
	"sidePair",
	"segmentId",
	"lat",
	"lng",
	"capacity",
	"priority",
];
const WORLD_KEYS = [
	"gridWidth",
	"gridHeight",
	"gridRes",
	"landMask",
	"dominantSideMap",
	"maxSides",
	"hostilityMatrix",
	"frontlineLatitude",
	"frontlineLongitude",
	"objectives",
];
const CASE_KEYS = [
	"name",
	"config",
	"world",
	"units",
	"verifyPermutationInvariance",
	"expectedError",
];

function validateConfig(config) {
	exactKeys(config, CONFIG_KEYS, "config");
	for (const key of CONFIG_KEYS.slice(0, 11))
		fail(finite(config[key]), "invalid_config", `config.${key} must be finite`);
	for (const key of CONFIG_KEYS.slice(11))
		fail(
			integer(config[key]) && config[key] > 0,
			"invalid_config",
			`config.${key} must be a positive safe integer`,
		);
	fail(
		config.contactScanRadius > 0 && config.contactScanRadius <= 180,
		"invalid_config",
		"contactScanRadius is out of range",
	);
	for (const key of [
		"retreatMinHostilePower",
		"retreatMultiple",
		"retreatBoost",
		"encircledRetreatMultiplier",
		"priorAssignmentStickiness",
		"contactPlanSpeedMultiplier",
		"frontPlanSpeedMultiplier",
		"reinforcementPlanSpeedMultiplier",
		"fieldPlanSpeedMultiplier",
	])
		fail(
			config[key] >= 0,
			"invalid_config",
			`config.${key} must be nonnegative`,
		);
	fail(
		config.reinforcementReadinessThreshold >= 0 &&
			config.reinforcementReadinessThreshold <= 1,
		"invalid_config",
		"reinforcementReadinessThreshold is out of range",
	);
}

function validateUnit(unit, maxSides) {
	exactKeys(unit, UNIT_KEYS, `unit ${unit?.id ?? "?"}`);
	fail(
		integer(unit.id) && unit.id >= 0 && unit.id <= SAFE_ID,
		"invalid_unit",
		"unit id must be a nonnegative safe integer",
	);
	fail(
		integer(unit.side) && unit.side >= 0 && unit.side < maxSides,
		"invalid_unit",
		`unit ${unit.id} side is out of range`,
	);
	fail(
		integer(unit.sovereign) && unit.sovereign >= 0 && unit.sovereign <= SAFE_ID,
		"invalid_unit",
		`unit ${unit.id} sovereign is invalid`,
	);
	fail(
		unit.kind === "army" || unit.kind === "armor",
		"invalid_unit",
		`unit ${unit.id} kind is invalid`,
	);
	for (const key of [
		"lat",
		"lng",
		"health",
		"maxHealth",
		"combatPower",
		"allyWeight",
		"baseSpeed",
	])
		fail(
			finite(unit[key]),
			"invalid_unit",
			`unit ${unit.id} ${key} must be finite`,
		);
	fail(
		unit.lat >= -90 && unit.lat <= 90 && unit.lng >= -180 && unit.lng <= 180,
		"invalid_unit",
		`unit ${unit.id} position is out of range`,
	);
	fail(
		unit.maxHealth > 0 &&
			unit.health >= 0 &&
			unit.health <= unit.maxHealth &&
			unit.combatPower >= 0 &&
			unit.allyWeight >= 0 &&
			unit.baseSpeed >= 0,
		"invalid_unit",
		`unit ${unit.id} scalar is out of range`,
	);
	for (const key of [
		"atSea",
		"transport",
		"isReserve",
		"reinforcementEligible",
		"encircled",
	])
		fail(
			typeof unit[key] === "boolean",
			"invalid_unit",
			`unit ${unit.id} ${key} must be boolean`,
		);
	exactKeys(unit.movement, MOVEMENT_KEYS, `unit ${unit.id}.movement`);
	for (const key of MOVEMENT_KEYS)
		fail(
			finite(unit.movement[key]) && unit.movement[key] >= 0,
			"invalid_unit",
			`unit ${unit.id}.movement.${key} is invalid`,
		);
	exactKeys(unit.combat, COMBAT_KEYS, `unit ${unit.id}.combat`);
	for (const key of COMBAT_KEYS.slice(0, 4))
		fail(
			finite(unit.combat[key]) && unit.combat[key] >= 0,
			"invalid_unit",
			`unit ${unit.id}.combat.${key} is invalid`,
		);
	for (const key of COMBAT_KEYS.slice(4))
		fail(
			typeof unit.combat[key] === "boolean",
			"invalid_unit",
			`unit ${unit.id}.combat.${key} must be boolean`,
		);
	if (unit.previousAssignment !== null) {
		exactKeys(
			unit.previousAssignment,
			["objectiveId"],
			`unit ${unit.id}.previousAssignment`,
		);
		fail(
			integer(unit.previousAssignment.objectiveId) &&
				unit.previousAssignment.objectiveId >= 0 &&
				unit.previousAssignment.objectiveId <= SAFE_ID,
			"invalid_unit",
			`unit ${unit.id} previous objective is invalid`,
		);
	}
}

function validateWorld(world) {
	exactKeys(world, WORLD_KEYS, "world");
	fail(
		integer(world.gridWidth) &&
			world.gridWidth > 0 &&
			integer(world.gridHeight) &&
			world.gridHeight > 0 &&
			finite(world.gridRes) &&
			world.gridRes > 0,
		"invalid_world",
		"grid shape is invalid",
	);
	fail(
		integer(world.maxSides) && world.maxSides > 0 && world.maxSides <= 128,
		"invalid_hostility",
		"maxSides is invalid",
	);
	const cells = world.gridWidth * world.gridHeight;
	fail(
		Number.isSafeInteger(cells),
		"invalid_world",
		"grid size overflows safe integer",
	);
	fail(
		Array.isArray(world.landMask) &&
			world.landMask.length === cells &&
			world.landMask.every(
				(value) => integer(value) && value >= 0 && value <= 2,
			),
		"invalid_world",
		"landMask is invalid",
	);
	fail(
		Array.isArray(world.dominantSideMap) &&
			world.dominantSideMap.length === cells &&
			world.dominantSideMap.every(
				(value) => integer(value) && value >= -1 && value < world.maxSides,
			),
		"invalid_world",
		"dominantSideMap is invalid",
	);
	fail(
		Array.isArray(world.hostilityMatrix) &&
			world.hostilityMatrix.length === world.maxSides ** 2 &&
			world.hostilityMatrix.every((value) => value === 0 || value === 1),
		"invalid_hostility",
		"hostilityMatrix is invalid",
	);
	fail(
		(world.frontlineLatitude === null) === (world.frontlineLongitude === null),
		"invalid_world",
		"frontline direction grids must both be present or absent",
	);
	if (world.frontlineLatitude !== null) {
		fail(
			Array.isArray(world.frontlineLatitude) &&
				world.frontlineLatitude.length === cells &&
				world.frontlineLatitude.every(finite),
			"invalid_world",
			"frontlineLatitude is invalid",
		);
		fail(
			Array.isArray(world.frontlineLongitude) &&
				world.frontlineLongitude.length === cells &&
				world.frontlineLongitude.every(finite),
			"invalid_world",
			"frontlineLongitude is invalid",
		);
	}
	fail(
		Array.isArray(world.objectives),
		"invalid_input",
		"world.objectives must be an array",
	);
	const ids = new Set();
	for (const objective of world.objectives) {
		exactKeys(objective, OBJECTIVE_KEYS, `objective ${objective?.id ?? "?"}`);
		fail(
			integer(objective.id) && objective.id >= 0 && objective.id <= SAFE_ID,
			"invalid_objective",
			"objective id is invalid",
		);
		fail(
			!ids.has(objective.id),
			"duplicate_objective",
			`objective ${objective.id} is duplicated`,
		);
		ids.add(objective.id);
		fail(
			Array.isArray(objective.sidePair) &&
				objective.sidePair.length === 2 &&
				objective.sidePair.every(
					(side) => integer(side) && side >= 0 && side < world.maxSides,
				) &&
				objective.sidePair[0] !== objective.sidePair[1],
			"invalid_objective",
			`objective ${objective.id} sidePair is invalid`,
		);
		fail(
			integer(objective.segmentId) &&
				objective.segmentId >= 0 &&
				objective.segmentId <= SAFE_ID &&
				finite(objective.lat) &&
				objective.lat >= -90 &&
				objective.lat <= 90 &&
				finite(objective.lng) &&
				objective.lng >= -180 &&
				objective.lng <= 180 &&
				integer(objective.capacity) &&
				objective.capacity > 0 &&
				Number.isSafeInteger(objective.priority),
			"invalid_objective",
			`objective ${objective.id} is invalid`,
		);
	}
	return cells;
}

export function prepareFixture(fixture) {
	exactKeys(fixture, ["schema", "cases"], "fixture");
	fail(
		fixture.schema === SCHEMA,
		"invalid_input",
		`schema must be '${SCHEMA}'`,
	);
	fail(
		Array.isArray(fixture.cases) && fixture.cases.length > 0,
		"invalid_input",
		"cases must be a nonempty array",
	);
	const names = new Set();
	for (const testCase of fixture.cases) {
		exactKeys(testCase, CASE_KEYS, "case");
		fail(
			typeof testCase.name === "string" &&
				testCase.name.length > 0 &&
				!names.has(testCase.name),
			"invalid_input",
			"case name must be unique and nonempty",
		);
		names.add(testCase.name);
		fail(
			typeof testCase.verifyPermutationInvariance === "boolean",
			"invalid_input",
			`${testCase.name}.verifyPermutationInvariance must be boolean`,
		);
		fail(
			testCase.expectedError === null ||
				typeof testCase.expectedError === "string",
			"invalid_input",
			`${testCase.name}.expectedError is invalid`,
		);
	}
	return fixture;
}

function validateCase(testCase) {
	validateConfig(testCase.config);
	const cells = validateWorld(testCase.world);
	fail(
		Array.isArray(testCase.units),
		"invalid_input",
		`${testCase.name}.units must be an array`,
	);
	fail(
		testCase.units.length <= testCase.config.maxUnits &&
			testCase.world.objectives.length <= testCase.config.maxObjectives &&
			cells <= testCase.config.maxGridCells &&
			testCase.units.length * testCase.world.objectives.length <=
				testCase.config.maxAssignmentEdges,
		"planning_limit_exceeded",
		`${testCase.name} exceeds planning bounds`,
	);
	const ids = new Set();
	for (const unit of testCase.units) {
		if (ids.has(unit.id))
			throw new OracleError("duplicate_unit", `unit ${unit.id} is duplicated`);
		ids.add(unit.id);
		validateUnit(unit, testCase.world.maxSides);
	}
}

function hostile(world, left, right) {
	return left !== right && world.hostility[left * world.maxSides + right] === 1;
}
function objectiveApplies(unit, objective, world) {
	return (
		unit.side === objective.sidePair[0] &&
		hostile(world, objective.sidePair[0], objective.sidePair[1])
	);
}
function objectiveDistanceSq(unit, objective) {
	const dLat = objective.lat - unit.lat;
	const dLng = wrappedDelta(unit.lng, objective.lng);
	return dLat * dLat + dLng * dLng;
}

function tacticalCoords(lat, lng, cellSize, columns, rows) {
	const x = Math.min(columns - 1, Math.floor((wrap(lng) + 180) / cellSize));
	const y = Math.min(
		rows - 1,
		Math.floor((Math.max(-90, Math.min(90, lat)) + 90) / cellSize),
	);
	return { x, y, key: y * columns + x };
}
function discoverContacts(config, units, world) {
	const columns = Math.ceil(360 / config.contactScanRadius);
	const rows = Math.ceil(180 / config.contactScanRadius);
	const bySide = new Map();
	for (let index = 0; index < units.length; index++) {
		const unit = units[index];
		const { key } = tacticalCoords(
			unit.lat,
			unit.lng,
			config.contactScanRadius,
			columns,
			rows,
		);
		if (!bySide.has(unit.side)) bySide.set(unit.side, new Map());
		const cells = bySide.get(unit.side);
		if (!cells.has(key)) cells.set(key, []);
		cells.get(key).push(index);
	}
	const sides = [...bySide.keys()].sort((a, b) => a - b);
	const radiusSq = config.contactScanRadius ** 2;
	const result = [];
	for (let unitIndex = 0; unitIndex < units.length; unitIndex++) {
		const unit = units[unitIndex];
		const origin = tacticalCoords(
			unit.lat,
			unit.lng,
			config.contactScanRadius,
			columns,
			rows,
		);
		const contact = {
			preferredTargetId: null,
			preferredDistanceSq: Infinity,
			preferredDeltaLat: 0,
			preferredDeltaLng: 0,
			friendlyPower: unit.combatPower * unit.allyWeight,
			hostilePower: 0,
			hostileDeltaLat: 0,
			hostileDeltaLng: 0,
			retreat: false,
			retreatDir: null,
		};
		for (const side of sides) {
			const isEnemy = hostile(world, unit.side, side);
			if (side !== unit.side && !isEnemy) continue;
			const sideCells = bySide.get(side);
			const keys = [];
			for (let dy = -1; dy <= 1; dy++) {
				const y = origin.y + dy;
				if (y < 0 || y >= rows) continue;
				for (let dx = -1; dx <= 1; dx++) {
					let x = origin.x + dx;
					if (x < 0) x = columns - 1;
					else if (x >= columns) x = 0;
					const key = y * columns + x;
					if (sideCells.has(key) && !keys.includes(key)) keys.push(key);
				}
			}
			keys.sort((a, b) => a - b);
			for (const key of keys)
				for (const otherIndex of sideCells.get(key)) {
					if (otherIndex === unitIndex) continue;
					const other = units[otherIndex];
					const dLat = other.lat - unit.lat;
					const dLng = wrappedDelta(unit.lng, other.lng);
					const dSq = dLat * dLat + dLng * dLng;
					if (dSq > radiusSq) continue;
					if (isEnemy) {
						contact.hostilePower += other.combatPower;
						contact.hostileDeltaLat += dLat * other.combatPower;
						contact.hostileDeltaLng += dLng * other.combatPower;
						if (
							dSq < contact.preferredDistanceSq ||
							(dSq === contact.preferredDistanceSq &&
								(contact.preferredTargetId === null ||
									other.id < contact.preferredTargetId))
						) {
							contact.preferredTargetId = other.id;
							contact.preferredDistanceSq = dSq;
							contact.preferredDeltaLat = dLat;
							contact.preferredDeltaLng = dLng;
						}
					} else if (side === unit.side)
						contact.friendlyPower += other.combatPower * other.allyWeight;
				}
		}
		contact.retreat =
			contact.hostilePower >= config.retreatMinHostilePower &&
			contact.hostilePower > contact.friendlyPower * config.retreatMultiple;
		if (contact.retreat && contact.hostilePower > 0)
			contact.retreatDir = normalize(
				-contact.hostileDeltaLat / contact.hostilePower,
				-contact.hostileDeltaLng / contact.hostilePower,
			);
		result.push(contact);
	}
	return result;
}

function assignFronts(config, units, world, contacts, reinforcement) {
	const assigned = new Array(units.length).fill(null);
	const occupancy = new Array(world.objectives.length).fill(0);
	const byId = new Map(
		world.objectives.map((objective, index) => [objective.id, index]),
	);
	let stickyAssignments = 0;
	let frontAssignments = 0;
	let reinforcementAssignments = 0;
	const sticky = [];
	for (let i = 0; i < units.length; i++) {
		const unit = units[i];
		if (
			contacts[i].retreat ||
			reinforcement[i] ||
			unit.previousAssignment === null
		)
			continue;
		const oi = byId.get(unit.previousAssignment.objectiveId);
		if (oi === undefined) continue;
		const objective = world.objectives[oi];
		const distanceSq = objectiveDistanceSq(unit, objective);
		if (
			objectiveApplies(unit, objective, world) &&
			distanceSq <= config.priorAssignmentStickiness ** 2
		)
			sticky.push({ unitIndex: i, objectiveIndex: oi, distanceSq });
	}
	sticky.sort(
		(a, b) =>
			world.objectives[a.objectiveIndex].id -
				world.objectives[b.objectiveIndex].id ||
			a.distanceSq - b.distanceSq ||
			units[a.unitIndex].id - units[b.unitIndex].id,
	);
	for (const edge of sticky)
		if (
			assigned[edge.unitIndex] === null &&
			occupancy[edge.objectiveIndex] <
				world.objectives[edge.objectiveIndex].capacity
		) {
			assigned[edge.unitIndex] = edge.objectiveIndex;
			occupancy[edge.objectiveIndex]++;
			stickyAssignments++;
		}
	const edges = [];
	for (let i = 0; i < units.length; i++)
		if (!contacts[i].retreat && !reinforcement[i] && assigned[i] === null)
			for (let oi = 0; oi < world.objectives.length; oi++)
				if (objectiveApplies(units[i], world.objectives[oi], world))
					edges.push({
						unitIndex: i,
						objectiveIndex: oi,
						distanceSq: objectiveDistanceSq(units[i], world.objectives[oi]),
					});
	edges.sort(
		(a, b) =>
			world.objectives[b.objectiveIndex].priority -
				world.objectives[a.objectiveIndex].priority ||
			a.distanceSq - b.distanceSq ||
			world.objectives[a.objectiveIndex].id -
				world.objectives[b.objectiveIndex].id ||
			units[a.unitIndex].id - units[b.unitIndex].id,
	);
	for (const edge of edges)
		if (
			assigned[edge.unitIndex] === null &&
			occupancy[edge.objectiveIndex] <
				world.objectives[edge.objectiveIndex].capacity
		) {
			assigned[edge.unitIndex] = edge.objectiveIndex;
			occupancy[edge.objectiveIndex]++;
			frontAssignments++;
		}
	const reserves = units
		.map((_, index) => index)
		.filter((index) => reinforcement[index] && !contacts[index].retreat)
		.sort(
			(a, b) =>
				units[a].health / units[a].maxHealth -
					units[b].health / units[b].maxHealth || units[a].id - units[b].id,
		);
	for (const unitIndex of reserves) {
		let best = null;
		for (let oi = 0; oi < world.objectives.length; oi++) {
			const objective = world.objectives[oi];
			if (
				occupancy[oi] >= objective.capacity ||
				!objectiveApplies(units[unitIndex], objective, world)
			)
				continue;
			if (best === null) {
				best = oi;
				continue;
			}
			const current = world.objectives[best];
			const fillCmp =
				occupancy[oi] * current.capacity - occupancy[best] * objective.capacity;
			const cmp =
				fillCmp ||
				current.priority - objective.priority ||
				objectiveDistanceSq(units[unitIndex], objective) -
					objectiveDistanceSq(units[unitIndex], current) ||
				objective.id - current.id;
			if (cmp < 0) best = oi;
		}
		if (best !== null) {
			assigned[unitIndex] = best;
			occupancy[best]++;
			reinforcementAssignments++;
		}
	}
	return {
		assigned,
		stickyAssignments,
		frontAssignments,
		reinforcementAssignments,
	};
}

function gridIndex(unit, world) {
	const x = Math.floor((wrap(unit.lng) + 180) / world.gridRes);
	const y = Math.floor((unit.lat + 90) / world.gridRes);
	return x >= 0 && y >= 0 && x < world.gridWidth && y < world.gridHeight
		? y * world.gridWidth + x
		: null;
}
function fieldDirection(unit, world) {
	if (world.frontlineLatitude === null) return null;
	const index = gridIndex(unit, world);
	return index === null
		? null
		: normalize(
				world.frontlineLatitude[index],
				world.frontlineLongitude[index],
			);
}
function nearestFriendlyDirection(unit, world) {
	let best = null;
	for (let index = 0; index < world.landMask.length; index++) {
		if (
			world.landMask[index] === 0 ||
			world.dominantSideMap[index] !== unit.side
		)
			continue;
		const x = index % world.gridWidth;
		const y = Math.floor(index / world.gridWidth);
		const lat = (y + 0.5) * world.gridRes - 90;
		const lng = (x + 0.5) * world.gridRes - 180;
		const dLat = lat - unit.lat;
		const dLng = wrappedDelta(unit.lng, lng);
		const dSq = dLat * dLat + dLng * dLng;
		if (
			best === null ||
			dSq < best.distanceSq ||
			(dSq === best.distanceSq && index < best.index)
		)
			best = { distanceSq: dSq, index, lat, lng };
	}
	return best === null
		? null
		: directionTo(unit.lat, unit.lng, best.lat, best.lng);
}

function resolveCase(testCase) {
	const config = testCase.config;
	const source = testCase.world;
	const world = {
		...source,
		landMask: Uint8Array.from(source.landMask),
		dominantSideMap: Int16Array.from(source.dominantSideMap),
		hostility: Uint8Array.from(source.hostilityMatrix),
		frontlineLatitude:
			source.frontlineLatitude === null
				? null
				: Float32Array.from(source.frontlineLatitude),
		frontlineLongitude:
			source.frontlineLongitude === null
				? null
				: Float32Array.from(source.frontlineLongitude),
	};
	const units = [...testCase.units].sort((a, b) => a.id - b.id);
	const contacts = discoverContacts(config, units, world);
	const reinforcement = units.map(
		(unit) =>
			unit.isReserve ||
			(unit.reinforcementEligible &&
				unit.health / unit.maxHealth <= config.reinforcementReadinessThreshold),
	);
	const fronts = assignFronts(config, units, world, contacts, reinforcement);
	const counters = {
		inputUnits: units.length,
		contactOrders: 0,
		retreatOrders: 0,
		stickyAssignments: fronts.stickyAssignments,
		frontAssignments: fronts.frontAssignments,
		reinforcementAssignments: fronts.reinforcementAssignments,
		fieldOrders: 0,
		holdOrders: 0,
	};
	const orders = [];
	const assignments = [];
	for (let i = 0; i < units.length; i++) {
		const unit = units[i];
		const contact = contacts[i];
		const oi = fronts.assigned[i];
		let direction = null;
		let reason;
		if (contact.retreat) {
			direction = contact.retreatDir;
			reason = "retreat";
		} else {
			if (contact.preferredTargetId !== null) {
				direction = normalize(
					contact.preferredDeltaLat,
					contact.preferredDeltaLng,
				);
				if (direction !== null) reason = "contact";
			}
			if (reason === undefined && oi !== null) {
				const objective = world.objectives[oi];
				direction = directionTo(
					unit.lat,
					unit.lng,
					objective.lat,
					objective.lng,
				);
				reason = reinforcement[i] ? "reinforce" : "front";
			} else if (reason === undefined && reinforcement[i]) {
				direction = nearestFriendlyDirection(unit, world);
				reason = direction === null ? "hold" : "reinforce";
			} else if (reason === undefined) {
				direction = fieldDirection(unit, world);
				reason = direction === null ? "hold" : "field";
			}
		}
		fail(REASONS.has(reason), "invalid_input", "internal reason error");
		if (reason === "contact") counters.contactOrders++;
		else if (reason === "retreat") counters.retreatOrders++;
		else if (reason === "field") counters.fieldOrders++;
		else if (reason === "hold") counters.holdOrders++;
		const reasonSpeed =
			reason === "contact"
				? config.contactPlanSpeedMultiplier
				: reason === "front"
					? config.frontPlanSpeedMultiplier
					: reason === "reinforce"
						? config.reinforcementPlanSpeedMultiplier
						: reason === "field"
							? config.fieldPlanSpeedMultiplier
							: 1;
		const retreatBoost =
			reason === "retreat"
				? config.retreatBoost *
					(unit.encircled ? config.encircledRetreatMultiplier : 1)
				: 1;
		orders.push({
			unitId: unit.id,
			preferredTargetId: contact.preferredTargetId,
			movementEnabled:
				direction !== null && unit.baseSpeed > 0 && unit.health > 0,
			dirLat: direction?.[0] ?? 0,
			dirLng: direction?.[1] ?? 0,
			factors: {
				baseSpeed: unit.baseSpeed,
				speedMult:
					unit.movement.terrainSpeedMultiplier * unit.movement.speedMultiplier,
				planSpeedMult: unit.movement.planSpeedMultiplier * reasonSpeed,
				neutralPenalty: unit.movement.neutralPenalty,
				retreatBoost,
				pushReadiness: unit.movement.pushReadiness,
			},
			combat: {
				dealtMultiplier: unit.combat.dealtMultiplier,
				takenMultiplier: unit.combat.takenMultiplier,
				defenseBonus: unit.combat.defenseBonus,
				longWarDefense: unit.combat.longWarDefense,
				mountain: unit.combat.mountain,
				urban: unit.combat.urban,
			},
		});
		assignments.push({
			unitId: unit.id,
			objectiveId: oi === null ? null : world.objectives[oi].id,
			reason,
		});
	}
	return { orders, assignments, counters };
}

function errorCode(error) {
	return error instanceof OracleError ? error.code : "unknown_error";
}
function runOne(testCase, validate = true) {
	try {
		if (validate) validateCase(testCase);
		const result = resolveCase(testCase);
		if (testCase.expectedError !== null)
			throw new OracleError(
				"unexpected_success",
				`case ${testCase.name} expected ${testCase.expectedError}`,
			);
		if (testCase.verifyPermutationInvariance) {
			const permuted = {
				...testCase,
				units: [...testCase.units].reverse(),
				world: {
					...testCase.world,
					objectives: [...testCase.world.objectives].reverse(),
				},
				verifyPermutationInvariance: false,
			};
			const other = resolveCase(permuted);
			fail(
				JSON.stringify(result) === JSON.stringify(other),
				"permutation_mismatch",
				`case ${testCase.name} is permutation-sensitive`,
			);
		}
		return { name: testCase.name, result, error: null };
	} catch (error) {
		const code = errorCode(error);
		if (testCase.expectedError === code)
			return { name: testCase.name, result: null, error: code };
		throw error;
	}
}
export function runFixture(fixture) {
	prepareFixture(fixture);
	return { schema: SCHEMA, cases: fixture.cases.map(runOne) };
}

function checksum(report) {
	let hash = 14695981039346656037n;
	const prime = 1099511628211n;
	const mask = 0xffffffffffffffffn;
	const scale = 1_000_000_000;
	const encoder = new TextEncoder();
	const byte = (value) => {
		hash ^= BigInt(value);
		hash = (hash * prime) & mask;
	};
	const u64 = (value) => {
		let current = BigInt.asUintN(64, BigInt(value));
		for (let index = 0; index < 8; index++) {
			byte(Number(current & 0xffn));
			current >>= 8n;
		}
	};
	const bool = (value) => u64(value ? 1 : 0);
	const optionalU64 = (value) => {
		bool(value !== null && value !== undefined);
		if (value !== null && value !== undefined) u64(value);
	};
	const float = (value) => {
		bool(value < 0 || Object.is(value, -0));
		u64(
			Math.min(
				Number.MAX_SAFE_INTEGER,
				Math.floor(Math.abs(value) * scale + 0.5),
			),
		);
	};
	const text = (value) => {
		const bytes = encoder.encode(value);
		u64(bytes.length);
		for (const value of bytes) byte(value);
	};
	const reasonCode = new Map([
		["contact", 0],
		["retreat", 1],
		["front", 2],
		["reinforce", 3],
		["field", 4],
		["hold", 5],
	]);
	u64(report.cases.length);
	for (const testCase of report.cases) {
		text(testCase.name);
		bool(testCase.error !== null);
		if (testCase.error !== null) text(testCase.error);
		bool(testCase.result !== null);
		if (testCase.result === null) continue;
		u64(testCase.result.orders.length);
		for (const order of testCase.result.orders) {
			u64(order.unitId);
			optionalU64(order.preferredTargetId);
			bool(order.movementEnabled);
			float(order.dirLat);
			float(order.dirLng);
			for (const value of [
				order.factors.baseSpeed,
				order.factors.speedMult,
				order.factors.planSpeedMult,
				order.factors.neutralPenalty,
				order.factors.retreatBoost,
				order.factors.pushReadiness,
				order.combat.dealtMultiplier,
				order.combat.takenMultiplier,
				order.combat.defenseBonus,
				order.combat.longWarDefense,
			])
				float(value);
			bool(order.combat.mountain);
			bool(order.combat.urban);
		}
		u64(testCase.result.assignments.length);
		for (const assignment of testCase.result.assignments) {
			u64(assignment.unitId);
			optionalU64(assignment.objectiveId);
			u64(reasonCode.get(assignment.reason));
		}
		for (const value of [
			testCase.result.counters.inputUnits,
			testCase.result.counters.contactOrders,
			testCase.result.counters.retreatOrders,
			testCase.result.counters.stickyAssignments,
			testCase.result.counters.frontAssignments,
			testCase.result.counters.reinforcementAssignments,
			testCase.result.counters.fieldOrders,
			testCase.result.counters.holdOrders,
		])
			u64(value);
	}
	return hash.toString(16).padStart(16, "0");
}
function percentile(samples, value) {
	const sorted = [...samples].sort((a, b) => a - b);
	return sorted[
		Math.max(
			0,
			Math.min(sorted.length - 1, Math.ceil(sorted.length * value) - 1),
		)
	];
}
export function benchmarkFixture(fixture, options = {}) {
	prepareFixture(fixture);
	const preflight = fixture.cases.map((testCase) => runOne(testCase, true));
	const repeat = Number(options.repeat ?? 20);
	const warmup = Number(options.warmup ?? 5);
	fail(
		integer(repeat) && repeat > 0 && integer(warmup) && warmup >= 0,
		"invalid_input",
		"repeat/warmup are invalid",
	);
	const plan = () => ({
		schema: SCHEMA,
		cases: fixture.cases.map((testCase, index) =>
			testCase.expectedError === null
				? runOne(testCase, false)
				: preflight[index],
		),
	});
	for (let i = 0; i < warmup; i++) plan();
	const samples = [];
	let report;
	for (let i = 0; i < repeat; i++) {
		const started = performance.now();
		report = plan();
		samples.push(performance.now() - started);
	}
	const counters = {
		inputUnits: 0,
		contactOrders: 0,
		retreatOrders: 0,
		stickyAssignments: 0,
		frontAssignments: 0,
		reinforcementAssignments: 0,
		fieldOrders: 0,
		holdOrders: 0,
	};
	for (const testCase of report.cases) {
		if (!testCase.result) continue;
		for (const key of Object.keys(counters))
			counters[key] += testCase.result.counters[key];
	}
	return {
		schema: SCHEMA,
		mode: "bench",
		cases: fixture.cases.length,
		units: fixture.cases.reduce((sum, item) => sum + item.units.length, 0),
		objectives: fixture.cases.reduce(
			(sum, item) => sum + item.world.objectives.length,
			0,
		),
		repeat,
		warmup,
		planning: {
			medianMs: percentile(samples, 0.5),
			p95Ms: percentile(samples, 0.95),
		},
		counters,
		checksum: checksum(report),
	};
}
function parseOptions(args) {
	const result = {};
	for (let index = 0; index < args.length; index++) {
		const equals = /^--([^=]+)=(.+)$/.exec(args[index]);
		if (equals) result[equals[1]] = equals[2];
		else if (/^--/.test(args[index]))
			result[args[index].slice(2)] = args[++index];
		else throw new Error(`unknown option ${args[index]}`);
	}
	return result;
}
async function main() {
	const [fixturePath, mode = "report", ...args] = process.argv.slice(2);
	if (!fixturePath || !["report", "bench"].includes(mode))
		throw new Error(
			"usage: js-ai-orders-reference.mjs <fixture.json> [report|bench] [--repeat=N --warmup=N]",
		);
	const fixture = JSON.parse(await readFile(fixturePath, "utf8"));
	const output =
		mode === "bench"
			? benchmarkFixture(fixture, parseOptions(args))
			: runFixture(fixture);
	process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
}
if (import.meta.url === `file://${process.argv[1]}`)
	main().catch((error) => {
		console.error(error.stack || error.message);
		process.exitCode = 1;
	});
