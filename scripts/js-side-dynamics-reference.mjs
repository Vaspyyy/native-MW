#!/usr/bin/env node
import assert from "node:assert/strict";

const PHASES = ["ADVANCING", "STALEMATE", "RETREATING", "COLLAPSING"];
const POSTURES = ["OFFENSIVE", "BALANCED", "DEFENSIVE"];

export function momentumSampleDue(tick) {
	return tick >= 37 && (tick - 37) % 200 === 0;
}

export function phaseFromHistory(history, personnel, initialPersonnel) {
	if (history.length < 3) return "STALEMATE";
	const first = history[0].controlled;
	const last = history.at(-1).controlled;
	const deltaRatio = first > 0 ? (last - first) / first : 0;
	let trendUp = 0;
	let trendDown = 0;
	for (let index = Math.max(0, history.length - 3); index < history.length; index++) {
		if (index === 0) continue;
		if (history[index].controlled > history[index - 1].controlled) trendUp++;
		else if (history[index].controlled < history[index - 1].controlled) trendDown++;
	}
	const manpowerRatio = initialPersonnel > 0 ? personnel / initialPersonnel : 1;
	if (deltaRatio < -0.05 || manpowerRatio < 0.1) return "COLLAPSING";
	if (deltaRatio < -0.005 || trendDown >= 2) return "RETREATING";
	if (deltaRatio > 0.005 || trendUp >= 2) return "ADVANCING";
	return "STALEMATE";
}

export function postureFromStrength({ strength, enemyStrength, manpower, initialManpower, lastStand = false, offensiveDesperation = false, defenderReaction = false }) {
	let posture = "BALANCED";
	if (defenderReaction || lastStand) posture = "DEFENSIVE";
	else if (offensiveDesperation) posture = "OFFENSIVE";
	else if (enemyStrength > 0) {
		const ratio = strength / Math.max(1, enemyStrength);
		if (ratio > 1.5) posture = "OFFENSIVE";
		else if (ratio < 0.7) posture = "DEFENSIVE";
	}
	if (initialManpower > 0 && manpower / initialManpower < 0.15) posture = "DEFENSIVE";
	return posture;
}

function run() {
	const cadence = [
		{ tick: 36, counted: momentumSampleDue(36) },
		{ tick: 37, counted: momentumSampleDue(37) },
		{ tick: 236, counted: momentumSampleDue(236) },
		{ tick: 237, counted: momentumSampleDue(237) },
	];
	const cases = {
		cadence,
		shortHistory: phaseFromHistory([{ controlled: 10 }, { controlled: 11 }], 100, 100),
		advancing: phaseFromHistory([{ controlled: 100 }, { controlled: 101 }, { controlled: 102 }], 100, 100),
		suffixEdgeAdvancing: phaseFromHistory(
			[
				{ controlled: 100 },
				{ controlled: 101 },
				{ controlled: 102 },
				{ controlled: 102 },
			],
			100,
			100,
		),
		retreating: phaseFromHistory([{ controlled: 100 }, { controlled: 100 }, { controlled: 99 }], 100, 100),
		collapsingBySlope: phaseFromHistory([{ controlled: 100 }, { controlled: 95 }, { controlled: 94 }], 100, 100),
		collapsingByManpower: phaseFromHistory([{ controlled: 100 }, { controlled: 100 }, { controlled: 100 }], 9, 100),
		offensive: postureFromStrength({ strength: 151, enemyStrength: 100, manpower: 100, initialManpower: 100 }),
		balancedAtOffensiveBoundary: postureFromStrength({ strength: 150, enemyStrength: 100, manpower: 100, initialManpower: 100 }),
		defensive: postureFromStrength({ strength: 69, enemyStrength: 100, manpower: 100, initialManpower: 100 }),
		balancedAtDefensiveBoundary: postureFromStrength({ strength: 70, enemyStrength: 100, manpower: 100, initialManpower: 100 }),
		defensiveByManpower: postureFromStrength({ strength: 200, enemyStrength: 100, manpower: 14, initialManpower: 100 }),
		lastStandOverride: postureFromStrength({ strength: 200, enemyStrength: 100, manpower: 100, initialManpower: 100, lastStand: true }),
		offensiveDesperationOverride: postureFromStrength({ strength: 1, enemyStrength: 100, manpower: 100, initialManpower: 100, offensiveDesperation: true }),
		defenderReactionOverride: postureFromStrength({ strength: 200, enemyStrength: 100, manpower: 100, initialManpower: 100, defenderReaction: true }),
		manpowerBeatsOffensiveOverride: postureFromStrength({ strength: 200, enemyStrength: 100, manpower: 14, initialManpower: 100, offensiveDesperation: true }),
	};
	assert.equal(cases.shortHistory, "STALEMATE");
	assert.deepEqual(
		cadence.map(({ counted }) => counted),
		[false, true, false, true],
	);
	assert.equal(cases.advancing, "ADVANCING");
	assert.equal(cases.suffixEdgeAdvancing, "ADVANCING");
	assert.equal(cases.retreating, "RETREATING");
	assert.equal(cases.collapsingBySlope, "COLLAPSING");
	assert.equal(cases.collapsingByManpower, "COLLAPSING");
	assert.equal(cases.offensive, "OFFENSIVE");
	assert.equal(cases.balancedAtOffensiveBoundary, "BALANCED");
	assert.equal(cases.defensive, "DEFENSIVE");
	assert.equal(cases.balancedAtDefensiveBoundary, "BALANCED");
	assert.equal(cases.defensiveByManpower, "DEFENSIVE");
	assert.equal(cases.lastStandOverride, "DEFENSIVE");
	assert.equal(cases.offensiveDesperationOverride, "OFFENSIVE");
	assert.equal(cases.defenderReactionOverride, "DEFENSIVE");
	assert.equal(cases.manpowerBeatsOffensiveOverride, "DEFENSIVE");
	assert.ok(PHASES.includes(cases.advancing));
	assert.ok(POSTURES.includes(cases.offensive));
	console.log(JSON.stringify(cases));
}

if (import.meta.url === `file://${process.argv[1]}`) run();
