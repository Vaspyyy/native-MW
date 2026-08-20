#!/usr/bin/env node

import fs from "node:fs";

const [leftPath, rightPath] = process.argv.slice(2);
if (!leftPath || !rightPath) {
  throw new Error("usage: compare-front-layout-parity.mjs <left.json> <right.json>");
}

const left = JSON.parse(fs.readFileSync(leftPath, "utf8"));
const right = JSON.parse(fs.readFileSync(rightPath, "utf8"));

function compare(a, b, location = "$") {
  if (typeof a === "number" && typeof b === "number") {
    const tolerance = 1e-10 * Math.max(1, Math.abs(a), Math.abs(b));
    if (!Number.isFinite(a) || !Number.isFinite(b) || Math.abs(a - b) > tolerance) {
      throw new Error(`${location}: ${a} != ${b}`);
    }
    return;
  }
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) {
      throw new Error(`${location}: array shape mismatch`);
    }
    for (let index = 0; index < a.length; index++) {
      compare(a[index], b[index], `${location}[${index}]`);
    }
    return;
  }
  if (a && b && typeof a === "object" && typeof b === "object") {
    const aKeys = Object.keys(a).sort();
    const bKeys = Object.keys(b).sort();
    if (JSON.stringify(aKeys) !== JSON.stringify(bKeys)) {
      throw new Error(`${location}: object keys mismatch`);
    }
    for (const key of aKeys) compare(a[key], b[key], `${location}.${key}`);
    return;
  }
  if (a !== b) throw new Error(`${location}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`);
}

compare(left, right);
console.log("front layout parity: ok");
