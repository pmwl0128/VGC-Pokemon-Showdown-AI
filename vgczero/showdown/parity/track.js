'use strict';
// ② (native tracker) fixture generator. Plays Showdown battles in the training env, which hands
// each side its own channel of the protocol (`|split|` secrets) plus its own request -- exactly the
// input the JS tracker consumes. At every decision point it feeds the side's lines and request to
// the JS tracker (the reference) and records: the lines, the request, a canonical JSON of the
// tracker state, and the v3 encoded tensor + masks. A native tracker replays the same lines and
// must reproduce the canonical state; the native encoder must reproduce the tensors.
//
//   node track.js [--games 20] [--seed 3] [--out <fixtures/tracker.jsonl.gz>]
//
// Output is a gzipped JSONL, one record per decision point per side. Reading it needs no Showdown.
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const pinned = require('../pinned');
const L = require('./lib');
const {DoublesEnv} = require('../../../../env/battle_env.cjs');
const {Tracker} = require('../../../../env/tracker.cjs');
const {encode, masks} = require('../../../../env/encoder.cjs');

function arg(name, dflt) {
	const i = process.argv.indexOf(name);
	return i >= 0 ? process.argv[i + 1] : dflt;
}
const GAMES = Number(arg('--games', 20));
const SEED = Number(arg('--seed', 3));
const OUT = arg('--out', path.join(pinned.DATA_DIR, 'fixtures', 'tracker.jsonl.gz'));
const sodium = tag => 'sodium,' + require('crypto').createHash('sha256').update(`track/${tag}`).digest('hex');

function sortedPreview(order) {
	return [...order.slice(0, 2).sort((a, b) => a - b), ...order.slice(2, 4).sort((a, b) => a - b)];
}

// Canonical, key-sorted JSON so the Rust side can match it byte for byte from its own state.
function sortedJSON(v) {
	if (Array.isArray(v)) return '[' + v.map(sortedJSON).join(',') + ']';
	if (v && typeof v === 'object') {
		return '{' + Object.keys(v).sort().map(k => JSON.stringify(k) + ':' + sortedJSON(v[k])).join(',') + '}';
	}
	return JSON.stringify(v === undefined ? null : v);
}

function canonMon(m) {
	if (!m) return null;
	return {
		index: m.index, name: m.name, species: m.species, forme: m.forme, item: m.item, ability: m.ability,
		baseAbility: m.baseAbility, moves: m.moves, nature: m.nature, gender: m.gender,
		hp: m.hp, exact: m.exact, status: m.status, fainted: m.fainted, seen: m.seen, brought: m.brought, slot: m.slot,
		sleepElapsed: m.sleepElapsed, sleepRest: m.sleepRest, enteredTurn: m.enteredTurn, movesSinceEntry: m.movesSinceEntry,
		stall: m.stall, toxStage: m.toxStage, lastMoveTurn: m.lastMoveTurn, boosts: m.boosts, volatiles: m.volatiles,
		types: m.types, megaEvolved: m.megaEvolved, itemGone: m.itemGone, stats: m.stats, lastMove: m.lastMove,
		snapshot: m.snapshot, commanding: m.commanding, identityConfirmed: m.identityConfirmed,
		confusedWith: m.confusedWith,
	};
}

function canonSide(s) {
	return {id: s.id, player: s.player, teamSize: s.teamSize, megaUsed: s.megaUsed, sheet: s.sheet,
		active: s.active.slice(), conditions: s.conditions, names: s.names, team: s.team.map(canonMon)};
}

function canonTracker(t) {
	return {
		me: t.me, foe: t.foe, spectator: t.spectator, phase: t.phase, turn: t.turn, order: t.order,
		ended: t.ended, winner: t.winner, hiddenLockUntil: t.hiddenLockUntil,
		lastMover: t.lastMover ? t.lastMover.index : null,
		field: {weather: t.field.weather, terrain: t.field.terrain, pseudo: t.field.pseudo},
		sides: {p1: canonSide(t.sides.p1), p2: canonSide(t.sides.p2)},
		history: t.history.map(h => ({
			turn: h.turn,
			actions: h.actions.map(a => ({side: a.side, slot: a.slot, actor: a.actor, kind: a.kind, id: a.id,
				target: a.target ? {side: a.target.side, slot: a.target.slot} : null,
				spread: !!a.spread, missed: !!a.missed, order: a.order, phase: a.phase})),
		})),
		lastMoveBySlot: t.lastMoveBySlot,
	};
}

function play(teamA, teamB, seed) {
	const env = new DoublesEnv({turnLimit: 200});
	env.reset({p1: teamA.mons.map((m, i) => ({...L.toSet(m), vgczIdx: i})), p2: teamB.mons.map((m, i) => ({...L.toSet(m), vgczIdx: i})), seed});
	const rand = new L.Rand(seed);
	const trackers = {p1: new Tracker('p1'), p2: new Tracker('p2')};
	const steps = [];
	for (let guard = 0; !env.done && guard < 500; guard++) {
		const pending = env.pending();
		if (!pending.length) break;
		for (const id of pending) {
			const lines = env.drain(id);
			trackers[id].feed(lines);
			const request = env.request(id);
			trackers[id].observeRequest(request);
			const obs = encode(trackers[id]);
			const m = masks(request);
			steps.push({id, turn: env.battle.turn, lines, request,
				state: sortedJSON(canonTracker(trackers[id])),
				obs: {cats: Array.from(obs.cats), floats: Array.from(obs.floats), switchMap: Array.from(obs.switchMap), kind: obs.kind},
				masks: {kind: m.kind, preview: m.preview ? Array.from(m.preview) : null,
					mask0: m.mask0 ? Array.from(m.mask0) : null, mask1: m.mask1 ? Array.from(m.mask1) : null}});
		}
		for (const id of pending) {
			const side = env.side(id);
			const c = L.randomChoice(env.battle, side, rand, {allyDamage: rand.chance(0.1)});
			if (env.battle.requestState === 'teampreview') c.preview = sortedPreview(c.preview);
			env.choose(id, L.choiceString(side, c));
		}
	}
	return steps;
}

function main() {
	const pool = L.loadPool();
	const sup = L.supportedTeams();
	const rand = new L.Rand(SEED);
	const games = [];
	for (let g = 0; g < GAMES; g++) {
		const ta = pool[rand.pick(sup)], tb = pool[rand.pick(sup)];
		games.push(play(ta, tb, sodium(`${SEED}.${g}`)));
	}
	fs.mkdirSync(path.dirname(OUT), {recursive: true});
	const lines = [];
	for (let g = 0; g < games.length; g++) {
		lines.push(JSON.stringify({game: g, seed: sodium(`${SEED}.${g}`), steps: games[g]}));
	}
	fs.writeFileSync(OUT, zlib.gzipSync(lines.join('\n') + '\n'));
	const steps = games.reduce((n, g) => n + g.length, 0);
	console.log(`wrote ${OUT}: ${GAMES} games, ${steps} decision steps`);
}

main();
