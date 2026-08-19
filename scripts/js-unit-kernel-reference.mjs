#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const COMBAT_DAMAGE = 0.7;
const UNIT_SPEED = 0.003;
const UNIT_NAVAL_SPEED = 0.025;
const UNIT_HEALTH = 100;
const UNIT_TO_SOLDIER_RATIO = 1000;
const ARMOR_CREW_PER_VEHICLE = 2;

function gridIndex(grid, lat, lng) {
	const wrappedLng = ((((lng + 180) % 360) + 360) % 360) - 180;
	const x = Math.floor((wrappedLng + 180) / grid.gridRes);
	const y = Math.floor((lat + 90) / grid.gridRes);
	if (x < 0 || x >= grid.width || y < 0 || y >= grid.height) return -1;
	return y * grid.width + x;
}

function movementCase(testCase) {
	const { grid, state, factors } = testCase;
	let lat = state.lat;
	let lng = state.lng;
	let moveDirLat = state.dirLat;
	let moveDirLng = state.dirLng;
	let coastStuckTicks = state.coastStuckTicks || 0;
	let moveDist =
		factors.baseSpeed *
		factors.speedMult *
		factors.planSpeedMult *
		factors.neutralPenalty *
		factors.retreatBoost *
		factors.pushReadiness *
		0.8;
	let coastBlocked = false;
	let abandonTarget = false;
	let coastDeflectHalved = false;

	if (
		!Number.isNaN(moveDirLat) &&
		!Number.isNaN(moveDirLng) &&
		!Number.isNaN(moveDist)
	) {
		if (!state.isTransport) {
			const destIdx = gridIndex(
				grid,
				lat + moveDirLat * moveDist,
				lng + moveDirLng * moveDist,
			);
			if (destIdx !== -1 && grid.landMask[destIdx] === 0 && !state.isAtSea) {
				coastBlocked = true;
				let deflected = false;
				const lookDist = moveDist * 3;
				for (const angle of [-90, 90, -45, 45, -135, 135, -30, 30]) {
					const radians = (angle * Math.PI) / 180;
					const candidateLat =
						moveDirLat * Math.cos(radians) - moveDirLng * Math.sin(radians);
					const candidateLng =
						moveDirLat * Math.sin(radians) + moveDirLng * Math.cos(radians);
					let landCount = 0;
					for (let sample = 1; sample <= 3; sample++) {
						const candidateIndex = gridIndex(
							grid,
							lat + candidateLat * lookDist * sample,
							lng + candidateLng * lookDist * sample,
						);
						if (candidateIndex !== -1 && grid.landMask[candidateIndex] > 0) {
							landCount++;
						}
					}
					if (landCount >= 2) {
						const magnitude = Math.sqrt(
							candidateLat * candidateLat + candidateLng * candidateLng,
						);
						if (magnitude > 0) {
							moveDirLat = candidateLat / magnitude;
							moveDirLng = candidateLng / magnitude;
						}
						const deflectedDestination = gridIndex(
							grid,
							lat + moveDirLat * moveDist,
							lng + moveDirLng * moveDist,
						);
						if (
							deflectedDestination !== -1 &&
							grid.landMask[deflectedDestination] === 0
						) {
							moveDist *= 0.5;
							coastDeflectHalved = true;
						}
						deflected = true;
						break;
					}
				}
				if (!deflected) {
					moveDirLat = 0;
					moveDirLng = 0;
					moveDist = 0;
				}
			}
		}

		if (coastBlocked) {
			coastStuckTicks = (coastStuckTicks || 0) + 1;
			if (coastStuckTicks > 60) {
				abandonTarget = true;
				coastStuckTicks = 0;
			}
		} else {
			coastStuckTicks = 0;
		}

		lat += moveDirLat * moveDist;
		lng += moveDirLng * moveDist;
		lat = Math.max(-89.9, Math.min(89.9, lat));
		if (lng > 180) lng -= 360;
		if (lng < -180) lng += 360;
	}

	return {
		lat,
		lng,
		dir_lat: moveDirLat,
		dir_lng: moveDirLng,
		move_dist: moveDist,
		coast_blocked: coastBlocked,
		coast_stuck_ticks: coastStuckTicks,
		abandon_target: abandonTarget,
		coast_deflect_halved: coastDeflectHalved,
	};
}

function livePersonnel(unit) {
	if (unit.personnel !== undefined) return Math.round(unit.personnel);
	const nominal = unit.nominalPersonnel ?? UNIT_TO_SOLDIER_RATIO;
	if (unit.strengthMultiplier !== undefined) {
		return Math.round(nominal * unit.strengthMultiplier);
	}
	const baseHealth = unit.baseHealth ?? unit.maxHealth ?? UNIT_HEALTH;
	if (unit.health !== undefined) return Math.round((unit.health / baseHealth) * nominal);
	return nominal;
}

function liveStrength(unit) {
	return unit.kind === "armor" ? 1 : livePersonnel(unit) / UNIT_TO_SOLDIER_RATIO;
}

function applyDamage(target, damage) {
	if (
		!target ||
		!Number.isFinite(damage) ||
		damage <= 0 ||
		!Number.isFinite(target.health) ||
		target.health <= 0
	) {
		return { effectiveDamage: 0, personnelLoss: 0 };
	}
	const effectiveDamage = Math.min(target.health, damage);
	let personnelLoss = 0;
	if (target.kind === "armor") {
		const beforeEquipment = Math.max(0, target.equipment || 0);
		const nextHealth = Math.max(0, target.health - effectiveDamage);
		const nextEquipment = Math.min(
			beforeEquipment,
			Math.max(
				0,
				Math.ceil(
					(target.maxEquipment || beforeEquipment) * (nextHealth / UNIT_HEALTH),
				),
			),
		);
		personnelLoss = (beforeEquipment - nextEquipment) * ARMOR_CREW_PER_VEHICLE;
		target.equipment = nextEquipment;
	} else {
		const beforePersonnel = livePersonnel(target);
		const maxHealth = Math.max(1, target.maxHealth || UNIT_HEALTH);
		const personnelCapacity = Math.max(
			beforePersonnel,
			target.personnelCapacity || beforePersonnel,
		);
		const nextHealth = Math.max(0, target.health - effectiveDamage);
		const nextPersonnel = Math.min(
			beforePersonnel,
			Math.max(0, Math.round(personnelCapacity * (nextHealth / maxHealth))),
		);
		personnelLoss = beforePersonnel - nextPersonnel;
		target.personnel = nextPersonnel;
		target.strengthMultiplier = target.personnel / UNIT_TO_SOLDIER_RATIO;
		if (target.personnel <= 0) target.health = 0;
	}
	// The browser writes health after the personnel-zero assignment too.
	target.health = Math.max(0, target.health - effectiveDamage);
	return { effectiveDamage, personnelLoss };
}

function shortestLongitudeDelta(from, to) {
	let delta = to - from;
	if (delta > 180) delta -= 360;
	else if (delta < -180) delta += 360;
	return delta;
}

function combinedDamage(base, attacker, target, context, helpers) {
	const landing =
		attacker.kind === "armor" &&
		(attacker.armorLandingPenaltyUntilTick || 0) > context.simTick
			? 0.3
			: 1;
	return (
		base *
		helpers.getArmorCombatMultiplier(attacker.kind, target.kind, {
			mountain: context.mountain,
			urban: context.urban,
			supported: !!attacker.armorSupported,
		}) *
		(attacker.kind === "armor" ? helpers.getQualityMultiplier(attacker.quality) : 1) *
		landing *
		(attacker.kind === "army" ? Math.max(0, liveStrength(attacker)) : 1)
	);
}

function pushUnit(unit, dLat, dLng, grid) {
	let lat = Math.max(-89.9, Math.min(89.9, unit.lat + dLat));
	let lng = unit.lng + dLng;
	if (lng > 180) lng -= 360;
	else if (lng < -180) lng += 360;
	const index = grid ? gridIndex(grid, lat, lng) : -1;
	if (grid && index !== -1 && grid.landMask[index] === 0 && !unit.isAtSea) return true;
	unit.lat = lat;
	unit.lng = lng;
	return false;
}

function makeEvent(layer, attacker, target) {
	return {
		layer,
		attacker_id: attacker.id,
		target_id: target.id,
		target_damage: 0,
		self_damage: 0,
		target_personnel_loss: 0,
		self_personnel_loss: 0,
		target_health: target.health,
		self_health: attacker.health,
		target_knockback_blocked: false,
		self_knockback_blocked: false,
	};
}

function proximityOperation(attacker, target, testCase, helpers) {
	const context = testCase.context;
	const dLat = attacker.lat - target.lat;
	const dLng = shortestLongitudeDelta(attacker.lng, target.lng);
	const distanceSquared = dLat * dLat + dLng * dLng;
	if (distanceSquared >= 0.09 || context.simFrame < context.warGraceEnd) return null;

	const event = makeEvent("proximity", attacker, target);
	let proximityDamage =
		COMBAT_DAMAGE *
		0.45 *
		context.damageDealtMult *
		(1 - Math.sqrt(distanceSquared) / 0.3);
	if (attacker.isAtSea && target.isAtSea) proximityDamage *= 2.2;
	if (target.isTransport && !attacker.isTransport) proximityDamage *= 1.05;
	if (attacker.isTransport && !target.isTransport) {
		const transportSelfDamage = proximityDamage * 1.05 * context.damageTakenMult;
		const result = applyDamage(attacker, transportSelfDamage);
		event.self_damage += transportSelfDamage;
		event.self_personnel_loss += result.personnelLoss;
		proximityDamage *= 0.85;
	}

	const targetDamage = combinedDamage(proximityDamage, attacker, target, context, helpers);
	const targetResult = applyDamage(target, targetDamage);
	event.target_damage += targetDamage;
	event.target_personnel_loss += targetResult.personnelLoss;
	const selfDamage = combinedDamage(
		proximityDamage * 0.8 * context.damageTakenMult,
		target,
		attacker,
		context,
		helpers,
	);
	const selfResult = applyDamage(attacker, selfDamage);
	event.self_damage += selfDamage;
	event.self_personnel_loss += selfResult.personnelLoss;
	attacker.lastCombatTick = context.simFrame;
	target.lastCombatTick = context.simFrame;
	if (target.health <= 0) attacker.victoryBoostTicks = 240;
	event.target_health = target.health;
	event.self_health = attacker.health;
	return event;
}

function directOperation(attacker, target, testCase, helpers) {
	const context = testCase.context;
	const jitterLat = target.lat + Math.sin(attacker.id * 100) * 0.08;
	const jitterLng = target.lng + Math.cos(attacker.id * 100) * 0.08;
	const gateLat = jitterLat - attacker.lat;
	const gateLng = shortestLongitudeDelta(attacker.lng, jitterLng);
	if (Math.sqrt(gateLat * gateLat + gateLng * gateLng) > 0.05) return null;

	const event = makeEvent("direct", attacker, target);
	attacker.lastCombatTick = context.simFrame;
	target.lastCombatTick = context.simFrame;
	const attackerLanding =
		attacker.kind === "armor" &&
		(attacker.armorLandingPenaltyUntilTick || 0) > context.simTick
			? 0.3
			: 1;
	const defenderLanding =
		target.kind === "armor" &&
		(target.armorLandingPenaltyUntilTick || 0) > context.simTick
			? 0.3
			: 1;
	const terrain = { mountain: context.mountain, urban: context.urban };
	const targetDamage =
		COMBAT_DAMAGE *
		context.damageDealtMult *
		0.7 *
		helpers.getArmorCombatMultiplier(attacker.kind, target.kind, {
			...terrain,
			supported: !!attacker.armorSupported,
		}) *
		(attacker.kind === "armor" ? helpers.getQualityMultiplier(attacker.quality) : 1) *
		attackerLanding *
		(attacker.kind === "army" ? Math.max(0, liveStrength(attacker)) : 1);
	const selfDamage =
		COMBAT_DAMAGE *
		0.8 *
		context.damageTakenMult *
		context.defenseBonus *
		context.longWarDefense *
		helpers.getArmorCombatMultiplier(target.kind, attacker.kind, {
			...terrain,
			supported: !!target.armorSupported,
		}) *
		(target.kind === "armor" ? helpers.getQualityMultiplier(target.quality) : 1) *
		defenderLanding *
		(target.kind === "army" ? Math.max(0, liveStrength(target)) : 1);
	const targetResult = applyDamage(target, targetDamage);
	const selfResult = applyDamage(attacker, selfDamage);
	event.target_damage = targetDamage;
	event.self_damage = selfDamage;
	event.target_personnel_loss = targetResult.personnelLoss;
	event.self_personnel_loss = selfResult.personnelLoss;

	const deltaLat = target.lat - attacker.lat;
	const deltaLng = shortestLongitudeDelta(attacker.lng, target.lng);
	const distanceSquared = deltaLat * deltaLat + deltaLng * deltaLng;
	if (distanceSquared > 0) {
		const distance = Math.sqrt(distanceSquared) || 1e-6;
		const nx = deltaLng / distance;
		const ny = deltaLat / distance;
		const basePush = (attacker.isAtSea ? UNIT_NAVAL_SPEED : UNIT_SPEED) * 1.2;
		const totalDamage = targetDamage + selfDamage || 1e-6;
		const targetFactor = Math.min(1.5, (targetDamage / totalDamage) * 1.5);
		const selfFactor = Math.min(1, selfDamage / totalDamage);
		event.target_knockback_blocked = pushUnit(
			target,
			ny * basePush * targetFactor,
			nx * basePush * targetFactor,
			testCase.grid,
		);
		event.self_knockback_blocked = pushUnit(
			attacker,
			-ny * basePush * 0.5 * selfFactor,
			-nx * basePush * 0.5 * selfFactor,
			testCase.grid,
		);
	}
	if (target.health <= 0) attacker.victoryBoostTicks = 180;
	event.target_health = target.health;
	event.self_health = attacker.health;
	return event;
}

function reportUnit(unit) {
	return {
		id: unit.id,
		lat: unit.lat,
		lng: unit.lng,
		health: unit.health,
		personnel: unit.personnel ?? null,
		strength_multiplier: unit.strengthMultiplier ?? null,
		equipment: unit.equipment ?? null,
		victory_boost_ticks: unit.victoryBoostTicks || 0,
		last_combat_tick: unit.lastCombatTick || 0,
	};
}

function combatCase(testCase, helpers) {
	const units = testCase.units.map((unit) => structuredClone(unit));
	const byId = new Map(units.map((unit) => [unit.id, unit]));
	const events = testCase.operations.map((operation) => {
		const attacker = byId.get(operation.attackerId);
		const target = byId.get(operation.targetId);
		if (!attacker || !target) return null;
		return operation.layer === "proximity"
			? proximityOperation(attacker, target, testCase, helpers)
			: directOperation(attacker, target, testCase, helpers);
	});
	return {
		name: testCase.name,
		events,
		units: units.sort((a, b) => a.id - b.id).map(reportUnit),
	};
}

function movementChecksum(fixture) {
	let checksum = 0;
	for (const testCase of fixture.movementCases) {
		const output = movementCase(testCase);
		checksum +=
			output.lat * 0.5 +
			output.lng * 0.25 +
			output.dir_lat * 0.125 +
			output.dir_lng * 0.0625 +
			output.move_dist * 0.03125 +
			(output.coast_blocked ? 3 : 0) +
			output.coast_stuck_ticks * 0.01 +
			(output.abandon_target ? 5 : 0) +
			(output.coast_deflect_halved ? 7 : 0);
	}
	return checksum;
}

function combatChecksum(fixture, helpers) {
	let checksum = 0;
	for (const testCase of fixture.combatCases) {
		const result = combatCase(testCase, helpers);
		for (const event of result.events) {
			if (event === null) {
				checksum += 0.125;
				continue;
			}
			checksum +=
				event.attacker_id * 1e-7 +
				event.target_id * 2e-7 +
				event.target_damage * 0.5 +
				event.self_damage * 0.25 +
				event.target_personnel_loss * 0.01 +
				event.self_personnel_loss * 0.02 +
				event.target_health * 0.001 +
				event.self_health * 0.002 +
				(event.target_knockback_blocked ? 3 : 0) +
				(event.self_knockback_blocked ? 5 : 0);
		}
		for (const unit of result.units) {
			checksum +=
				unit.id * 1e-8 +
				unit.lat * 1e-5 +
				unit.lng * 2e-5 +
				unit.health * 3e-5 +
				(unit.personnel ?? 0) * 1e-6 +
				(unit.equipment ?? 0) * 2e-6 +
				unit.victory_boost_ticks * 3e-7 +
				unit.last_combat_tick * 4e-8;
		}
	}
	return checksum;
}

function percentile(samples, percentileValue) {
	const sorted = [...samples].sort((a, b) => a - b);
	const index = Math.max(0, Math.ceil(sorted.length * percentileValue) - 1);
	return sorted[index];
}

function timingSummary(samples) {
	return {
		median: percentile(samples, 0.5),
		p95: percentile(samples, 0.95),
	};
}

function parsePositiveInteger(value, name, fallback) {
	if (value === undefined) return fallback;
	const parsed = Number(value);
	if (!Number.isSafeInteger(parsed) || parsed <= 0) {
		throw new Error(`${name} must be a positive integer`);
	}
	return parsed;
}

function benchmark(fixture, helpers, repeat, warmup) {
	const expectedMovement = movementChecksum(fixture);
	const expectedCombat = combatChecksum(fixture, helpers);
	if (!Number.isFinite(expectedMovement) || !Number.isFinite(expectedCombat)) {
		throw new Error("benchmark checksum is not finite");
	}
	// Untimed second pass catches accidental state retention before measurement.
	if (
		movementChecksum(fixture) !== expectedMovement ||
		combatChecksum(fixture, helpers) !== expectedCombat
	) {
		throw new Error("benchmark workload is not deterministic from fresh state");
	}

	let sink = 0;
	for (let iteration = 0; iteration < warmup; iteration++) {
		sink += movementChecksum(fixture);
		sink += combatChecksum(fixture, helpers);
	}
	const movementSamples = [];
	for (let iteration = 0; iteration < repeat; iteration++) {
		const started = performance.now();
		sink += movementChecksum(fixture);
		movementSamples.push(performance.now() - started);
	}
	const combatSamples = [];
	for (let iteration = 0; iteration < repeat; iteration++) {
		const started = performance.now();
		sink += combatChecksum(fixture, helpers);
		combatSamples.push(performance.now() - started);
	}
	if (!Number.isFinite(sink)) throw new Error("benchmark sink is not finite");
	return {
		schema_version: fixture.schema,
		movement_cases: fixture.movementCases.length,
		combat_cases: fixture.combatCases.length,
		operations: fixture.combatCases.reduce(
			(total, testCase) => total + testCase.operations.length,
			0,
		),
		repeat,
		warmup,
		movement_ms: timingSummary(movementSamples),
		combat_ms: timingSummary(combatSamples),
		checksum: expectedMovement + expectedCombat,
	};
}

async function main() {
	const [command, webRoot, fixturePath, repeatArgument, warmupArgument] =
		process.argv.slice(2);
	if (!new Set(["report", "bench"]).has(command) || !webRoot || !fixturePath) {
		throw new Error(
			"usage: node scripts/js-unit-kernel-reference.mjs <report|bench> <web-root> <fixture> [repeat=50] [warmup=10]",
		);
	}
	const combinedArmsUrl = pathToFileURL(
		path.join(path.resolve(webRoot), "src", "combined-arms.js"),
	).href;
	const { getArmorCombatMultiplier, getQualityMultiplier } = await import(combinedArmsUrl);
	const fixture = JSON.parse(await readFile(fixturePath, "utf8"));
	if (fixture.schema !== "movement-combat-v1") {
		throw new Error(`unsupported fixture schema: ${fixture.schema}`);
	}
	const helpers = { getArmorCombatMultiplier, getQualityMultiplier };
	if (command === "bench") {
		const repeat = parsePositiveInteger(repeatArgument, "repeat", 50);
		const warmup = parsePositiveInteger(warmupArgument, "warmup", 10);
		process.stdout.write(`${JSON.stringify(benchmark(fixture, helpers, repeat, warmup))}\n`);
		return;
	}
	const report = {
		schema_version: fixture.schema,
		movement_cases: fixture.movementCases.map((testCase) => ({
			name: testCase.name,
			output: movementCase(testCase),
		})),
		combat_cases: fixture.combatCases.map((testCase) => combatCase(testCase, helpers)),
	};
	process.stdout.write(`${JSON.stringify(report)}\n`);
}

await main();
