const unitsPerSide = Number(process.argv[2] ?? "2400");
if (!Number.isSafeInteger(unitsPerSide) || unitsPerSide < 1) {
	throw new Error(
		"usage: node scripts/generate-tactical-stress.mjs [units-per-side=2400]",
	);
}

let state = 0x4d575031;
function random() {
	state ^= state << 13;
	state ^= state >>> 17;
	state ^= state << 5;
	state >>>= 0;
	return state / 0x100000000;
}
const jitter = (scale) => (random() * 2 - 1) * scale;
const hotspots = [
	[-24, -12], [-18, -6], [-12, 0], [-6, 6], [0, 12],
	[6, -12], [12, -6], [18, 0], [24, 6], [30, 12],
];
const units = [];
let id = 1;
for (let side = 0; side < 2; side++) {
	for (let index = 0; index < unitsPerSide; index++) {
		const fraction = index / unitsPerSide;
		let lat;
		let lng;
		if (fraction < 0.8) {
			lng = -30 + random() * 60;
			lat = (side === 0 ? -0.8 : 0.8) + jitter(1.5);
		} else if (fraction < 0.95) {
			lng = -45 + random() * 90;
			lat = (side === 0 ? -1 : 1) * (5 + random() * 20);
		} else {
			const [hotLng, hotLat] = hotspots[index % hotspots.length];
			lng = hotLng + jitter(0.12);
			lat = hotLat + (side === 0 ? -0.15 : 0.15) + jitter(0.12);
		}
		units.push({
			id: id++,
			side,
			lat,
			lng,
			strength: 25 + Math.floor(random() * 76),
			allyWeight: 0.5 + random() * 2.5,
			armor: index % 10 === 0,
			support: index % 5 === 0,
		});
	}
}

console.log(JSON.stringify({
	schemaVersion: "1",
	cellSize: 0.6,
	units,
	neighborQueries: [],
	pairQueries: [{ side: 0, radiusCells: 1, radiusSq: 0.36 }],
}));
