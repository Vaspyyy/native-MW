import { readFileSync } from "node:fs";
import { performance } from "node:perf_hooks";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import vm from "node:vm";
import { gunzipSync } from "node:zlib";

const [webRoot, scenarioPath, gridResText] = process.argv.slice(2);
if (!webRoot || !scenarioPath || !gridResText) {
	throw new Error(
		"usage: node scripts/js-direction-parity.mjs <web-root> <scenario> <grid-res>",
	);
}

const codecUrl = pathToFileURL(resolve(webRoot, "src/scenario-codec.js"));
const { decodeScenarioBinary } = await import(codecUrl.href);
const scenarioBytes = new Uint8Array(gunzipSync(readFileSync(scenarioPath)));
const decoded = decodeScenarioBinary(scenarioBytes, {
	targetGridRes: Number(gridResText),
});

const countries = decoded.scenario.metadata;
const sideA = countries.find((country) => country.name === "Russia");
const sideB = countries.find(
	(country) => country.name === "People's Republic of China",
);
if (!sideA || !sideB) throw new Error("Russia/China fixture countries are missing");

const landMask = new Uint8Array(decoded.land);
const dominantSideMap = new Int8Array(decoded.worldControl.length);
dominantSideMap.fill(-1);
for (let index = 0; index < decoded.worldControl.length; index++) {
	const owner = decoded.worldControl[index];
	if (owner === sideA.id) {
		landMask[index] = 2;
		dominantSideMap[index] = 0;
	} else if (owner === sideB.id) {
		landMask[index] = 2;
		dominantSideMap[index] = 1;
	}
}

const workerSource = `${readFileSync(resolve(webRoot, "workers/simulation-worker.js"), "utf8")}\n\
globalThis.__buildDirectionField = buildDirectionField;\n\
globalThis.__createHostilityChecker = createHostilityChecker;`;
const context = vm.createContext({
	performance,
	self: { postMessage() {} },
	Uint8Array,
	Int8Array,
	Int32Array,
	Float32Array,
	Map,
	Set,
	Math,
});
vm.runInContext(workerSource, context, { filename: "simulation-worker.js" });
const relations = new Uint8Array([0, 1, 1, 0]);
const field = context.__buildDirectionField({
	landMask,
	dominantSideMap,
	hostile: context.__createHostilityChecker(relations, 2),
	gridWidth: decoded.target.width,
	gridHeight: decoded.target.height,
	gridRes: decoded.target.gridRes,
});

const OFFSET = 0xcbf29ce484222325n;
const PRIME = 0x100000001b3n;
const MASK = (1n << 64n) - 1n;
function hashF32(values) {
	let hash = OFFSET;
	const view = new DataView(new ArrayBuffer(4));
	for (const value of values) {
		view.setFloat32(0, value, true);
		for (let byte = 0; byte < 4; byte++) {
			hash = ((hash ^ BigInt(view.getUint8(byte))) * PRIME) & MASK;
		}
	}
	return hash.toString(16).padStart(16, "0");
}

let directedCells = 0;
for (let index = 0; index < field.frontlineDirLat.length; index++) {
	if (field.frontlineDirLat[index] !== 0 || field.frontlineDirLng[index] !== 0) {
		directedCells++;
	}
}

console.log(
	JSON.stringify({
		side_a: { id: sideA.id, name: sideA.name },
		side_b: { id: sideB.id, name: sideB.name },
		target: {
			grid_res: decoded.target.gridRes,
			width: decoded.target.width,
			height: decoded.target.height,
		},
		hashes: {
			latitude: hashF32(field.frontlineDirLat),
			longitude: hashF32(field.frontlineDirLng),
		},
		directed_cells: directedCells,
	}),
);
