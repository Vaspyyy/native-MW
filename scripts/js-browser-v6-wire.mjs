#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [webRoot, checkpointPath, outputPath] = process.argv.slice(2);
if (!webRoot || !checkpointPath || !outputPath) {
	throw new Error(
		"usage: js-browser-v6-wire.mjs <modern-wars-root> <v6-checkpoint> <output>",
	);
}

const main = readFileSync(join(webRoot, "src/main.js"), "utf8");
const start = main.indexOf("function buildMidWarNativeRuntimeCheckpointV6");
const end = main.indexOf("function createNativeRuntimeCheckpoint", start);
assert.ok(start >= 0 && end > start, "browser v6 checkpoint exporter exists");

const source = JSON.parse(readFileSync(checkpointPath, "utf8"));
assert.equal(source.schema, "native-runtime-checkpoint-v6");
assert.equal(
	source.operationalExecution?.schema,
	"native-operational-execution-v1",
);
assert.equal(source.airPower?.schema, "native-air-v2");
assert.equal(
	source.airPower.countryCoverage?.length,
	source.economies.length,
	"production save covers every stable country",
);
assert.deepEqual(
	source.airPower.countryCoverage.map((coverage) => coverage.countryId),
	[...source.economies]
		.sort((left, right) => left.countryId - right.countryId)
		.map((economy) => economy.countryId),
	"production air coverage is complete and ordered by country",
);
assert.ok(source.airPower.airfields.length > 0, "production save has airfields");
assert.ok(source.airPower.wings.length > 0, "production save has air wings");

const base = structuredClone(source);
base.schema = "native-runtime-checkpoint-v5";
delete base.operationalExecution;
delete base.airPower;

const topology = {
	stable: base.sides.map((_, browserSideIndex) => ({ browserSideIndex })),
	browserToNativeSide: new Map(
		base.sides.map((_, sideIndex) => [sideIndex, sideIndex]),
	),
};
const sideUids = base.sides.map((_, side) => `wire-side-${side}`);
const sides = base.sides.map((side) =>
	side.countryIds.map((id) => ({ id })),
);
const browserIdByNativeUnit = new Map(
	base.units.map((unit) => [unit.id, `browser-unit-${unit.id}`]),
);
base.units.forEach((unit, index) => {
	assert.equal(unit.id, index + 1, "production unit IDs stay dense and stable");
});
const units = base.units.map((unit) => ({
	id: browserIdByNativeUnit.get(unit.id),
	sideIndex: unit.side,
	sovereignId: unit.countryId,
	kind: unit.kind,
	health: unit.health,
	formationStrength: Math.max(1, unit.personnel || unit.equipment || 1),
	lat: unit.lat,
	lng: unit.lng,
	navalAssigned: false,
	supplyAssigned: false,
	_transportPlanSignature: null,
	_defenderReactTarget: null,
}));
const unitsBySide = base.sides.map((_, side) =>
	units.filter((unit) => unit.sideIndex === side),
);
assert.ok(unitsBySide[0]?.length >= 5, "attacker has representative units");
assert.ok(unitsBySide[1]?.length >= 1, "defender has a representative unit");

const attackerUnits = unitsBySide[0].slice(0, 5);
const defenderUnit = unitsBySide[1][0];
const staging = { lat: attackerUnits[0].lat, lng: attackerUnits[0].lng };
const target = { lat: defenderUnit.lat, lng: defenderUnit.lng };
const waypoint = {
	lat: (staging.lat + target.lat) / 2,
	lng: (staging.lng + target.lng) / 2,
};
const startedFrame = Math.max(0, base.frame - 10);
const progressFrame = Math.max(startedFrame, base.frame - 2);
const invasionSignature = `wire:invasion:${target.lat}:${target.lng}`;
const supplySignature = `wire:supply:${target.lat}:${target.lng}`;
const transportSignature = `wire:transport:${target.lat}:${target.lng}`;

for (const unit of attackerUnits.slice(0, 3)) unit.navalAssigned = true;
attackerUnits[3].supplyAssigned = true;
attackerUnits[4]._transportPlanSignature = transportSignature;
defenderUnit._defenderReactTarget = { ...target };

const navalPlans = Array(base.sides.length).fill(null);
const supplyPlans = Array(base.sides.length).fill(null);
const transportPlans = Array(base.sides.length).fill(null);
const defenderReactionPlans = Array(base.sides.length).fill(null);
navalPlans[0] = {
	type: "NAVAL_INVASION",
	signature: invasionSignature,
	phase: "TRANSIT",
	target: { ...target },
	targetCountryId: base.sides[1].countryIds[0],
	targetSideUid: sideUids[1],
	stagingPoint: { ...staging },
	arrowPoints: [{ ...staging }, waypoint, { ...target }],
	maxAssignedUnits: 5,
	progress: 0.55,
	startedTick: startedFrame,
	lastProgressTick: progressFrame,
};
supplyPlans[0] = {
	type: "NAVAL_SUPPLY",
	signature: supplySignature,
	phase: "TRANSIT",
	target: { ...target },
	stagingPoint: { ...staging },
	arrowPoints: [{ ...staging }, { ...target }],
	maxAssignedUnits: 3,
	progress: 0.5,
	startedTick: startedFrame,
	lastProgressTick: progressFrame,
};
transportPlans[0] = {
	type: "TRANSPORT",
	signature: transportSignature,
	phase: "EXECUTION",
	target: { ...target },
	arrowPoints: [{ ...staging }, { ...target }],
	maxAssignedUnits: 5,
	progress: 0.25,
	startedTick: startedFrame,
	lastProgressTick: progressFrame,
};
defenderReactionPlans[1] = {
	type: "DEFEND",
	target: { ...target },
	enemySideIdx: 0,
	phase: "EXECUTION",
	maxUnits: 3,
	activeUnitCount: 1,
	startedTick: startedFrame,
	lastProgressTick: progressFrame,
	_landingDefeatedTick: 0,
};

const browserAirfieldId = new Map(
	source.airPower.airfields.map((field) => [
		field.id,
		`browser-airfield-${field.id}`,
	]),
);
const airfields = source.airPower.airfields.map((field) => ({
	id: browserAirfieldId.get(field.id),
	lat: field.lat,
	lng: field.lng,
	ownerId: field.ownerCountryId,
	controllerId: field.controllerCountryId,
	sideIndex: field.side,
	isCapital: field.capital,
	health: field.health,
	disabled: field.disabled,
	captureRepairCycles: field.captureRepairCycles,
}));
const browserAirWingId = new Map(
	source.airPower.wings.map((wing) => [wing.id, `browser-air-wing-${wing.id}`]),
);
const airWings = source.airPower.wings.map((wing) => {
	let targetId = null;
	if (wing.targetKind === "AIR_WING") {
		targetId = browserAirWingId.get(wing.targetId) ?? null;
	} else if (wing.targetKind === "AIRFIELD") {
		targetId = browserAirfieldId.get(wing.targetId) ?? null;
	} else if (wing.targetKind === "ARMY" || wing.targetKind === "ARMOR") {
		targetId = browserIdByNativeUnit.get(wing.targetId) ?? null;
	}
	return {
		id: browserAirWingId.get(wing.id),
		role: wing.role,
		sovereignId: wing.sovereignCountryId,
		sideIndex: wing.side,
		equipment: wing.count,
		maxEquipment: wing.maxCount,
		quality: wing.quality,
		airfieldId: browserAirfieldId.get(wing.airfieldId),
		returnFieldId:
			wing.returnAirfieldId == null
				? null
				: browserAirfieldId.get(wing.returnAirfieldId),
		lat: wing.lat,
		lng: wing.lng,
		state: wing.state,
		targetId,
		targetType: wing.targetKind,
		cooldownTicks: wing.cooldownTicks,
		rearmTicks: wing.rearmTicks,
		enduranceTicks: wing.enduranceTicks,
		nextMissionTick: wing.nextMissionTick,
		forceMission: wing.forceMission,
	};
});
const countryEquipment = new Map(
	source.airPower.countryCoverage.map((coverage) => [
		coverage.countryId,
		{ airOperationsCoverage: coverage.operationsCoverage },
	]),
);
const underfundedCountryId = Math.min(
	...base.economies.map((economy) => economy.countryId),
);
countryEquipment.get(underfundedCountryId).airOperationsCoverage = 0.4;

const exporter = Function(
	"buildMidWarNativeRuntimeCheckpointV5",
	"nativeRuntimeStableTopology",
	"units",
	"getLiveFormationStrength",
	"sides",
	"sideUids",
	"_navalPlan",
	"_navalSupplyPlan",
	"_transportPlan",
	"_defenderReactionPlan",
	"countryEquipment",
	"airfields",
	"airWings",
	"NATIVE_RUNTIME_CHECKPOINT_V6_SCHEMA",
	"NATIVE_OPERATIONAL_EXECUTION_SCHEMA",
	"NATIVE_AIR_POWER_SCHEMA",
	`"use strict";\n${main.slice(start, end)}\nreturn buildMidWarNativeRuntimeCheckpointV6;`,
)(
	(options = {}) => ({
		...structuredClone(base),
		steps: options.steps ?? base.steps,
	}),
	() => topology,
	units,
	(unit) => unit.formationStrength,
	sides,
	sideUids,
	navalPlans,
	supplyPlans,
	transportPlans,
	defenderReactionPlans,
	countryEquipment,
	airfields,
	airWings,
	"native-runtime-checkpoint-v6",
	"native-operational-execution-v1",
	"native-air-v2",
);

const first = exporter({ steps: 1 });
const second = exporter({ steps: 1 });
assert.deepEqual(first, second, "browser v6 production export is deterministic");
assert.equal(first.schema, "native-runtime-checkpoint-v6");
assert.equal(
	first.operationalExecution.schema,
	"native-operational-execution-v1",
);
assert.equal(first.airPower.schema, "native-air-v2");
assert.equal(first.airPower.countryCoverage.length, base.economies.length);
assert.equal(
	first.airPower.countryCoverage.find(
		(coverage) => coverage.countryId === underfundedCountryId,
	)?.operationsCoverage,
	0.4,
);
assert.deepEqual(
	first.airPower.countryCoverage.map((coverage) => coverage.countryId),
	[...first.airPower.countryCoverage]
		.sort((left, right) => left.countryId - right.countryId)
		.map((coverage) => coverage.countryId),
);
assert.deepEqual(
	first.operationalExecution.navalOperations.map((operation) => operation.kind),
	["INVASION", "SUPPLY", "FAST_TRANSPORT"],
);
assert.equal(first.operationalExecution.defenderReactions.length, 1);
assert.equal(
	first.operationalExecution.defenderReactions[0].threatSignature,
	invasionSignature,
);
assert.equal(first.airPower.airfields.length, source.airPower.airfields.length);
assert.equal(first.airPower.wings.length, source.airPower.wings.length);
for (const operation of first.operationalExecution.navalOperations) {
	assert.ok(Object.hasOwn(operation, "enemySide"));
	assert.ok(Object.hasOwn(operation, "completionReason"));
}
for (const reaction of first.operationalExecution.defenderReactions) {
	assert.ok(Object.hasOwn(reaction, "bestDistanceSquared"));
	assert.ok(Object.hasOwn(reaction, "landingDefeatedTick"));
}
for (const wing of first.airPower.wings) {
	for (const key of [
		"returnAirfieldId",
		"targetKind",
		"targetId",
		"nextMissionTick",
	]) {
		assert.ok(Object.hasOwn(wing, key), `air wing includes nullable ${key}`);
	}
}

const savedNavalPlan = navalPlans[0];
const savedTaskForces = base.operationalAi.taskForces;
navalPlans[0] = null;
base.operationalAi.taskForces = [
	{
		id: "wire-same-target-not-attacking",
		sideIndex: 0,
		phase: "ASSEMBLING",
		target: { ...target },
		planSignature: "wire-same-target-land-plan",
	},
];
const nonAttacking = exporter({ steps: 1 });
assert.deepEqual(
	nonAttacking.operationalExecution.defenderReactions,
	[],
	"same-target non-attacking task force does not continue a land reaction",
);
navalPlans[0] = savedNavalPlan;
base.operationalAi.taskForces = savedTaskForces;

writeFileSync(outputPath, `${JSON.stringify(first)}\n`);
console.log("actual browser v6 execution and air-power exporter wire contract ok");
