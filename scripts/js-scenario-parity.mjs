import { readFileSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

const [webRoot, scenarioPath, gridResText] = process.argv.slice(2);
if (!webRoot || !scenarioPath || !gridResText) {
	throw new Error(
		"usage: node scripts/js-scenario-parity.mjs <web-root> <scenario> <grid-res>",
	);
}

const codecUrl = pathToFileURL(resolve(webRoot, "src/scenario-codec.js"));
const { decodeScenarioBinary } = await import(codecUrl.href);
const compressed = readFileSync(scenarioPath);
const bytes = new Uint8Array(gunzipSync(compressed));
const decoded = decodeScenarioBinary(bytes, {
	targetGridRes: Number(gridResText),
});

const OFFSET = 0xcbf29ce484222325n;
const PRIME = 0x100000001b3n;
const MASK = (1n << 64n) - 1n;

function hashByte(hash, byte) {
	return ((hash ^ BigInt(byte)) * PRIME) & MASK;
}

function finish(hash) {
	return hash.toString(16).padStart(16, "0");
}

function hashU8(values) {
	let hash = OFFSET;
	for (const value of values) hash = hashByte(hash, value);
	return finish(hash);
}

function hashU16(values) {
	let hash = OFFSET;
	for (const value of values) {
		hash = hashByte(hash, value & 0xff);
		hash = hashByte(hash, (value >>> 8) & 0xff);
	}
	return finish(hash);
}

function hashI32(values) {
	let hash = OFFSET;
	for (const signed of values) {
		const value = signed >>> 0;
		hash = hashByte(hash, value & 0xff);
		hash = hashByte(hash, (value >>> 8) & 0xff);
		hash = hashByte(hash, (value >>> 16) & 0xff);
		hash = hashByte(hash, (value >>> 24) & 0xff);
	}
	return finish(hash);
}

console.log(
	JSON.stringify({
		name: decoded.scenario.name ?? "Unnamed scenario",
		entry_count: decoded.entryCount,
		source: {
			grid_res: decoded.source.gridRes,
			width: decoded.source.width,
			height: decoded.source.height,
		},
		target: {
			grid_res: decoded.target.gridRes,
			width: decoded.target.width,
			height: decoded.target.height,
		},
		hashes: {
			world_control: hashU16(decoded.worldControl),
			de_jure: hashU16(decoded.deJure),
			land: hashU8(decoded.land),
			biome: hashU8(decoded.biome),
			province: hashI32(decoded.province),
		},
	}),
);
