'use strict';
// FE-T 4 whole-episode, same-seed lockstep (PLAN §4.2). For each candidate game, Showdown is played
// to the end with random legal choices (with the tie-determinization whitelist oracle, the same
// reproduction path as exact.js). Two engine comparisons are then made, side by side:
//
//   continuous (`parity episode`): the engine runs ONE continuous game applying the recorded
//     per-decision-point choice sequence. A divergence here can be a local bug or accumulated drift
//     from an invisible state difference -- this is `episode_match`.
//   per-step   (`parity exact`):    each decision point is replayed independently from the oracle's
//     normalized state + RNG at that boundary -- the contract-5 `step_match` method.
//
// A decision whose channel matches per-step but NOT continuously is an accumulated (invisible)
// divergence; one that fails both is a local divergence. Channels compare as the tracker consumes
// them (exact multiset, order-independent). Reports coverage / step_match / episode_match /
// critical_errors plus the cumulative-vs-local split.
//
//   node episode.js [--games 30] [--seed 3] [--limit 8]

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const L = require('./lib');
const pinned = require('../pinned');
const {extractChannelMessages} = require(path.join(L.PS_DIR, 'dist', 'sim', 'battle'));
const {IGNORED} = require(path.join(pinned.root, 'model_train', 'env', 'tracker.cjs'));
const {instrument} = require('../../showdown/oracle.cjs');
const {compileWhitelist} = require('../../showdown/tie_policy.cjs');
// Same oracle as exact.js / channel.js: the whitelist removes RNG draws for verified artificial
// ties so Showdown's stream matches the engine's. Plain Showdown does not.
const policy = compileWhitelist(JSON.parse(fs.readFileSync(path.join(pinned.root,
	'model_train/artifacts/evaluation/fast-engine-state-20261024/distribution/whitelist.verified.json'), 'utf8')));

function arg(name, dflt) {
	const i = process.argv.indexOf(name);
	return i >= 0 ? process.argv[i + 1] : dflt;
}
const GAMES = Number(arg('--games', 30));
const SEED = Number(arg('--seed', 3));
const LIMIT = Number(arg('--limit', 8));
const DUMP = arg('--dump', null);
const CROSS = process.argv.includes('--cross');
const sodium = tag => 'sodium,' + crypto.createHash('sha256').update(`exact/${tag}`).digest('hex');

const trackerLines = lines => lines.filter(l => !IGNORED.has(l.split('|')[1] || ''));

function sortedPreview(order) {
	return [...order.slice(0, 2).sort((a, b) => a - b), ...order.slice(2, 4).sort((a, b) => a - b)];
}
function choose(b, choices) {
	for (let i = 0; i < 2; i++) {
		const c = choices[i];
		if (!c) continue;
		const side = b.sides[i];
		if (!b.choose(side.id, L.choiceString(side, c))) throw new Error(`choice rejected: ${side.id}`);
	}
}

// Play one game to the end in Showdown; record, per decision point, the oracle state + RNG seed +
// engine choices (for the per-step replay), the turn, and each side's tracker-consumed channel.
function play(ta, tb, initialSeed, rand) {
	const b = L.newBattle(ta, tb, initialSeed);
	instrument(b, {onTie: policy.onTie, recordRng: true, allowDeterminization: true});
	const scen = [], sd = [], turns = [];
	let logStart = b.log.length, guard = 0;
	while (!b.ended && guard++ < 300) {
		const step = b.sides.map(s => L.randomChoice(b, s, rand, {allyDamage: rand.chance(0.1)}));
		if (b.requestState === 'teampreview') for (const c of step) c.preview = sortedPreview(c.preview);
		const eng = step.map((c, i) => L.engineChoice(b.sides[i], c));
		scen.push({preview: b.requestState === 'teampreview', state: L.exportState(b, [ta, tb]), seed: b.prng.getSeed(), choices: eng,
			ser: L.serialize(b), sdc: step.map((c, i) => (c ? L.choiceString(b.sides[i], c) : null))});
		turns.push(b.turn);
		choose(b, step);
		const slice = b.log.slice(logStart);
		logStart = b.log.length;
		const ch = extractChannelMessages(slice.join('\n'), [1, 2]);
		sd.push([trackerLines(ch[1]), trackerLines(ch[2])]);
	}
	const winner = !b.ended ? null : (b.winner === 'p1' ? 0 : b.winner === 'p2' ? 1 : 2);
	return {scen, sd, turns, choices: scen.map(s => s.choices), ended: !!b.ended, winner};
}

function sameChan(a, b) {
	if (a.length !== b.length) return false;
	const m = new Map();
	for (const x of a) m.set(x, (m.get(x) || 0) + 1);
	for (const x of b) m.set(x, (m.get(x) || 0) - 1);
	for (const v of m.values()) if (v !== 0) return false;
	return true;
}
function diffLines(got, want) {
	return {missing: want.filter(x => !got.includes(x)).slice(0, 4), extra: got.filter(x => !want.includes(x)).slice(0, 4)};
}

function main() {
	const pool = L.loadPool();
	const sup = L.supportedTeams();
	const rand = new L.Rand(SEED);
	const games = [];
	for (let g = 0; g < GAMES; g++) {
		const ta = pool[rand.pick(sup)], tb = pool[rand.pick(sup)];
		const id = `e${SEED}.${g}`;
		games.push({id, ta, tb, initialSeed: sodium(`${SEED}.${g}`), ...play(ta, tb, sodium(`${SEED}.${g}`), rand)});
	}
	// Continuous engine run (one game per candidate).
	const cont = [0, 1].map(side => new Map(L.runEngine(['episode'],
		games.map(g => ({id: g.id, teams: [g.ta, g.tb], seed: g.initialSeed, choices: g.choices, log_side: side})))
		.map(r => [r.id, r])));
	// Per-step replay from each oracle decision boundary. Team preview has no pre-step state, so it
	// uses the teams + initial seed path (like exact.js / channel.js), not from_state_json.
	const perInput = (g, s, k, side) => s.preview
		? {id: `${g.id}.${k}`, teams: [g.ta, g.tb], seed: g.initialSeed, choices: s.choices, log_side: side}
		: {id: `${g.id}.${k}`, state: s.state, seed: s.seed, choices: s.choices, log_side: side};
	const per = [0, 1].map(side => new Map(L.runEngine(['exact'],
		games.flatMap(g => g.scen.map((s, k) => perInput(g, s, k, side))))
		.map(r => [r.id, r])));

	const samples = [];
	let covered = 0, epTotal = 0, epOk = 0, contStepTotal = 0, contStepOk = 0, badSteps = 0, badGames = 0;
	let perTotal = 0, perOk = 0, cumOnly = 0;
	for (const g of games) {
		const cs = cont.map(m => m.get(g.id));
		const ok = cs.every(e => e && !e.err) && cs[0].steps.length === g.sd.length;
		if (ok) covered++;
		epTotal++;
		let epAll = true;
		for (let k = 0; k < g.sd.length; k++) {
			// per-step (oracle-seeded) match
			perTotal++;
			const ps = [0, 1].map(s => per[s].get(`${g.id}.${k}`));
			const pOk = ps[0] && !ps[0].err && ps[1] && !ps[1].err &&
				sameChan(trackerLines(ps[0].log || []), g.sd[k][0]) && sameChan(trackerLines(ps[1].log || []), g.sd[k][1]);
			if (pOk) perOk++;
			// continuous match (needs the run to have reached this step, on the same turn)
			contStepTotal++;
			if (DUMP && `${g.id}.step${k}` === DUMP && ok) {
				console.log(`DUMP ${g.id}.step${k} turn sd=${g.turns[k]} eng=${cs[0].steps[k] && cs[0].steps[k].turn}`);
				for (let s = 0; s < 2; s++) console.log(`  SD s${s + 1} ${JSON.stringify(g.sd[k][s])}\n  EN s${s + 1} ${JSON.stringify(trackerLines(cs[s].steps[k].log || []))}`);
			}
			let cOk = false, why = null;
			if (ok && cs[0].steps[k] && cs[0].steps[k].turn === g.turns[k]) {
				const m0 = sameChan(trackerLines(cs[0].steps[k].log || []), g.sd[k][0]);
				const m1 = sameChan(trackerLines(cs[1].steps[k].log || []), g.sd[k][1]);
				cOk = m0 && m1;
				if (!cOk) {
					const s = m0 ? 1 : 0;
					why = {side: s + 1, ...diffLines(trackerLines(cs[s].steps[k].log || []), g.sd[k][s]), perStepOk: pOk};
				}
			} else if (ok) {
				why = {desync: `sd turn ${g.turns[k]} vs eng turn ${cs[0].steps[k] ? cs[0].steps[k].turn : null}`};
			} else {
				why = {uncovered: (cs[0] && cs[0].err) || (cs[1] && cs[1].err) || 'engine did not reach this step'};
			}
			if (cOk) contStepOk++;
			else {
				badSteps++; epAll = false;
				if (pOk) cumOnly++;
				if (samples.length < LIMIT) samples.push({id: `${g.id}.step${k}`, ...why});
			}
		}
		if (epAll && ok) epOk++; else if (!epAll) badGames++;
	}
	const pct = (n, d) => d ? (100 * n / d).toFixed(2) + '%' : '-';
	if (CROSS) {
		let diff = 0;
		for (const g of games) for (let k = 0; k < g.sd.length; k++) {
			if (g.turns[k] === 0) continue; // team preview: no pre-step state to replay
			const b2 = L.deserialize(g.scen[k].ser);
			instrument(b2, {onTie: policy.onTie, recordRng: true, allowDeterminization: true});
			const ls = b2.log.length;
			for (let i = 0; i < 2; i++) if (g.scen[k].sdc[i]) b2.choose(b2.sides[i].id, g.scen[k].sdc[i]);
			const ch = extractChannelMessages(b2.log.slice(ls).join('\n'), [1, 2]);
			for (let s = 0; s < 2; s++) {
				if (!sameChan(trackerLines(ch[s + 1]), g.sd[k][s])) { diff++; if (diff <= 5) console.log(`   [cross] ${g.id}.step${k} s${s + 1} differs`); }
			}
		}
		console.log(`cross-check: continuous vs deserialize-replay SD channel differs at ${diff} side-steps`);
	}
	console.log(`candidates ${GAMES}  seed ${SEED}`);
	console.log(`coverage      ${covered}/${epTotal}  ${pct(covered, epTotal)}   (engine ran the whole episode)`);
	console.log(`step_match    continuous ${contStepOk}/${contStepTotal}  ${pct(contStepOk, contStepTotal)}   per-step ${perOk}/${perTotal}  ${pct(perOk, perTotal)}`);
	console.log(`episode_match ${epOk}/${epTotal}  ${pct(epOk, epTotal)}`);
	console.log(`critical_errors  continuous steps ${badSteps}  games ${badGames}  (of which per-step-OK / cumulative: ${cumOnly})`);
	for (const s of samples) console.log(`   [${s.id}] ${JSON.stringify(s)}`);
	process.exitCode = badSteps || (epTotal - covered) ? 1 : 0;
}

main();
