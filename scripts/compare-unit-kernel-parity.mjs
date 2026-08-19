#!/usr/bin/env node

import { readFile } from "node:fs/promises";

const TOLERANCE = 1e-10;

function compare(expected, actual, path = "$") {
	if (typeof expected === "number" && typeof actual === "number") {
		if (!Number.isFinite(expected) || !Number.isFinite(actual)) {
			throw new Error(`${path}: non-finite number`);
		}
		if (Math.abs(expected - actual) > TOLERANCE) {
			throw new Error(`${path}: expected ${expected}, got ${actual}`);
		}
		return;
	}
	if (expected === null || actual === null || typeof expected !== "object" || typeof actual !== "object") {
		if (expected !== actual) throw new Error(`${path}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
		return;
	}
	if (Array.isArray(expected) || Array.isArray(actual)) {
		if (!Array.isArray(expected) || !Array.isArray(actual)) {
			throw new Error(`${path}: array/object mismatch`);
		}
		if (expected.length !== actual.length) {
			throw new Error(`${path}: expected length ${expected.length}, got ${actual.length}`);
		}
		for (let index = 0; index < expected.length; index++) {
			compare(expected[index], actual[index], `${path}[${index}]`);
		}
		return;
	}
	const expectedKeys = Object.keys(expected).sort();
	const actualKeys = Object.keys(actual).sort();
	if (JSON.stringify(expectedKeys) !== JSON.stringify(actualKeys)) {
		throw new Error(`${path}: expected keys ${expectedKeys}, got ${actualKeys}`);
	}
	for (const key of expectedKeys) compare(expected[key], actual[key], `${path}.${key}`);
}

const [expectedPath, actualPath] = process.argv.slice(2);
if (!expectedPath || !actualPath) {
	throw new Error("usage: node scripts/compare-unit-kernel-parity.mjs <expected.json> <actual.json>");
}
const [expected, actual] = await Promise.all([
	readFile(expectedPath, "utf8").then(JSON.parse),
	readFile(actualPath, "utf8").then(JSON.parse),
]);
compare(expected, actual);
process.stdout.write("movement/combat parity: ok\n");
