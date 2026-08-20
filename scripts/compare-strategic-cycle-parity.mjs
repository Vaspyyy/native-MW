#!/usr/bin/env node
import { readFile } from "node:fs/promises";

const tolerance = 1e-10;
function diff(a, b, path = "$") {
	if (typeof a === "number" && typeof b === "number")
		return Number.isFinite(a) &&
			Number.isFinite(b) &&
			Math.abs(a - b) <= tolerance
			? null
			: `${path}: ${a} != ${b}`;
	if (
		a === null ||
		b === null ||
		typeof a !== "object" ||
		typeof b !== "object"
	)
		return Object.is(a, b)
			? null
			: `${path}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`;
	if (Array.isArray(a) || Array.isArray(b)) {
		if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length)
			return `${path}: array length/type mismatch`;
		for (let i = 0; i < a.length; i++) {
			const found = diff(a[i], b[i], `${path}[${i}]`);
			if (found) return found;
		}
		return null;
	}
	const ka = Object.keys(a).sort();
	const kb = Object.keys(b).sort();
	if (JSON.stringify(ka) !== JSON.stringify(kb))
		return `${path}: object keys mismatch`;
	for (const key of ka) {
		const found = diff(a[key], b[key], `${path}.${key}`);
		if (found) return found;
	}
	return null;
}
const [left, right] = process.argv.slice(2);
if (!left || !right)
	throw new Error(
		"usage: compare-strategic-cycle-parity.mjs <js.json> <rust.json>",
	);
const [a, b] = await Promise.all([
	readFile(left, "utf8").then(JSON.parse),
	readFile(right, "utf8").then(JSON.parse),
]);
const mismatch = diff(a, b);
if (mismatch) {
	console.error(`strategic cycle parity: mismatch\n${mismatch}`);
	process.exitCode = 1;
} else console.log("strategic cycle parity: ok");
