#!/usr/bin/env node
// Deterministic sparse production-sized territory-control-v1 input.
const [widthText = "2400", heightText = "1200", sourcesText = "4800"] =
	process.argv.slice(2);
const width = Number(widthText);
const height = Number(heightText);
const sourceCount = Number(sourcesText);
if (
	![width, height, sourceCount].every(Number.isInteger) ||
	width <= 0 ||
	height <= 0 ||
	sourceCount <= 0
)
	throw new Error(
		"usage: generate-territory-control-stress.mjs [width=2400] [height=1200] [sources=4800]",
	);
const cells = width * height;
const maxSides = 4;
const gridRes = 180 / height;
const land = new Array(cells);
const worldControl = new Array(cells);
const deJure = new Array(cells);
const primaryOccupier = new Array(cells).fill(0);
const dominantSide = new Array(cells).fill(-1);
const occupation = new Array(cells).fill(0);
for (let y = 0; y < height; y++)
	for (let x = 0; x < width; x++) {
		const index = y * width + x;
		const country = ((Math.floor(x / 300) + Math.floor(y / 240) * 3) % 8) + 1;
		land[index] = 2;
		worldControl[index] = country;
		deJure[index] = country;
		primaryOccupier[index] = country;
		dominantSide[index] = (country - 1) % maxSides;
	}
const countryToSide = Array.from({ length: 8 }, (_, index) => [
	index + 1,
	index % maxSides,
]);
const hostilityMatrix = [0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0];
const sideInfluence = Array.from({ length: maxSides }, () =>
	new Array(cells).fill(0),
);
for (let index = 0; index < cells; index++)
	sideInfluence[dominantSide[index]][index] = 0.2;
const sources = [];
const benchmarkDirtyCells = [];
for (let index = 0; index < sourceCount; index++) {
	const x = (index * 149 + 71) % width;
	const y = (index * 83 + 29) % height;
	const cell = y * width + x;
	const ownerCountry = worldControl[cell];
	const ownerSide = countryToSide[ownerCountry - 1][1];
	const invades = index % 3 === 0;
	const sideIndex = invades ? ownerSide ^ 1 : ownerSide;
	const sovereignId = sideIndex + 1;
	sources.push({
		id: index + 1,
		sideIndex,
		sovereignId,
		beneficiaryId: sovereignId,
		lat: y * gridRes - 90,
		lng: x * gridRes - 180,
		radius: gridRes * 2.5,
		delta: invades ? 0.4 : 0.08,
		concentrationBonus: 1,
		role: "OFFENSE",
		ownerAllyCountryIds: [sovereignId],
	});
	if (index % 24 === 0) benchmarkDirtyCells.push(cell);
}
const fixture = {
	schema: "territory-control-v1",
	config: {
		width,
		height,
		gridRes,
		maxSides,
		tileSize: 32,
		countedLandValue: 2,
		hysteresis: 0.15,
		cityResistance: 0.35,
		hostileDecay: 0.5,
		reclaimMultiplier: 1.5,
	},
	maps: {
		land,
		worldControl,
		deJure,
		primaryOccupier,
		dominantSide,
		occupation,
		sideInfluence,
	},
	countryToSide,
	hostilityMatrix,
	cities: [],
	benchmarkDirtyCells,
	operations: [
		{ op: "applySources", sources },
		{ op: "advance", budget: cells + sourceCount },
		{ op: "markCells", cellIndices: benchmarkDirtyCells },
		{ op: "advance", budget: 32768 },
	],
};
process.stdout.write(`${JSON.stringify(fixture)}\n`);
