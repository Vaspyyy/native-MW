import { readFileSync } from "node:fs";

const [rustPath, jsPath] = process.argv.slice(2);
if (!rustPath || !jsPath) {
	throw new Error(
		"usage: node scripts/compare-tactical-parity.mjs <rust-json> <js-json>",
	);
}
const rust = JSON.parse(readFileSync(rustPath, "utf8"));
const js = JSON.parse(readFileSync(jsPath, "utf8"));
const FLOAT_TOLERANCE = 1e-10;

function compare(left, right, path = "$") {
	if (typeof left === "number" && typeof right === "number") {
		if (Number.isInteger(left) && Number.isInteger(right)) {
			if (left !== right) throw new Error(`${path}: ${left} !== ${right}`);
			return;
		}
		if (!Number.isFinite(left) || !Number.isFinite(right)) {
			if (!Object.is(left, right)) throw new Error(`${path}: ${left} !== ${right}`);
			return;
		}
		if (Math.abs(left - right) > FLOAT_TOLERANCE) {
			throw new Error(`${path}: ${left} !== ${right} (tolerance ${FLOAT_TOLERANCE})`);
		}
		return;
	}
	if (Array.isArray(left) || Array.isArray(right)) {
		if (!Array.isArray(left) || !Array.isArray(right)) throw new Error(`${path}: type mismatch`);
		if (left.length !== right.length) throw new Error(`${path}: length ${left.length} !== ${right.length}`);
		for (let index = 0; index < left.length; index++) compare(left[index], right[index], `${path}[${index}]`);
		return;
	}
	if (left && right && typeof left === "object" && typeof right === "object") {
		const leftKeys = Object.keys(left).sort();
		const rightKeys = Object.keys(right).sort();
		if (leftKeys.length !== rightKeys.length || leftKeys.some((key, index) => key !== rightKeys[index])) {
			throw new Error(`${path}: object keys ${JSON.stringify(leftKeys)} !== ${JSON.stringify(rightKeys)}`);
		}
		for (const key of leftKeys) compare(left[key], right[key], `${path}.${key}`);
		return;
	}
	if (!Object.is(left, right)) throw new Error(`${path}: ${JSON.stringify(left)} !== ${JSON.stringify(right)}`);
}

compare(rust, js);
console.log("Tactical grid parity passed");
