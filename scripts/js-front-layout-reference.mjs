#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const MASK_64 = 0xffffffffffffffffn;
const encoder = new TextEncoder();

function usage() {
  throw new Error(
    "usage: js-front-layout-reference.mjs <report|bench> <web-root> <fixture.json> [repeat=20] [warmup=5]",
  );
}

const [mode, webRoot, fixturePath, repeatRaw = "20", warmupRaw = "5"] =
  process.argv.slice(2);
if (!mode || !webRoot || !fixturePath || !["report", "bench"].includes(mode)) {
  usage();
}
const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf8"));
if (fixture.schemaVersion !== "front-layout-v1") {
  throw new Error(`unsupported schema ${fixture.schemaVersion}`);
}

let response = null;
globalThis.self = {
  postMessage(value) {
    response = value;
  },
};
const workerUrl = pathToFileURL(
  path.join(path.resolve(webRoot), "workers/simulation-worker.js"),
);
await import(`${workerUrl.href}?front-layout-reference=1`);

function runWorker() {
  response = null;
  const landMask = Uint8Array.from(fixture.grid.landMask);
  const dominantSideMap = Int8Array.from(fixture.grid.dominantSideMap);
  const hostilityMatrix = Uint8Array.from(fixture.hostilityMatrix);
  globalThis.self.onmessage({
    data: {
      requestId: 1,
      generation: 1,
      territoryGeneration: 1,
      includeField: false,
      includeLayout: true,
      landMask: landMask.buffer,
      dominantSideMap: dominantSideMap.buffer,
      hostilityMatrix: hostilityMatrix.buffer,
      maxSides: fixture.maxSides,
      sideCount: fixture.maxSides,
      gridWidth: fixture.grid.width,
      gridHeight: fixture.grid.height,
      gridRes: fixture.grid.gridRes,
      units: fixture.units,
    },
  });
  if (!response || response.error) {
    throw new Error(response?.error || "front worker returned no response");
  }
  return response;
}

function hashBytes(hash, bytes) {
  let value = hash;
  for (const byte of bytes) {
    value ^= BigInt(byte);
    value = (value * FNV_PRIME) & MASK_64;
  }
  return value;
}

function stableHash(value) {
  const hash = hashBytes(FNV_OFFSET, encoder.encode(value));
  return hash === 0n ? 1n : hash;
}

function uniqueId(namespace, logicalKey, used) {
  for (let collision = 0; ; collision++) {
    const value =
      collision === 0
        ? `${namespace}|${logicalKey}`
        : `${namespace}|${logicalKey}|collision:${collision}`;
    const id = stableHash(value);
    if (!used.has(id)) {
      used.add(id);
      return id;
    }
  }
}

function hostile(left, right) {
  return (
    left !== right &&
    fixture.hostilityMatrix[left * fixture.maxSides + right] === 1
  );
}

function normalizedReport(raw) {
  const usedSegments = new Set();
  const segmentByKey = new Map();
  const segments = Object.entries(raw.polylines).map(([stableKey, points]) => {
    const [left, right] = stableKey.split("_").map(Number);
    const id = uniqueId("front-segment", stableKey, usedSegments);
    const segment = {
      stable_key: stableKey,
      id: id.toString(),
      pair: [left, right],
      points: points.map((point) => ({ lat: point.lat, lng: point.lng })),
    };
    segmentByKey.set(stableKey, { ...segment, id });
    return segment;
  });
  const unitById = new Map(fixture.units.map((unit) => [unit.id, unit]));
  const usedObjectives = new Set();
  const objectives = [];
  const nextPrior = [];
  const assignments = raw.slotAssignments.map((workerAssignment) => {
    const unit = unitById.get(workerAssignment.unitId);
    const pairKey = workerAssignment.pairKey ?? null;
    const segment = pairKey ? segmentByKey.get(pairKey) : null;
    let objectiveId = null;
    if (segment) {
      const opponent =
        segment.pair[0] === unit.sideIndex ? segment.pair[1] : segment.pair[0];
      if (hostile(unit.sideIndex, opponent)) {
        objectiveId = uniqueId(
          "front-objective",
          `${pairKey}|${unit.sideIndex}|${opponent}|${unit.id}`,
          usedObjectives,
        );
        objectives.push({
          id: objectiveId.toString(),
          side_pair: [unit.sideIndex, opponent],
          segment_id: segment.id.toString(),
          lat: workerAssignment.targetLat,
          lng: workerAssignment.targetLng,
          capacity: 1,
          priority: 0,
        });
        nextPrior.push({
          unit_id: String(unit.id),
          pair_key: pairKey,
          segment_idx: workerAssignment.segmentIdx,
          objective_id: objectiveId.toString(),
        });
      }
    }
    return {
      unit_id: String(workerAssignment.unitId),
      pair_key: pairKey,
      segment_id: segment ? segment.id.toString() : null,
      segment_idx: workerAssignment.segmentIdx ?? null,
      target_lat: workerAssignment.targetLat ?? null,
      target_lng: workerAssignment.targetLng ?? null,
      objective_id: objectiveId?.toString() ?? null,
    };
  });
  const sidesWithFronts = new Set(segments.flatMap((segment) => segment.pair));
  const eligibleUnits = fixture.units.filter(
    (unit) =>
      !unit.garrisonExcluded &&
      !(unit.deployTicks > 0) &&
      sidesWithFronts.has(unit.sideIndex),
  ).length;
  const frontierCells = segments.reduce((sum, segment) => {
    const unique = new Set(segment.points.map((point) => `${point.lat}|${point.lng}`));
    return sum + unique.size;
  }, 0);
  const report = {
    schema_version: "front-layout-v1",
    segments,
    assignments,
    objectives,
    next_prior: nextPrior,
    counters: {
      grid_cells: fixture.grid.width * fixture.grid.height,
      frontier_cells: frontierCells,
      segments: segments.length,
      input_units: fixture.units.length,
      eligible_units: eligibleUnits,
      assigned_units: assignments.filter((assignment) => assignment.pair_key !== null)
        .length,
      objectives: objectives.length,
    },
  };
  report.checksum = checksum(report);
  return report;
}

function u64Bytes(value) {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, BigInt(value), true);
  return bytes;
}

function f64Bytes(value) {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setFloat64(0, value, true);
  return bytes;
}

function checksum(report) {
  let hash = FNV_OFFSET;
  for (const segment of report.segments) {
    hash = hashBytes(hash, encoder.encode(segment.stable_key));
    hash = hashBytes(hash, u64Bytes(segment.id));
    for (const point of segment.points) {
      hash = hashBytes(hash, f64Bytes(point.lat));
      hash = hashBytes(hash, f64Bytes(point.lng));
    }
  }
  for (const assignment of report.assignments) {
    hash = hashBytes(hash, u64Bytes(assignment.unit_id));
    if (assignment.pair_key !== null) {
      hash = hashBytes(hash, encoder.encode(assignment.pair_key));
    }
    hash = hashBytes(
      hash,
      u64Bytes(assignment.segment_idx === null ? MASK_64 : assignment.segment_idx),
    );
    hash = hashBytes(hash, u64Bytes(assignment.objective_id ?? 0));
  }
  return hash.toString(16).padStart(16, "0");
}

function percentile(samples, fraction) {
  const index = Math.min(
    samples.length - 1,
    Math.max(0, Math.ceil(samples.length * fraction) - 1),
  );
  return samples[index];
}

if (mode === "report") {
  console.log(JSON.stringify(normalizedReport(runWorker())));
} else {
  const repeat = Number(repeatRaw);
  const warmup = Number(warmupRaw);
  if (!Number.isInteger(repeat) || repeat < 1 || !Number.isInteger(warmup) || warmup < 0) {
    usage();
  }
  for (let index = 0; index < warmup; index++) runWorker();
  const samples = [];
  let finalReport = null;
  for (let index = 0; index < repeat; index++) {
    const started = performance.now();
    const raw = runWorker();
    samples.push(performance.now() - started);
    finalReport = normalizedReport(raw);
  }
  samples.sort((left, right) => left - right);
  console.log(
    JSON.stringify({
      schema_version: "front-layout-v1",
      repeat,
      warmup,
      median_ms: percentile(samples, 0.5),
      p95_ms: percentile(samples, 0.95),
      segments: finalReport.segments.length,
      assignments: finalReport.counters.assigned_units,
      objectives: finalReport.objectives.length,
      checksum: finalReport.checksum,
    }),
  );
}
