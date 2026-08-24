#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { pathToFileURL } from "node:url";

const D=0.7, HEALTH=100, SPEED=0.003, NAVAL=0.025;
let combinedArms = null;
const clone=x=>structuredClone(x);
const wrapGrid=x=>((((x+180)%360)+360)%360)-180;
function wrapOnce(x){if(x>180)return x-360;if(x< -180)return x+360;return x}
function deltaOnce(from,to){return wrapOnce(to-from)}
function idx(g,lat,lng){if(!Number.isFinite(lat)||!Number.isFinite(lng))return -1;const x=Math.floor((wrapGrid(lng)+180)/g.gridRes),y=Math.floor((lat+90)/g.gridRes);return x<0||y<0||x>=g.width||y>=g.height?-1:y*g.width+x}
function dist(a,b){const y=b.lat-a.lat,x=deltaOnce(a.lng,b.lng);return y*y+x*x}
function strength(u){return u.kind==="armor"?1:Math.max(0,u.personnel||0)/1000}
function damageMod(kind,target,terrain){return combinedArms.getArmorCombatMultiplier(kind,target,terrain)}
function quality(q){return combinedArms.getQualityMultiplier(q)}
function move(g,u,o){let lat=u.lat,lng=u.lng,dl=o.dirLat||0,dx=o.dirLng||0,f=o.factors||{};let md=(f.baseSpeed??(u.isAtSea?NAVAL:SPEED))*(f.speedMult??1)*(f.planSpeedMult??1)*(f.neutralPenalty??1)*(f.retreatBoost??1)*(f.pushReadiness??1)*.8;let blocked=false,deflected=false,half=false;if(!u.isTransport){const di=idx(g,lat+dl*md,lng+dx*md);if(di>=0&&!g.landMask[di]&&!u.isAtSea){blocked=true;for(const a of [-90,90,-45,45,-135,135,-30,30]){const r=a*Math.PI/180,cl=dl*Math.cos(r)-dx*Math.sin(r),cx=dl*Math.sin(r)+dx*Math.cos(r);let n=0;for(let s=1;s<=3;s++){const q=idx(g,lat+cl*md*3*s,lng+cx*md*3*s);if(q>=0&&g.landMask[q])n++}if(n<2)continue;const z=Math.hypot(cl,cx);if(z){dl=cl/z;dx=cx/z}const q=idx(g,lat+dl*md,lng+dx*md);if(q>=0&&!g.landMask[q]){md*=.5;half=true}deflected=true;break}if(!deflected){dl=dx=md=0}}}let stuck=blocked?(u.coastStuckTicks||0)+1:0,abandon=false;if(stuck>60){stuck=0;abandon=true}u.lat=Math.max(-89.9,Math.min(89.9,lat+dl*md));u.lng=wrapOnce(lng+dx*md);u.dirLat=dl;u.dirLng=dx;u.coastStuckTicks=stuck;return {abandon,blocked,half,move_dist:md}}
function apply(u,x){if(!Number.isFinite(x)||x<=0||u.health<=0)return {loss:0};const e=Math.min(u.health,x);let loss=0;if(u.kind==="armor"){const before=u.equipment||0,next=Math.min(before,Math.max(0,Math.ceil((u.maxEquipment||before)*((u.health-e)/HEALTH))));loss=(before-next)*combinedArms.COMBINED_ARMS_CONFIG.ARMOR_CREW_PER_VEHICLE;u.equipment=next}else{const before=u.personnel||0,cap=Math.max(before,u.personnelCapacity||before),next=Math.min(before,Math.max(0,Math.round(cap*((u.health-e)/Math.max(1,u.maxHealth||HEALTH)))));loss=before-next;u.personnel=next;if(!next)u.health=0}u.health=Math.max(0,u.health-e);return {loss}}
function push(u,la,ln,g){const a=Math.max(-89.9,Math.min(89.9,u.lat+la)),b=wrapOnce(u.lng+ln),q=idx(g,a,b);if(q>=0&&!g.landMask[q]&&!u.isAtSea)return true;u.lat=a;u.lng=b;return false}
function prox(a,b,c){const d=dist(a,b);if(d>=.09||c.frame<c.warGraceEnd)return null;let p=D*.45*(c.damageDealtMult??1)*(1-Math.sqrt(d)/.3),self=0;if(a.isAtSea&&b.isAtSea)p*=2.2;if(b.isTransport&&!a.isTransport)p*=1.05;if(a.isTransport&&!b.isTransport){self=p*1.05*(c.damageTakenMult??1);apply(a,self);p*=.85}const td=p*damageMod(a.kind,b.kind,{mountain:c.mountain,urban:c.urban,supported:a.armorSupported})*(a.kind==="armor"?quality(a.quality):1)*(a.kind==="armor"&&a.armorLandingPenaltyUntilTick>c.tick?.3:1)*(a.kind==="army"?strength(a):1);apply(b,td);const ad=p*.8*(c.damageTakenMult??1)*damageMod(b.kind,a.kind,{mountain:c.mountain,urban:c.urban,supported:b.armorSupported})*(b.kind==="armor"?quality(b.quality):1)*(b.kind==="army"?strength(b):1);apply(a,ad);a.lastCombatTick=b.lastCombatTick=c.frame;if(b.health<=0)a.victoryBoostTicks=240;return {layer:"proximity",attacker_id:a.id,target_id:b.id,target_damage:td,attacker_damage:ad,transport_self_damage:self,target_health:b.health,self_health:a.health}}
function direct(a,b,c,g){const y=b.lat+Math.sin(a.id*100)*.08,x=deltaOnce(a.lng,b.lng+Math.cos(a.id*100)*.08);if(Math.hypot(y-a.lat,x)>.05)return null;const terrain={mountain:c.mountain,urban:c.urban},td=D*(c.damageDealtMult??1)*.7*damageMod(a.kind,b.kind,{...terrain,supported:a.armorSupported})*(a.kind==="armor"?quality(a.quality):1)*(a.kind==="armor"&&a.armorLandingPenaltyUntilTick>c.tick?.3:1)*(a.kind==="army"?strength(a):1),ad=D*.8*(c.damageTakenMult??1)*(c.defenseBonus??1)*(c.longWarDefense??1)*damageMod(b.kind,a.kind,{...terrain,supported:b.armorSupported})*(b.kind==="armor"?quality(b.quality):1)*(b.kind==="army"?strength(b):1);apply(b,td);apply(a,ad);a.lastCombatTick=b.lastCombatTick=c.frame;const q=dist(a,b);let tb=false,ab=false;if(q>0){const z=Math.sqrt(q),nx=deltaOnce(a.lng,b.lng)/z,ny=(b.lat-a.lat)/z,base=(a.isAtSea?NAVAL:SPEED)*1.2,total=td+ad||1e-6;tb=push(b,ny*base*Math.min(1.5,td/total*1.5),nx*base*Math.min(1.5,td/total*1.5),g);ab=push(a,-ny*base*.5*Math.min(1,ad/total),-nx*base*.5*Math.min(1,ad/total),g)}if(b.health<=0)a.victoryBoostTicks=180;return {layer:"direct",attacker_id:a.id,target_id:b.id,target_damage:td,attacker_damage:ad,target_health:b.health,self_health:a.health,target_knockback_blocked:tb,self_knockback_blocked:ab}}
function report(u){return {id:u.id,side:u.side,sovereign:u.sovereign,kind:u.kind,lat:u.lat,lng:u.lng,health:u.health,personnel:u.personnel??null,equipment:u.equipment??null,dir_lat:u.dirLat||0,dir_lng:u.dirLng||0,coast_stuck_ticks:u.coastStuckTicks||0,last_combat_tick:u.lastCombatTick||0,victory_boost_ticks:u.victoryBoostTicks||0}}
function run(f, tactical) {
	let us = f.units.map(clone);
	const steps = [];
	const orders = new Map((f.orders || []).map((order) => [order.unitId, order]));
	const cellSize = f.config?.tacticalCellSize ?? 0.6;
	const tacticalOptions = {
		cellSize,
		getSide: (unit) => unit.side,
		getStrength: strength,
		getAllyWeight: (unit) => unit.allyWeight ?? 1,
		isArmor: (unit) => unit.kind === "armor",
		isSupport: (unit) => unit.isSupport === true,
	};
	let tacticalGrid = null;

	for (let s = 0; s < f.steps; s++) {
		const tick = f.tick + s;
		const frame = f.frame + s;
		const snapshot = us
			.filter(
				(unit) =>
					unit.health > 0 &&
					(unit.kind === "armor" || unit.personnel > 0),
			)
			.map((unit) => ({ ...unit }));
		if (tacticalGrid === null) {
			tacticalGrid = tactical.buildTacticalGrid(snapshot, tacticalOptions);
		} else {
			tactical.rebuildTacticalGrid(tacticalGrid, snapshot, tacticalOptions);
		}

		const unitById = new Map(us.map((unit) => [unit.id, unit]));
		const events = [];
		const removed = [];
		const counts = {
			proximity_contacts: 0,
			direct_contacts: 0,
			movement: 0,
		};

		for (let i = us.length - 1; i >= 0; i--) {
			const attacker = us[i];
			if (
				attacker.health <= 0 ||
				(attacker.kind !== "armor" && attacker.personnel <= 0)
			) {
				continue;
			}
			const order = orders.get(attacker.id) || { movementEnabled: false };
			const candidates = [];
			for (let targetSide = 0; targetSide < f.maxSides; targetSide++) {
				if (!isHostile(f, attacker.side, targetSide)) continue;
				tactical.forEachNeighborCell(
					tacticalGrid,
					targetSide,
					attacker,
					(cell) => candidates.push(...cell.units),
					{ radiusCells: Math.ceil(0.3 / cellSize) },
				);
			}
			candidates.sort((left, right) => left.id - right.id);
			const accepted = candidates.filter(
				(target) =>
					target.id !== attacker.id &&
					isHostile(f, attacker.side, target.side) &&
					dist(attacker, target) < 0.09,
			);
			let target =
				accepted.find((candidate) => candidate.id === order.preferredTargetId) ||
				accepted[0];
			const context = {
				tick,
				frame,
				warGraceEnd: f.warGraceEnd || 0,
				...order.combat,
			};
			for (const targetSnapshot of accepted) {
				const liveTarget = unitById.get(targetSnapshot.id);
				if (liveTarget && liveTarget.health > 0) {
					const event = prox(attacker, liveTarget, context);
					if (event) {
						events.push(event);
						counts.proximity_contacts++;
					}
				}
			}
			target = target && unitById.get(target.id);
			const event =
				target && target.health > 0
					? direct(attacker, target, context, f.grid)
					: null;
			if (event) {
				events.push(event);
				counts.direct_contacts++;
			} else if (order.movementEnabled) {
				const movement = move(f.grid, attacker, order);
				counts.movement++;
				if (movement.abandon) attacker._abandoned = true;
			}
		}

		for (let i = us.length - 1; i >= 0; i--) {
			if (
				us[i].health <= 0 ||
				(us[i].kind === "armor" && us[i].equipment <= 0) ||
				(us[i].kind !== "armor" && us[i].personnel <= 0)
			) {
				removed.push(us[i].id);
				us.splice(i, 1);
			}
		}
		removed.sort((left, right) => left - right);
		steps.push({
			tick,
			frame,
			events,
			removed,
			counts,
			units: us.map(report).sort((left, right) => left.id - right.id),
		});
	}
	return {
		schema: "native-tick-v2",
		steps,
		final_units: us.map(report).sort((left, right) => left.id - right.id),
	};
}

function isHostile(fixture, attackerSide, targetSide) {
	if (attackerSide === targetSide) return false;
	return (
		fixture.hostilityRelations?.[`${attackerSide}:${targetSide}`] ?? true
	);
}

async function main() {
	const [mode, web, fixture, repeat = "20", warmup = "5"] =
		process.argv.slice(2);
	if (!mode || !web || !fixture) {
		throw Error(
			"usage: report|bench <web-root> <fixture.json> [repeat] [warmup]",
		);
	}
	const f = JSON.parse(await readFile(fixture, "utf8"));
	const tactical = await import(
		pathToFileURL(resolve(web, "src/tactical-grid.js")).href
	);
	combinedArms = await import(
		pathToFileURL(resolve(web, "src/combined-arms.js")).href
	);
	for (const name of [
		"buildTacticalGrid",
		"rebuildTacticalGrid",
		"forEachNeighborCell",
	]) {
		if (typeof tactical[name] !== "function") {
			throw new TypeError(`web tactical grid is missing ${name}`);
		}
	}
	for (const name of ["getArmorCombatMultiplier", "getQualityMultiplier"]) {
		if (typeof combinedArms[name] !== "function") {
			throw new TypeError(`web combined-arms module is missing ${name}`);
		}
	}
	if (mode === "report") {
		console.log(JSON.stringify(run(f, tactical), null, 2));
		return;
	}
	if (mode !== "bench") throw new Error(`unknown mode: ${mode}`);
	for (let i = 0; i < +warmup; i++) run(f, tactical);
	const timings = [];
	let checksum = 0;
	for (let i = 0; i < +repeat; i++) {
		const start = performance.now();
		const result = run(f, tactical);
		const elapsed = performance.now() - start;
		timings.push(elapsed);
		// Report serialization is intentionally outside the measured interval.
		checksum += JSON.stringify(result).length;
	}
	timings.sort((left, right) => left - right);
	console.log(
		JSON.stringify({
			steps: f.steps,
			units: f.units.length,
			repeat: +repeat,
			median_ms: timings[Math.floor(timings.length / 2)],
			p95_ms: timings[Math.floor(timings.length * 0.95)],
			checksum,
		}),
	);
}

main();
