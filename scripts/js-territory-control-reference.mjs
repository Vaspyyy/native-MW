#!/usr/bin/env node
/** Deterministic browser-semantics oracle for territory-control-v1. */
import { readFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";

export const SCHEMA = "territory-control-v1";
const NEIGHBORS = [
	[0, 1],
	[0, -1],
	[1, 0],
	[-1, 0],
	[1, 1],
	[1, -1],
	[-1, 1],
	[-1, -1],
]; // N,S,E,W,SE,NE,SW,NW
const integer = (v, fallback = 0) =>
	Number.isFinite(Number(v)) ? Math.trunc(Number(v)) : fallback;
const positiveInteger = (v, fallback = 1) => Math.max(1, integer(v, fallback));
const countryId = (v) => Math.max(0, integer(v));
const sideIndex = (v) => {
	const i = integer(v, -1);
	return i >= 0 ? i : -1;
};
const numeric = (v, fallback = 0) =>
	Number.isFinite(Number(v)) ? Number(v) : fallback;
const fail = (condition, message) => {
	if (!condition) throw new Error(`territory-control-v1: ${message}`);
};
const cloneJson = (v) => JSON.parse(JSON.stringify(v));

function configOf(fixture) {
	fail(fixture?.schema === SCHEMA, `schema must be '${SCHEMA}'`);
	const c = fixture.config || {};
	const config = {
		width: positiveInteger(c.width),
		height: positiveInteger(c.height),
		gridRes: numeric(c.gridRes),
		maxSides: positiveInteger(c.maxSides),
		tileSize: positiveInteger(c.tileSize, 32),
		countedLandValue: numeric(c.countedLandValue, 2),
		hysteresis: numeric(c.hysteresis, 0.15),
		cityResistance: numeric(c.cityResistance, 0.35),
		hostileDecay: numeric(c.hostileDecay, 0.5),
		reclaimMultiplier: numeric(c.reclaimMultiplier, 1.5),
	};
	fail(config.gridRes > 0, "config.gridRes must be positive");
	return config;
}
const cellCount = (state) => state.config.width * state.config.height;
function arrayOf(v, length, name) {
	fail(
		Array.isArray(v) && v.length === length,
		`${name} must have ${length} entries`,
	);
	return v;
}
function mapsOf(fixture, config) {
	const maps = fixture.maps || {};
	const cells = config.width * config.height;
	return {
		land: Uint8Array.from(arrayOf(maps.land, cells, "maps.land")),
		worldControl: Int32Array.from(
			arrayOf(maps.worldControl, cells, "maps.worldControl"),
		),
		deJure: Int32Array.from(arrayOf(maps.deJure, cells, "maps.deJure")),
		primaryOccupier: Int32Array.from(
			arrayOf(maps.primaryOccupier, cells, "maps.primaryOccupier"),
		),
		dominantSide: Int8Array.from(
			arrayOf(maps.dominantSide, cells, "maps.dominantSide"),
		),
		occupation: Float32Array.from(
			arrayOf(maps.occupation, cells, "maps.occupation"),
		),
		sideInfluence: arrayOf(
			maps.sideInfluence,
			config.maxSides,
			"maps.sideInfluence",
		).map((v, i) =>
			Float32Array.from(arrayOf(v, cells, `maps.sideInfluence[${i}]`)),
		),
	};
}
function serialMaps(maps) {
	return {
		land: [...maps.land],
		worldControl: [...maps.worldControl],
		deJure: [...maps.deJure],
		primaryOccupier: [...maps.primaryOccupier],
		dominantSide: [...maps.dominantSide],
		occupation: [...maps.occupation],
		sideInfluence: maps.sideInfluence.map((v) => [...v]),
	};
}
function mappingOf(entries, maxSides) {
	fail(
		Array.isArray(entries),
		"countryToSide must be sorted [countryId, sideIndex] entries",
	);
	const result = new Map();
	let prior = -1;
	for (const entry of entries) {
		fail(
			Array.isArray(entry) && entry.length === 2,
			"countryToSide entries must be pairs",
		);
		const id = countryId(entry[0]);
		const side = sideIndex(entry[1]);
		fail(
			id > 0 && id > prior,
			"countryToSide must be strictly sorted by country id",
		);
		fail(side >= 0 && side < maxSides, "countryToSide side is out of range");
		prior = id;
		result.set(id, side);
	}
	return result;
}
function hostilityOf(matrix, maxSides) {
	return Uint8Array.from(
		arrayOf(matrix, maxSides * maxSides, "hostilityMatrix"),
		(v) => (numeric(v) === 1 ? 1 : 0),
	);
}
function hostile(state, a, b) {
	return (
		a >= 0 &&
		b >= 0 &&
		a !== b &&
		a < state.config.maxSides &&
		b < state.config.maxSides &&
		state.hostility[a * state.config.maxSides + b] === 1
	);
}

function tileBounds(state, index) {
	const { width, height, tileSize } = state.config;
	const wide = Math.ceil(width / tileSize);
	const total = wide * Math.ceil(height / tileSize);
	const tileIndex = Math.max(0, Math.min(total - 1, integer(index)));
	const tileX = tileIndex % wide;
	const tileY = Math.floor(tileIndex / wide);
	const minX = tileX * tileSize;
	const minY = tileY * tileSize;
	return {
		tileIndex,
		tileX,
		tileY,
		minX,
		minY,
		maxX: Math.min(width, minX + tileSize),
		maxY: Math.min(height, minY + tileSize),
	};
}
function dirtyTracker(state) {
	const tilesWide = Math.ceil(state.config.width / state.config.tileSize);
	const tilesHigh = Math.ceil(state.config.height / state.config.tileSize);
	const totalTiles = tilesWide * tilesHigh;
	const flags = new Uint8Array(totalTiles);
	const dirty = [];
	const markIndex = (index) => {
		if (index < 0 || index >= totalTiles || flags[index]) return false;
		flags[index] = 1;
		dirty.push(index);
		return true;
	};
	const markTile = (x, y, neighbors = true) => {
		x = integer(x, -1);
		y = integer(y, -1);
		if (x < 0 || y < 0 || x >= tilesWide || y >= tilesHigh) return 0;
		let added = 0;
		for (let dy = neighbors ? -1 : 0; dy <= (neighbors ? 1 : 0); dy++)
			for (let dx = neighbors ? -1 : 0; dx <= (neighbors ? 1 : 0); dx++) {
				const nx = x + dx;
				const ny = y + dy;
				if (nx < 0 || ny < 0 || nx >= tilesWide || ny >= tilesHigh) continue;
				added += Number(markIndex(ny * tilesWide + nx));
			}
		return added;
	};
	return {
		tilesWide,
		tilesHigh,
		totalTiles,
		markCell(index, neighbors = true) {
			index = integer(index, -1);
			if (index < 0 || index >= cellCount(state)) return 0;
			return markTile(
				Math.floor((index % state.config.width) / state.config.tileSize),
				Math.floor(
					Math.floor(index / state.config.width) / state.config.tileSize,
				),
				neighbors,
			);
		},
		markAll() {
			let added = 0;
			for (let i = 0; i < totalTiles; i++) added += Number(markIndex(i));
			return added;
		},
		consume() {
			const picked = [...dirty].sort((a, b) => a - b);
			if (!picked.length) return picked;
			const set = new Set(picked);
			for (const i of picked) flags[i] = 0;
			const retained = dirty.filter((i) => !set.has(i));
			dirty.length = 0;
			dirty.push(...retained);
			return picked;
		},
		clear() {
			for (const i of dirty) flags[i] = 0;
			dirty.length = 0;
		},
		peek() {
			return [...dirty].sort((a, b) => a - b);
		},
		size() {
			return dirty.length;
		},
	};
}

function countryCounts() {
	return {
		owned: 0,
		controlled: 0,
		creditedTerritory: 0,
		frontline: 0,
		deJureTotal: 0,
		coreControlled: 0,
		citiesTotal: 0,
		citiesControlled: 0,
		cityPopulationTotal: 0,
		cityPopulationControlled: 0,
		capitalsTotal: 0,
		capitalsHeld: 0,
		deJureControlBySide: new Map(),
		deJureControlByCountry: new Map(),
		cityControlBySide: new Map(),
		cityPopulationBySide: new Map(),
		capitalControlBySide: new Map(),
	};
}
function sideCounts() {
	return {
		territory: 0,
		ownedTerritory: 0,
		homeTerritoryControlled: 0,
		frontline: 0,
		deJureCellsControlled: 0,
		citiesControlled: 0,
		cityPopulationControlled: 0,
		capitalsControlled: 0,
	};
}
function aggregate() {
	return {
		landCells: 0,
		positiveOccupationCells: 0,
		negativeOccupationCells: 0,
		countries: new Map(),
		sides: new Map(),
	};
}
function country(agg, id) {
	if (!agg.countries.has(id)) agg.countries.set(id, countryCounts());
	return agg.countries.get(id);
}
function side(agg, id) {
	if (!agg.sides.has(id)) agg.sides.set(id, sideCounts());
	return agg.sides.get(id);
}
function inc(map, key, value = 1) {
	if (key !== undefined && key !== null && value !== 0)
		map.set(key, (map.get(key) || 0) + value);
}
const countryScalars = [
	"owned",
	"controlled",
	"creditedTerritory",
	"frontline",
	"deJureTotal",
	"coreControlled",
	"citiesTotal",
	"citiesControlled",
	"cityPopulationTotal",
	"cityPopulationControlled",
	"capitalsTotal",
	"capitalsHeld",
];
const countryNested = [
	"deJureControlBySide",
	"deJureControlByCountry",
	"cityControlBySide",
	"cityPopulationBySide",
	"capitalControlBySide",
];
const sideScalars = [
	"territory",
	"ownedTerritory",
	"homeTerritoryControlled",
	"frontline",
	"deJureCellsControlled",
	"citiesControlled",
	"cityPopulationControlled",
	"capitalsControlled",
];
function cloneAggregate(source) {
	const target = aggregate();
	target.landCells = source.landCells;
	target.positiveOccupationCells = source.positiveOccupationCells;
	target.negativeOccupationCells = source.negativeOccupationCells;
	for (const [id, old] of source.countries) {
		const next = countryCounts();
		for (const f of countryScalars) next[f] = old[f];
		for (const f of countryNested) next[f] = new Map(old[f]);
		target.countries.set(id, next);
	}
	for (const [id, old] of source.sides) target.sides.set(id, { ...old });
	return target;
}
function mergeTile(target, source, direction) {
	if (!source) return;
	target.landCells += source.landCells * direction;
	target.positiveOccupationCells += source.positiveOccupationCells * direction;
	target.negativeOccupationCells += source.negativeOccupationCells * direction;
	for (const [id, old] of source.countries) {
		const next = country(target, id);
		for (const f of countryScalars) next[f] += old[f] * direction;
		for (const f of countryNested)
			for (const [key, value] of old[f]) {
				const changed = (next[f].get(key) || 0) + value * direction;
				if (changed === 0) next[f].delete(key);
				else next[f].set(key, changed);
			}
	}
	for (const [id, old] of source.sides) {
		const next = side(target, id);
		for (const f of sideScalars) next[f] += old[f] * direction;
	}
}

function cityIndex(state, cities) {
	const byTile = new Map();
	for (let sourceIndex = 0; sourceIndex < cities.length; sourceIndex++) {
		const city = cities[sourceIndex];
		if (!city) continue;
		const cellIndex = integer(city.cellIndex ?? city.gridIndex, -1);
		if (cellIndex < 0 || cellIndex >= cellCount(state)) continue;
		const item = {
			id: city.id ?? sourceIndex,
			sourceIndex,
			cellIndex,
			ownerCountryId: countryId(city.ownerId ?? city.sovereignId),
			population: Math.max(0, numeric(city.population ?? city.pop)),
			isCapital: city.isCapital === true,
		};
		const tile =
			Math.floor(
				Math.floor(cellIndex / state.config.width) / state.config.tileSize,
			) *
				state.dirty.tilesWide +
			Math.floor((cellIndex % state.config.width) / state.config.tileSize);
		if (!byTile.has(tile)) byTile.set(tile, []);
		byTile.get(tile).push(item);
	}
	for (const value of byTile.values())
		value.sort(
			(a, b) => a.cellIndex - b.cellIndex || a.sourceIndex - b.sourceIndex,
		);
	return byTile;
}
function isFrontline(state, index, x, y, controller) {
	if (controller < 0) return false;
	const check = (neighbor) =>
		hostile(state, controller, sideIndex(state.maps.dominantSide[neighbor]));
	return (
		(x > 0 && check(index - 1)) ||
		(x + 1 < state.config.width && check(index + 1)) ||
		(y > 0 && check(index - state.config.width)) ||
		(y + 1 < state.config.height && check(index + state.config.width))
	);
}
function processCell(state, summary, index, x, y) {
	const maps = state.maps;
	if (maps.land[index] !== state.config.countedLandValue) return;
	summary.landCells++;
	const owner = countryId(maps.worldControl[index]);
	const core = countryId(maps.deJure[index] ?? owner);
	const controller = sideIndex(maps.dominantSide[index]);
	const ownerSide = state.countryToSide.get(owner) ?? -1;
	const coreSide = state.countryToSide.get(core) ?? -1;
	const credited = countryId(maps.primaryOccupier[index] || owner);
	const occupation = Number(maps.occupation[index] || 0);
	if (occupation > 0) summary.positiveOccupationCells++;
	else if (occupation < 0) summary.negativeOccupationCells++;
	const frontline = isFrontline(state, index, x, y, controller);
	if (controller >= 0) {
		const counts = side(summary, controller);
		counts.territory++;
		if (frontline) counts.frontline++;
	}
	if (owner > 0) {
		const counts = country(summary, owner);
		counts.owned++;
		if (ownerSide >= 0) side(summary, ownerSide).ownedTerritory++;
		if (controller === ownerSide && ownerSide >= 0) {
			counts.controlled++;
			side(summary, ownerSide).homeTerritoryControlled++;
			if (frontline) counts.frontline++;
		}
	}
	if (credited > 0) country(summary, credited).creditedTerritory++;
	if (core > 0) {
		const counts = country(summary, core);
		counts.deJureTotal++;
		if (controller >= 0) {
			inc(counts.deJureControlBySide, controller);
			side(summary, controller).deJureCellsControlled++;
		}
		if (credited > 0) inc(counts.deJureControlByCountry, credited);
		if (coreSide >= 0 && controller === coreSide) counts.coreControlled++;
	}
}
function processCity(state, summary, city) {
	const maps = state.maps;
	if (maps.land[city.cellIndex] !== state.config.countedLandValue) return;
	const ownerSide = state.countryToSide.get(city.ownerCountryId) ?? -1;
	const controller = sideIndex(maps.dominantSide[city.cellIndex]);
	if (city.ownerCountryId > 0) {
		const counts = country(summary, city.ownerCountryId);
		counts.citiesTotal++;
		counts.cityPopulationTotal += city.population;
		if (city.isCapital) counts.capitalsTotal++;
		if (controller >= 0) {
			inc(counts.cityControlBySide, controller);
			inc(counts.cityPopulationBySide, controller, city.population);
			if (city.isCapital) inc(counts.capitalControlBySide, controller);
		}
		if (ownerSide >= 0 && controller === ownerSide) {
			counts.citiesControlled++;
			counts.cityPopulationControlled += city.population;
			if (city.isCapital) counts.capitalsHeld++;
		}
	}
	if (controller >= 0) {
		const counts = side(summary, controller);
		counts.citiesControlled++;
		counts.cityPopulationControlled += city.population;
		if (city.isCapital) counts.capitalsControlled++;
	}
}

function sortedMap(map) {
	return [...map.entries()]
		.filter(([, value]) => value !== 0)
		.sort(([a], [b]) => Number(a) - Number(b));
}
function serializeSnapshot(state, snapshot) {
	if (!snapshot) return null;
	const ids = new Set([
		...snapshot.aggregate.countries.keys(),
		...state.countryToSide.keys(),
	]);
	const countries = [...ids]
		.sort((a, b) => a - b)
		.map((id) => {
			const c = snapshot.aggregate.countries.get(id) || countryCounts();
			const index = state.countryToSide.get(id) ?? -1;
			const total = Math.max(0, c.deJureTotal);
			const core = Math.max(0, c.coreControlled);
			const capitals = Math.max(0, c.capitalsTotal);
			const held = Math.max(0, c.capitalsHeld);
			return {
				countryId: id,
				sideIndex: index,
				sideUid: index >= 0 ? state.sideUids[index] || null : null,
				owned: Math.max(0, c.owned),
				controlled: Math.max(0, c.controlled),
				creditedTerritory: Math.max(0, c.creditedTerritory),
				frontline: Math.max(0, c.frontline),
				deJureTotal: total,
				coreControlled: core,
				coreControlRatio: total > 0 ? Math.min(1, core / total) : 0,
				deJureNotHeld: Math.max(0, total - core),
				deJureControlBySide: sortedMap(c.deJureControlBySide),
				deJureControlByCountry: sortedMap(c.deJureControlByCountry),
				citiesTotal: Math.max(0, c.citiesTotal),
				citiesControlled: Math.max(0, c.citiesControlled),
				cityPopulationTotal: Math.max(0, c.cityPopulationTotal),
				cityPopulationControlled: Math.max(0, c.cityPopulationControlled),
				capitalsTotal: capitals,
				capitalsHeld: held,
				capitalHeld: capitals === 0 || held === capitals,
				cityControlBySide: sortedMap(c.cityControlBySide),
				cityPopulationBySide: sortedMap(c.cityPopulationBySide),
				capitalControlBySide: sortedMap(c.capitalControlBySide),
			};
		});
	const sideIds = new Set([
		...snapshot.aggregate.sides.keys(),
		...state.countryToSide.values(),
	]);
	const sides = [...sideIds]
		.sort((a, b) => a - b)
		.map((id) => {
			const s = snapshot.aggregate.sides.get(id) || sideCounts();
			return {
				sideIndex: id,
				sideUid: state.sideUids[id] || null,
				countryIds: [...state.countryToSide.entries()]
					.filter(([, value]) => value === id)
					.map(([countryId]) => countryId)
					.sort((a, b) => a - b),
				territory: Math.max(0, s.territory),
				ownedTerritory: Math.max(0, s.ownedTerritory),
				homeTerritoryControlled: Math.max(0, s.homeTerritoryControlled),
				frontline: Math.max(0, s.frontline),
				deJureCellsControlled: Math.max(0, s.deJureCellsControlled),
				citiesControlled: Math.max(0, s.citiesControlled),
				cityPopulationControlled: Math.max(0, s.cityPopulationControlled),
				capitalsControlled: Math.max(0, s.capitalsControlled),
			};
		});
	return {
		generation: snapshot.generation,
		commitSequence: snapshot.commitSequence,
		topologyRevision: snapshot.topologyRevision,
		worldRevision: snapshot.worldRevision,
		cityRevision: snapshot.cityRevision,
		processedTiles: snapshot.processedTiles,
		processedItems: snapshot.processedItems,
		pendingDirtyTilesAtCommit: snapshot.pendingDirtyTilesAtCommit,
		landCells: Math.max(0, snapshot.aggregate.landCells),
		positiveOccupationCells: Math.max(
			0,
			snapshot.aggregate.positiveOccupationCells,
		),
		negativeOccupationCells: Math.max(
			0,
			snapshot.aggregate.negativeOccupationCells,
		),
		countries,
		countryById: countries.map((v) => [v.countryId, v]),
		sides,
		sideByIndex: sides.map((v) => [v.sideIndex, v]),
	};
}

function newLedger(state) {
	state.dirty = dirtyTracker(state);
	state.cityByTile = cityIndex(state, state.cities);
	const ledger = {
		committedTiles: new Array(state.dirty.totalTiles).fill(null),
		committedAggregate: aggregate(),
		snapshot: null,
		active: null,
		nextGeneration: 1,
		commitSequence: 0,
		mutationSequence: 0,
	};
	state.dirty.markAll();
	const reset = () => {
		ledger.active = null;
		ledger.committedTiles = new Array(state.dirty.totalTiles).fill(null);
		ledger.committedAggregate = aggregate();
		ledger.snapshot = null;
		state.dirty.clear();
		state.dirty.markAll();
		ledger.mutationSequence++;
	};
	const begin = () => {
		if (ledger.active || state.dirty.size() === 0) return ledger.active;
		const tiles = state.dirty.consume();
		let totalItems = 0;
		for (const i of tiles) {
			const b = tileBounds(state, i);
			totalItems +=
				(b.maxX - b.minX) * (b.maxY - b.minY) +
				(state.cityByTile.get(i)?.length || 0);
		}
		ledger.active = {
			generation: ledger.nextGeneration++,
			tileIndices: tiles,
			tileCursor: 0,
			tileState: null,
			changed: new Map(),
			totalItems,
			processedItems: 0,
			mutationSequence: ledger.mutationSequence,
		};
		return ledger.active;
	};
	const appendTail = (generation) => {
		const tiles = state.dirty.consume();
		for (const i of tiles) {
			generation.tileIndices.push(i);
			const b = tileBounds(state, i);
			generation.totalItems +=
				(b.maxX - b.minX) * (b.maxY - b.minY) +
				(state.cityByTile.get(i)?.length || 0);
		}
		generation.mutationSequence = ledger.mutationSequence;
	};
	const createTile = (index) => {
		const bounds = tileBounds(state, index);
		return {
			tileIndex: index,
			bounds,
			cellOffset: 0,
			cellCount: (bounds.maxX - bounds.minX) * (bounds.maxY - bounds.minY),
			cityOffset: 0,
			cities: state.cityByTile.get(index) || [],
			summary: aggregate(),
		};
	};
	const commit = (generation) => {
		const nextAggregate = cloneAggregate(ledger.committedAggregate);
		const nextTiles = ledger.committedTiles.slice();
		for (const [i, summary] of generation.changed) {
			mergeTile(nextAggregate, nextTiles[i], -1);
			mergeTile(nextAggregate, summary, 1);
			nextTiles[i] = summary;
		}
		const snapshot = {
			generation: generation.generation,
			commitSequence: ledger.commitSequence + 1,
			topologyRevision: state.topologyRevision,
			worldRevision: state.worldRevision,
			cityRevision: state.cityRevision,
			processedTiles: generation.tileIndices.length,
			processedItems: generation.processedItems,
			pendingDirtyTilesAtCommit: state.dirty.size(),
			aggregate: nextAggregate,
		};
		ledger.committedTiles = nextTiles;
		ledger.committedAggregate = nextAggregate;
		ledger.snapshot = snapshot;
		ledger.commitSequence++;
		ledger.active = null;
		return snapshot;
	};
	return {
		reset,
		markCells(cells, includeNeighbors = true) {
			let added = 0;
			for (const i of cells || [])
				added += state.dirty.markCell(i, includeNeighbors);
			if (added > 0) ledger.mutationSequence++;
			return added;
		},
		step(itemBudget = 16384) {
			let budget = Math.max(0, integer(itemBudget, 16384));
			let processedItems = 0;
			let published = null;
			const generation = begin();
			if (!generation || budget === 0)
				return {
					processedItems: 0,
					committed: false,
					discarded: false,
					generation: generation?.generation ?? null,
					remainingItems: generation
						? generation.totalItems - generation.processedItems
						: 0,
					dirtyTiles: state.dirty.size(),
					hasSnapshot: ledger.snapshot !== null,
					snapshot: null,
				};
			while (budget > 0 && !published) {
				while (
					budget > 0 &&
					generation.tileCursor < generation.tileIndices.length
				) {
					if (!generation.tileState)
						generation.tileState = createTile(
							generation.tileIndices[generation.tileCursor],
						);
					const tile = generation.tileState;
					while (budget > 0 && tile.cellOffset < tile.cellCount) {
						const width = tile.bounds.maxX - tile.bounds.minX;
						const x = tile.bounds.minX + (tile.cellOffset % width);
						const y = tile.bounds.minY + Math.floor(tile.cellOffset / width);
						processCell(state, tile.summary, y * state.config.width + x, x, y);
						tile.cellOffset++;
						generation.processedItems++;
						processedItems++;
						budget--;
					}
					while (budget > 0 && tile.cityOffset < tile.cities.length) {
						processCity(state, tile.summary, tile.cities[tile.cityOffset++]);
						generation.processedItems++;
						processedItems++;
						budget--;
					}
					if (
						tile.cellOffset >= tile.cellCount &&
						tile.cityOffset >= tile.cities.length
					) {
						generation.changed.set(tile.tileIndex, tile.summary);
						generation.tileCursor++;
						generation.tileState = null;
					}
				}
				if (generation.tileCursor < generation.tileIndices.length) break;
				if (generation.mutationSequence !== ledger.mutationSequence) {
					appendTail(generation);
					continue;
				}
				published = commit(generation);
			}
			return {
				processedItems,
				committed: !!published,
				discarded: false,
				generation: generation.generation,
				remainingItems: published
					? 0
					: generation.totalItems - generation.processedItems,
				dirtyTiles: state.dirty.size(),
				hasSnapshot: ledger.snapshot !== null,
				snapshot: published ? serializeSnapshot(state, published) : null,
			};
		},
		flush(itemBudget = 16384) {
			const budget = positiveInteger(itemBudget, 16384);
			let processedItems = 0;
			let committedGenerations = 0;
			while (ledger.active || state.dirty.size()) {
				const step = this.step(budget);
				processedItems += step.processedItems;
				if (step.committed) committedGenerations++;
				if (!step.processedItems && !step.committed)
					throw new Error(
						"territory ledger flush made no deterministic progress",
					);
			}
			return {
				processedItems,
				committedGenerations,
				snapshot: serializeSnapshot(state, ledger.snapshot),
			};
		},
		get snapshot() {
			return ledger.snapshot;
		},
		get status() {
			return {
				hasSnapshot: ledger.snapshot !== null,
				commitSequence: ledger.commitSequence,
				activeGeneration: ledger.active?.generation ?? null,
				activeProcessedItems: ledger.active?.processedItems ?? 0,
				activeTotalItems: ledger.active?.totalItems ?? 0,
				dirtyTiles: state.dirty.size(),
				dirtyTileIndices: state.dirty.peek(),
				mutationSequence: ledger.mutationSequence,
				topologyRevision: state.topologyRevision,
				worldRevision: state.worldRevision,
				cityRevision: state.cityRevision,
			};
		},
	};
}

function syncOccupation(state, index) {
	let bestSide = -1;
	let best = 0;
	for (let side = 0; side < state.maps.sideInfluence.length; side++) {
		const v = state.maps.sideInfluence[side][index];
		if (v > best) {
			best = v;
			bestSide = side;
		}
	}
	const current = state.maps.dominantSide[index];
	if (bestSide >= 0) {
		if (current === -1 || current === bestSide) {
			state.maps.dominantSide[index] = bestSide;
			state.maps.occupation[index] = bestSide % 2 === 0 ? best : -best;
		} else if (
			best >
			(state.maps.sideInfluence[current]?.[index] || 0) +
				state.config.hysteresis
		) {
			state.maps.dominantSide[index] = bestSide;
			state.maps.occupation[index] = bestSide % 2 === 0 ? best : -best;
		}
	} else {
		state.maps.dominantSide[index] = -1;
		state.maps.occupation[index] = 0;
	}
}
const idSet = (v) => new Set((v || []).map(countryId));
function neighborCredit(state, x, y, controller) {
	const ids = [];
	const counts = [];
	for (const [dx, dy] of NEIGHBORS) {
		const nx = x + dx;
		const ny = y + dy;
		if (
			nx < 0 ||
			ny < 0 ||
			nx >= state.config.width ||
			ny >= state.config.height
		)
			continue;
		const id = state.maps.primaryOccupier[ny * state.config.width + nx];
		if (id <= 0 || state.countryToSide.get(id) !== controller) continue;
		const slot = ids.indexOf(id);
		if (slot < 0) {
			ids.push(id);
			counts.push(1);
		} else counts[slot]++;
	}
	let bestId = 0;
	let bestCount = 0;
	for (let i = 0; i < ids.length; i++)
		if (counts[i] > bestCount && counts[i] >= 3) {
			bestId = ids[i];
			bestCount = counts[i];
		}
	return bestId;
}
function applySources(state, sources) {
	const cityCells = new Set(
		[...state.cityByTile.values()].flatMap((cities) =>
			cities.map((city) => city.cellIndex),
		),
	);
	let controllerChanges = 0;
	let creditChanges = 0;
	let touchedCells = 0;
	for (const source of sources || []) {
		const side = sideIndex(source.sideIndex);
		fail(
			side >= 0 && side < state.config.maxSides,
			"source.sideIndex is out of range",
		);
		const sovereign = countryId(source.sovereignId);
		const beneficiary = countryId(source.beneficiaryId) || sovereign;
		const lat = numeric(source.lat);
		const lng = numeric(source.lng);
		const radius = numeric(source.radius);
		const delta = numeric(source.delta);
		const concentration = numeric(source.concentrationBonus, 1);
		fail(radius > 0, "source.radius must be positive");
		fail(delta >= 0, "source.delta must be non-negative");
		fail(concentration >= 0, "source.concentrationBonus must be non-negative");
		const allies = idSet(source.ownerAllyCountryIds);
		const support = source.supportCountryIdsBySide || {};
		const startY = Math.max(
			0,
			Math.floor((lat - radius + 90) / state.config.gridRes),
		);
		const endY = Math.min(
			state.config.height - 1,
			Math.floor((lat + radius + 90) / state.config.gridRes),
		);
		const startX = Math.max(
			0,
			Math.floor((lng - radius + 180) / state.config.gridRes),
		);
		const endX = Math.min(
			state.config.width - 1,
			Math.floor((lng + radius + 180) / state.config.gridRes),
		);
		const radiusSq = radius * radius;
		for (let y = startY; y <= endY; y++) {
			const dLat = lat - (y * state.config.gridRes - 90);
			const dLatSq = dLat * dLat;
			for (let x = startX; x <= endX; x++) {
				const index = y * state.config.width + x;
				if (state.maps.land[index] !== state.config.countedLandValue) continue;
				const dLng = lng - (x * state.config.gridRes - 180);
				const dSq = dLatSq + dLng * dLng;
				if (dSq >= radiusSq) continue;
				let cellDelta = delta;
				if (cityCells.has(index)) cellDelta *= state.config.cityResistance;
				const own = state.maps.sideInfluence[side];
				const currentInfluence = own[index];
				const weight = (1 - Math.sqrt(dSq) / radius) ** 2 * concentration;
				let nextInfluence = currentInfluence + Math.abs(cellDelta) * weight;
				if (nextInfluence > 1) nextInfluence = 1;
				if (
					source.isRebel === true &&
					state.maps.deJure[index] !== countryId(source.rebelId) &&
					nextInfluence > currentInfluence
				)
					nextInfluence = currentInfluence;
				const owner = state.maps.worldControl[index];
				const ownerSide = state.countryToSide.get(owner);
				if (
					ownerSide !== undefined &&
					ownerSide !== side &&
					!hostile(state, side, ownerSide)
				)
					continue;
				const ownerAlly = allies.has(owner);
				if (source.refusesOffense === true && !ownerAlly) continue;
				if (
					!ownerAlly &&
					owner > 0 &&
					(source.role || "OFFENSE") === "OFFENSE" &&
					ownerSide !== undefined &&
					ownerSide !== side &&
					idSet(support[String(ownerSide)] ?? support[ownerSide]).has(owner) &&
					Math.abs(state.maps.occupation[index]) <= 0.1
				)
					continue;
				const oldPrimary = state.maps.primaryOccupier[index];
				const oldController = state.maps.dominantSide[index];
				if (!ownerAlly) {
					const primarySide = state.countryToSide.get(oldPrimary);
					if (primarySide === undefined || primarySide !== side) {
						const credit = neighborCredit(state, x, y, side) || beneficiary;
						const rebels = idSet(source.rebelCountryIds);
						const rebelCore =
							source.rebelCoreByCountry?.[String(credit)] ??
							source.rebelCoreByCountry?.[credit];
						if (
							!rebels.has(credit) ||
							state.maps.deJure[index] === countryId(rebelCore)
						)
							if (nextInfluence > 0.05 || oldPrimary === 0)
								state.maps.primaryOccupier[index] = credit;
					}
				}
				own[index] = nextInfluence;
				for (let other = 0; other < state.config.maxSides; other++)
					if (
						hostile(state, side, other) &&
						state.maps.sideInfluence[other][index] > 0
					)
						state.maps.sideInfluence[other][index] = Math.max(
							0,
							state.maps.sideInfluence[other][index] -
								cellDelta * state.config.hostileDecay,
						);
				if (state.maps.worldControl[index] === sovereign)
					own[index] *= state.config.reclaimMultiplier;
				syncOccupation(state, index);
				touchedCells++;
				const creditChanged = state.maps.primaryOccupier[index] !== oldPrimary;
				const controllerChanged =
					state.maps.dominantSide[index] !== oldController;
				if (creditChanged) creditChanges++;
				if (controllerChanged) controllerChanges++;
				if (creditChanged || controllerChanged)
					state.ledger.markCells([index], true);
			}
		}
	}
	return {
		sources: (sources || []).length,
		touchedCells,
		controllerChanges,
		creditChanges,
	};
}
function mutate(state, changes) {
	let writes = 0;
	for (const [name, pairs] of Object.entries(changes || {})) {
		if (name === "sideInfluence") {
			for (const [sideName, entries] of Object.entries(pairs || {})) {
				const s = sideIndex(sideName);
				fail(
					s >= 0 && s < state.config.maxSides,
					"mutate side is out of range",
				);
				for (const [index, value] of entries || []) {
					fail(
						index >= 0 && index < cellCount(state),
						"mutate index is out of range",
					);
					const parsed = numeric(value);
					fail(
						parsed >= 0 && Number.isFinite(Math.fround(parsed)),
						"sideInfluence must fit Float32",
					);
					state.maps.sideInfluence[s][index] = parsed;
					writes++;
				}
			}
		} else {
			const target = state.maps[name];
			fail(
				target && ArrayBuffer.isView(target),
				`unknown mutable map '${name}'`,
			);
			for (const [index, value] of pairs || []) {
				fail(
					index >= 0 && index < cellCount(state),
					"mutate index is out of range",
				);
				const parsed = numeric(value);
				if (name === "occupation")
					fail(
						Number.isFinite(Math.fround(parsed)),
						"occupation must fit Float32",
					);
				target[index] = parsed;
				writes++;
			}
		}
	}
	return writes;
}
function hash(values) {
	let value = 0x811c9dc5;
	for (const item of values)
		for (let shift = 0; shift < 32; shift += 8) {
			value ^= (item >>> shift) & 255;
			value = Math.imul(value, 0x01000193);
		}
	return (value >>> 0).toString(16).padStart(8, "0");
}
function render(state) {
	if (!state.ledger.snapshot) return null;
	const tiles = [];
	for (let tile = 0; tile < state.dirty.totalTiles; tile++) {
		const bounds = tileBounds(state, tile);
		const payload = [];
		for (let y = bounds.minY; y < bounds.maxY; y++)
			for (let x = bounds.minX; x < bounds.maxX; x++) {
				const i = y * state.config.width + x;
				payload.push(
					state.maps.primaryOccupier[i] || state.maps.worldControl[i],
				);
			}
		tiles.push({
			tileIndex: tile,
			bounds: {
				minX: bounds.minX,
				minY: bounds.minY,
				maxX: bounds.maxX,
				maxY: bounds.maxY,
			},
			payload,
			hash: hash(payload),
		});
	}
	return {
		tileSize: state.config.tileSize,
		tiles,
		totalBytes: tiles.reduce((n, tile) => n + tile.payload.length * 4, 0),
		checksum: hash(tiles.flatMap((tile) => tile.payload)),
	};
}

export function createState(fixture) {
	const config = configOf(fixture);
	const state = {
		config,
		maps: mapsOf(fixture, config),
		countryToSide: mappingOf(fixture.countryToSide || [], config.maxSides),
		hostility: hostilityOf(fixture.hostilityMatrix, config.maxSides),
		sideUids: [...(fixture.sideUids || [])].map(String),
		cities: cloneJson(fixture.cities || []),
		topologyRevision: fixture.topologyRevision ?? 0,
		worldRevision: fixture.worldRevision ?? 0,
		cityRevision: fixture.cityRevision ?? 0,
		dirty: null,
		cityByTile: null,
		ledger: null,
	};
	state.ledger = newLedger(state);
	return state;
}
export function runFixture(fixture) {
	const state = createState(fixture);
	const operationResults = [];
	for (
		let operationIndex = 0;
		operationIndex < (fixture.operations || []).length;
		operationIndex++
	) {
		const operation = fixture.operations[operationIndex] || {};
		let result;
		if (operation.op === "applySources")
			result = applySources(state, operation.sources);
		else if (operation.op === "markCells")
			result = {
				addedDirtyTiles: state.ledger.markCells(
					operation.cellIndices || [],
					operation.includeNeighborTiles !== false,
				),
			};
		else if (operation.op === "mutate")
			result = {
				writes: mutate(state, operation.changes),
				addedDirtyTiles: state.ledger.markCells(
					operation.markCells || [],
					operation.includeNeighborTiles !== false,
				),
			};
		else if (operation.op === "advance")
			result = state.ledger.step(operation.budget);
		else if (operation.op === "flush")
			result = state.ledger.flush(operation.budget);
		else if (operation.op === "reset") {
			for (const influence of state.maps.sideInfluence) influence.fill(0);
			state.maps.dominantSide.fill(-1);
			state.maps.occupation.fill(0);
			state.ledger.reset();
			result = { reset: true };
		} else if (operation.op === "replace") {
			fail(operation.maps, "replace requires maps");
			state.maps = mapsOf({ maps: operation.maps }, state.config);
			state.worldRevision = operation.worldRevision ?? state.worldRevision;
			state.ledger.reset();
			result = { replaced: true, worldRevision: state.worldRevision };
		} else
			throw new Error(
				`territory-control-v1: unsupported operation '${operation.op}'`,
			);
		operationResults.push({
			operationIndex,
			op: operation.op,
			result,
			status: state.ledger.status,
		});
	}
	return {
		schema: SCHEMA,
		config: state.config,
		operationResults,
		final: {
			status: state.ledger.status,
			maps: serialMaps(state.maps),
			snapshot: serializeSnapshot(state, state.ledger.snapshot),
			render: render(state),
		},
	};
}
const percentile = (values, p) => {
	const sorted = [...values].sort((a, b) => a - b);
	return (
		sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * p) - 1)] || 0
	);
};
export function benchmarkFixture(fixture, options = {}) {
	const repeat = positiveInteger(options.repeat, 7);
	const warmup = Math.max(0, integer(options.warmup, 2));
	const ticks = positiveInteger(options.ticks, 3);
	const budget = positiveInteger(options.budget, 16384);
	const sourceBatches = (fixture.operations || [])
		.filter((op) => op.op === "applySources")
		.flatMap((op) => op.sources || []);
	const configuredDirtyCells = fixture.benchmarkDirtyCells || [];
	const dirtyCells =
		configuredDirtyCells.length > 0
			? configuredDirtyCells
			: (fixture.operations || [])
					.filter((op) => op.op === "markCells")
					.flatMap((op) => op.cellIndices || []);
	const once = () => {
		const state = createState(fixture);
		const fullStart = performance.now();
		applySources(state, sourceBatches);
		state.ledger.flush(budget);
		const fullMs = performance.now() - fullStart;
		const persistentStart = performance.now();
		let processedItems = 0;
		let committedGenerations = 0;
		let controllerChanges = 0;
		let creditChanges = 0;
		for (let tick = 0; tick < ticks; tick++) {
			const applied = applySources(state, sourceBatches);
			controllerChanges += applied.controllerChanges;
			creditChanges += applied.creditChanges;
			state.ledger.markCells(dirtyCells);
			const flushed = state.ledger.flush(budget);
			processedItems += flushed.processedItems;
			committedGenerations += flushed.committedGenerations;
		}
		const persistentMs = performance.now() - persistentStart;
		const status = state.ledger.status;
		const remainingItems = Math.max(
			0,
			status.activeTotalItems - status.activeProcessedItems,
		);
		const rendered = render(state);
		return {
			fullMs,
			persistentMs,
			processedItems,
			committedGenerations,
			controllerChanges,
			creditChanges,
			remainingItems,
			activeGeneration: status.activeGeneration,
			dirtyTiles: status.dirtyTiles,
			ownershipProjectionBytes: rendered?.totalBytes || 0,
			checksum: rendered?.checksum || null,
		};
	};
	for (let i = 0; i < warmup; i++) once();
	const samples = Array.from({ length: repeat }, once);
	return {
		schema: SCHEMA,
		mode: "bench",
		repeat,
		warmup,
		ticks,
		budget,
		cells: configOf(fixture).width * configOf(fixture).height,
		sources: sourceBatches.length,
		dirtySeedCells: dirtyCells.length,
		full: {
			medianMs: percentile(
				samples.map((s) => s.fullMs),
				0.5,
			),
			p95Ms: percentile(
				samples.map((s) => s.fullMs),
				0.95,
			),
		},
		persistent: {
			medianMs: percentile(
				samples.map((s) => s.persistentMs),
				0.5,
			),
			p95Ms: percentile(
				samples.map((s) => s.persistentMs),
				0.95,
			),
		},
		processedItems: samples.map((s) => s.processedItems),
		committedGenerations: samples.map((s) => s.committedGenerations),
		controllerChanges: samples.map((s) => s.controllerChanges),
		creditChanges: samples.map((s) => s.creditChanges),
		remainingItems: samples.map((s) => s.remainingItems),
		activeGenerations: samples.map((s) => s.activeGeneration),
		dirtyTiles: samples.map((s) => s.dirtyTiles),
		ownershipProjectionBytes: samples[0]?.ownershipProjectionBytes || 0,
		checksum: samples[0]?.checksum || null,
	};
}
function parseOptions(args) {
	const result = {};
	for (const arg of args) {
		const match = /^--([^=]+)=(.+)$/.exec(arg);
		if (match) result[match[1]] = match[2];
	}
	return result;
}
async function main() {
	const [fixturePath, mode = "report", ...args] = process.argv.slice(2);
	if (!fixturePath || !["report", "bench"].includes(mode))
		throw new Error(
			"usage: js-territory-control-reference.mjs <fixture.json> [report|bench] [--repeat=N --warmup=N --ticks=N --budget=N]",
		);
	const fixture = JSON.parse(await readFile(fixturePath, "utf8"));
	process.stdout.write(
		`${JSON.stringify(mode === "bench" ? benchmarkFixture(fixture, parseOptions(args)) : runFixture(fixture), null, 2)}\n`,
	);
}
if (import.meta.url === `file://${process.argv[1]}`)
	main().catch((error) => {
		console.error(error.stack || error.message);
		process.exitCode = 1;
	});
