#!/usr/bin/env node
import { readFile } from "node:fs/promises";

const TOLERANCE = 1e-9;
function compare(left, right, path = "$") {
	if (typeof left === "number" && typeof right === "number") {
		return Number.isFinite(left) &&
			Number.isFinite(right) &&
			Math.abs(left - right) <= TOLERANCE
			? null
			: `${path}: ${left} != ${right} (float tolerance ${TOLERANCE})`;
	}
	if (
		left === null ||
		right === null ||
		typeof left !== "object" ||
		typeof right !== "object"
	)
		return Object.is(left, right)
			? null
			: `${path}: ${JSON.stringify(left)} != ${JSON.stringify(right)}`;
	if (Array.isArray(left) || Array.isArray(right)) {
		if (!Array.isArray(left) || !Array.isArray(right))
			return `${path}: array/object mismatch`;
		if (left.length !== right.length)
			return `${path}: length ${left.length} != ${right.length}`;
		for (let index = 0; index < left.length; index++) {
			const mismatch = compare(left[index], right[index], `${path}[${index}]`);
			if (mismatch) return mismatch;
		}
		return null;
	}
	const leftKeys = Object.keys(left).sort();
	const rightKeys = Object.keys(right).sort();
	if (
		leftKeys.length !== rightKeys.length ||
		leftKeys.some((key, index) => key !== rightKeys[index])
	)
		return `${path}: keys ${JSON.stringify(leftKeys)} != ${JSON.stringify(rightKeys)}`;
	for (const key of leftKeys) {
		const mismatch = compare(left[key], right[key], `${path}.${key}`);
		if (mismatch) return mismatch;
	}
	return null;
}

const [leftPath, rightPath] = process.argv.slice(2);
if (!leftPath || !rightPath)
	throw new Error(
		"usage: compare-territory-control-parity.mjs <left-report.json> <right-report.json>",
	);
const [left, right] = await Promise.all([
	readFile(leftPath, "utf8").then(JSON.parse),
	readFile(rightPath, "utf8").then(JSON.parse),
]);
const mismatch = compare(left, right);
if (mismatch) {
	console.error(`territory control parity: mismatch\n${mismatch}`);
	process.exitCode = 1;
} else console.log("territory control parity: ok");
