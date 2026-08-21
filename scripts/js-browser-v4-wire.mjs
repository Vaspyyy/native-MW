#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [webRoot, checkpointPath, outputPath] = process.argv.slice(2);
if (!webRoot || !checkpointPath || !outputPath) {
	throw new Error(
		"usage: js-browser-v4-wire.mjs <modern-wars-root> <v4-checkpoint> <output>",
	);
}

const main = readFileSync(join(webRoot, "src/main.js"), "utf8");
const start = main.indexOf("function nativeRuntimeV4SideDynamics");
const end = main.indexOf("function nativeRuntimeV4BaseAiSpeedMultiplier", start);
assert.ok(start >= 0 && end > start, "browser v4 side-dynamics exporter exists");

const checkpoint = JSON.parse(readFileSync(checkpointPath, "utf8"));
assert.equal(checkpoint.schema, "native-runtime-checkpoint-v4");
assert.equal(checkpoint.sideDynamics?.schema, "native-side-dynamics-v1");
assert.equal(checkpoint.sideDynamics.sides.length, checkpoint.sides.length);

const browserSides = checkpoint.sides.map((side) =>
	side.countryIds.map((id) => ({ id })),
);
const aiCountryState = new Map();
if (browserSides[0]?.[0]) {
	aiCountryState.set(browserSides[0][0].id, { mode: "LAST_STAND" });
}
if (browserSides[1]?.[0]) {
	aiCountryState.set(browserSides[1][0].id, {
		mode: "OFFENSIVE_DESPERATION",
	});
}
const aiPosture = {
	LAST_STAND: "LAST_STAND",
	OFFENSIVE_DESPERATION: "OFFENSIVE_DESPERATION",
};
const defenderReactionPlan = checkpoint.sides.map((_, index) => index === 2);
const sourceSides = checkpoint.sideDynamics.sides;
const exporter = Function(
	"sideUids",
	"_retiredSidePersonnelByUid",
	"sideSoldiers",
	"initialSideSoldiers",
	"_sideMomentumHistory",
	"simFrameCount",
	"_sideWarPhase",
	"_sidePosture",
	"NATIVE_SIDE_DYNAMICS_SCHEMA",
	"sides",
	"aiCountryState",
	"AI_POSTURE",
	"_defenderReactionPlan",
	`"use strict";\n${main.slice(start, end)}\nreturn nativeRuntimeV4SideDynamics;`,
)(
	checkpoint.sides.map((_, index) => `side-${index + 1}`),
	new Map(),
	Float64Array.from(sourceSides, (side) => side.personnel),
	Float64Array.from(sourceSides, (side) => side.initialPersonnel),
	sourceSides.map((side) =>
		side.momentumHistory.map((sample) => ({
			tick: sample.frame,
			controlled: sample.controlled,
		})),
	),
	checkpoint.frame,
	sourceSides.map((side) => side.warPhase),
	sourceSides.map((side) => side.posture),
	"native-side-dynamics-v1",
	browserSides,
	aiCountryState,
	aiPosture,
	defenderReactionPlan,
);

checkpoint.sideDynamics = exporter({
	stable: checkpoint.sides.map((_, browserSideIndex) => ({ browserSideIndex })),
	browserToNativeSide: new Map(
		checkpoint.sides.map((_, sideIndex) => [sideIndex, sideIndex]),
	),
});
if (sourceSides.length > 0) {
	assert.equal(checkpoint.sideDynamics.sides[0].postureOverride, "DEFENSIVE");
}
if (sourceSides.length > 1) {
	assert.equal(checkpoint.sideDynamics.sides[1].postureOverride, "OFFENSIVE");
}
writeFileSync(outputPath, `${JSON.stringify(checkpoint)}\n`);
