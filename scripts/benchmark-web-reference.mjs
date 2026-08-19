import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { pathToFileURL } from "node:url";
import vm from "node:vm";
import { gunzipSync } from "node:zlib";

const [mode, webRoot, scenarioPath, gridResText, repeatText = "5"] =
	process.argv.slice(2);
const gridRes = Number(gridResText);
const repeat = Number(repeatText);
if (
	!['decode', 'field'].includes(mode) ||
	!webRoot ||
	!scenarioPath ||
	!(gridRes > 0) ||
	!Number.isInteger(repeat) ||
	repeat < 1
) {
	throw new Error(
		"usage: node scripts/benchmark-web-reference.mjs <decode|field> <web-root> <scenario> <grid-res> [repeat]",
	);
}

const codecUrl = pathToFileURL(resolve(webRoot, "src/scenario-codec.js"));
const { decodeScenarioBinary } = await import(codecUrl.href);

function load() {
	const compressed = readFileSync(scenarioPath);
	return decodeScenarioBinary(new Uint8Array(gunzipSync(compressed)), {
		targetGridRes: gridRes,
	});
}

function median(samples) {
	const sorted = [...samples].sort((left, right) => left - right);
	return sorted[Math.floor(sorted.length / 2)];
}

if (mode === "decode") {
	load();
	const samples = [];
	for (let iteration = 0; iteration < repeat; iteration++) {
		const started = performance.now();
		load();
		samples.push(performance.now() - started);
	}
	console.log(JSON.stringify({ mode, repeat, median_ms: median(samples), samples_ms: samples }));
} else {
	const decoded = load();
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
	const input = {
		landMask,
		dominantSideMap,
		hostile: context.__createHostilityChecker(relations, 2),
		gridWidth: decoded.target.width,
		gridHeight: decoded.target.height,
		gridRes: decoded.target.gridRes,
	};
	context.__buildDirectionField(input);
	const samples = [];
	for (let iteration = 0; iteration < repeat; iteration++) {
		const started = performance.now();
		context.__buildDirectionField(input);
		samples.push(performance.now() - started);
	}
	console.log(JSON.stringify({ mode, repeat, median_ms: median(samples), samples_ms: samples }));
}
