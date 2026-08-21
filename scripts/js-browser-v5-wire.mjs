#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [webRoot, checkpointPath, outputPath] = process.argv.slice(2);
if (!webRoot || !checkpointPath || !outputPath) {
	throw new Error(
		"usage: js-browser-v5-wire.mjs <modern-wars-root> <v5-checkpoint> <output>",
	);
}

const main = readFileSync(join(webRoot, "src/main.js"), "utf8");
const start = main.indexOf("function buildMidWarNativeRuntimeCheckpointV5");
const end = main.indexOf("function createNativeRuntimeCheckpoint", start);
assert.ok(start >= 0 && end > start, "browser v5 checkpoint exporter exists");

const source = JSON.parse(readFileSync(checkpointPath, "utf8"));
assert.equal(source.schema, "native-runtime-checkpoint-v5");
assert.equal(source.operationalAi?.schema, "native-operational-ai-v1");
const base = structuredClone(source);
base.schema = "native-runtime-checkpoint-v4";
delete base.operationalAi;

const sideUids = base.sides.map((_, side) => `side-${side}`);
const topology = {
	stable: base.sides.map((_, browserSideIndex) => ({ browserSideIndex })),
	browserToNativeSide: new Map(
		base.sides.map((_, sideIndex) => [sideIndex, sideIndex]),
	),
};
const browserIdByNative = new Map(
	base.units.map((unit) => [unit.id, `browser-unit-${unit.id}`]),
);
base.units.forEach((unit, index) => {
	assert.equal(unit.id, index + 1, "native save unit IDs remain dense and stable");
});
const units = base.units.map((unit) => ({
	id: browserIdByNative.get(unit.id),
	kind: unit.kind,
	health: unit.health,
	formationStrength: Math.max(1, unit.personnel),
}));
const sides = base.sides.map((side) =>
	side.countryIds.map((id) => {
		const state = source.operationalAi.countryDesperation.find(
			(country) => country.countryId === id,
		);
		return {
			id,
			_aiInitialCities: state?.initialCities ?? null,
			_aiInitialManpower: state?.initialManpower ?? null,
			_aiPrevControlled: state?.previousControlled ?? null,
			_aiStallTicks: state?.stallTicks ?? 0,
		};
	}),
);
const aiCountryState = new Map(
	source.operationalAi.countryDesperation.map((state) => [
		state.countryId,
		{ mode: state.mode },
	]),
);
if (sides[0]?.[0]) {
	aiCountryState.set(sides[0][0].id, { mode: "LAST_STAND" });
	base.sideDynamics.sides[0].postureOverride = "DEFENSIVE";
}

const intelBySide = new Map();
const prewarPower = new Map();
for (const side of source.operationalAi.sides) {
	const contacts = Object.fromEntries(
		side.intel.contacts.map((contact) => [
			contact.key,
			{
				...contact,
				unitId: browserIdByNative.get(contact.unitId),
				enemySideUid: sideUids[contact.enemySideIndex],
			},
		]),
	);
	intelBySide.set(sideUids[side.sideIndex], {
		lastScanTick: side.intel.lastScanTick,
		revision: side.intel.revision,
		config: side.intel.config,
		contacts,
	});
	for (const enemy of side.intel.prewarEnemyPower) {
		prewarPower.set(
			`${sideUids[side.sideIndex]}|${sideUids[enemy.sideIndex]}`,
			enemy.power,
		);
	}
}

const taskForcesBySide = new Map(sideUids.map((uid) => [uid, []]));
for (const force of source.operationalAi.taskForces) {
	const unitRoles = Object.fromEntries(
		force.members.map((member) => [
			browserIdByNative.get(member.unitId),
			{ role: member.role, assignedTick: member.assignedTick },
		]),
	);
	taskForcesBySide.get(sideUids[force.sideIndex]).push({
		...force,
		assignedUnitIds: force.members.map((member) =>
			browserIdByNative.get(member.unitId),
		),
		unitRoles,
		reserveUnitIds: force.reserveUnitIds.map((id) =>
			browserIdByNative.get(id),
		),
	});
}

if (source.operationalAi.taskForces.length === 0 && base.units.length > 0) {
	const first = base.units[0];
	const browserId = browserIdByNative.get(first.id);
	taskForcesBySide.get(sideUids[first.side]).push({
		id: "browser-wire-force-1",
		signature: "browser-wire-force-1",
		planSignature: "browser-wire-plan-1",
		planType: "FRONTLINE_PUSH",
		theaterId: null,
		target: { lat: first.lat, lng: first.lng },
		stagingAnchor: { lat: first.lat, lng: first.lng },
		route: [{ lat: first.lat, lng: first.lng }],
		phase: "ATTACKING",
		posture: "BALANCED",
		assignedUnitIds: [browserId],
		unitRoles: { [browserId]: { role: "SPEARHEAD", assignedTick: 0 } },
		reserveUnitIds: [],
		desiredPower: 1,
		launchPower: 1,
		currentPower: 1,
		peakPower: 1,
		readiness: 0.8,
		maxAssignedUnits: 1,
		createdTick: 0,
		phaseStartedTick: 0,
		lastProgressTick: 0,
		lastRecoveryTick: 0,
		recoveryPower: 0,
		progress: 0.2,
		withdrawalAnchor: null,
		completionReason: null,
		outcome: null,
		severeSurprise: false,
		parentTaskForceId: null,
		supplyInvalidatedTick: null,
		intentRevision: 0,
	});
}

const exporter = Function(
	"buildMidWarNativeRuntimeCheckpointV4",
	"nativeRuntimeStableTopology",
	"units",
	"getLiveFormationStrength",
	"sideUids",
	"_aiIntelBySide",
	"_aiPrewarEnemyPowerBySide",
	"areSidesHostile",
	"_aiTaskForcesBySide",
	"_simTickCount",
	"sides",
	"aiCountryState",
	"AI_POSTURE",
	"NATIVE_RUNTIME_CHECKPOINT_V5_SCHEMA",
	"NATIVE_OPERATIONAL_AI_SCHEMA",
	`"use strict";\n${main.slice(start, end)}\nreturn buildMidWarNativeRuntimeCheckpointV5;`,
)(
	() => structuredClone(base),
	() => topology,
	units,
	(unit) => unit.formationStrength,
	sideUids,
	intelBySide,
	prewarPower,
	(left, right) =>
		base.hostilityMatrix[left * base.sides.length + right] === 1,
	taskForcesBySide,
	base.tick,
	sides,
	aiCountryState,
	{
		LAST_STAND: "LAST_STAND",
		DEFENSIVE_DESPERATION: "DEFENSIVE_DESPERATION",
	},
	"native-runtime-checkpoint-v5",
	"native-operational-ai-v1",
);

const checkpoint = exporter({ steps: base.steps });
assert.equal(checkpoint.schema, "native-runtime-checkpoint-v5");
assert.equal(checkpoint.operationalAi.schema, "native-operational-ai-v1");
assert.equal(checkpoint.operationalAi.sides.length, checkpoint.sides.length);
assert.ok(checkpoint.operationalAi.taskForces.length > 0);
assert.ok(checkpoint.operationalAi.overrideEvents.length > 0);
assert.equal(checkpoint.operationalAi.sides[0].override.source, "LAST_STAND");
writeFileSync(outputPath, `${JSON.stringify(checkpoint)}\n`);
console.log("actual browser v5 operationalAi exporter wire contract ok");
