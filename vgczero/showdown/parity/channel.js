'use strict';
// FE-T 4: per-side channel parity. For the same scenarios exact.js uses, renders the engine log as
// each side sees it (`log_side` = own HP exact, the other side's as the Champions shared percentage)
// and compares it to that side's Showdown channel (`extractChannelMessages(log, [side])`), reduced to
// the lines the tracker consumes. Reports, per side, how many scenarios match and, when not, the
// missing/extra line types (like exact.js --log, but per channel).
//
//   node channel.js [--battles 60] [--seed 3] [--sample 0.35] [--limit 10]
const path = require('path');
const crypto = require('crypto');
const L = require('./lib');
const pinned = require('../pinned');
const fs = require('fs');
const {extractChannelMessages} = require(path.join(L.PS_DIR, 'dist', 'sim', 'battle'));
const {IGNORED} = require(path.join(pinned.root, 'model_train', 'env', 'tracker.cjs'));
const {instrument} = require('../../showdown/oracle.cjs');
const {compileWhitelist} = require('../../showdown/tie_policy.cjs');
// Same oracle as exact.js: the tie-determinization whitelist removes RNG draws for verified
// artificial ties, so Showdown's stream and the engine's agree. Plain Showdown does not.
const policy = compileWhitelist(JSON.parse(fs.readFileSync(path.join(pinned.root,
  'model_train/artifacts/evaluation/fast-engine-state-20261024/distribution/whitelist.verified.json'), 'utf8')));

function arg(name, dflt) {
	const i = process.argv.indexOf(name);
	return i >= 0 ? process.argv[i + 1] : dflt;
}
const BATTLES = Number(arg('--battles', 60));
const SEED = Number(arg('--seed', 3));
const SAMPLE = Number(arg('--sample', 0.35));
const LIMIT = Number(arg('--limit', 10));
const sodium = tag => 'sodium,' + crypto.createHash('sha256').update(`exact/${tag}`).digest('hex');

function choose(battle, choices) {
	for (let i = 0; i < 2; i++) {
		const c = choices[i];
		if (!c) continue;
		const side = battle.sides[i];
		if (!battle.choose(side.id, L.choiceString(side, c))) throw new Error(`choice rejected: ${side.id}`);
	}
}
function sortedPreview(order) {
	return [...order.slice(0, 2).sort((a, b) => a - b), ...order.slice(2, 4).sort((a, b) => a - b)];
}
function showdownLog(sc) {
	const b = sc.serialized ? L.deserialize(sc.serialized) : L.newBattle(sc.teams[0], sc.teams[1], sc.initialSeed);
	instrument(b, {onTie: policy.onTie, recordRng: true, allowDeterminization: true});
	const logStart = b.log.length;
	choose(b, sc.choices);
	return b.log.slice(logStart);
}
function generate() {
	const pool = L.loadPool();
	const sup = L.supportedTeams();
	const rand = new L.Rand(SEED);
	const out = [];
	for (let g = 0; g < BATTLES; g++) {
		const ta = pool[rand.pick(sup)], tb = pool[rand.pick(sup)];
		const initialSeed = sodium(`${SEED}.${g}`);
		const battle = L.newBattle(ta, tb, initialSeed);
		let steps = 0;
		while (!battle.ended && steps < 120) {
			steps++;
			const choices = battle.sides.map(s => L.randomChoice(battle, s, rand, {allyDamage: rand.chance(0.1)}));
			const preview = battle.requestState === 'teampreview';
			if (preview) for (const c of choices) c.preview = sortedPreview(c.preview);
			if (rand.chance(preview ? 0.5 : SAMPLE)) {
				out.push({id: `x${SEED}.${g}.${steps}`, kind: battle.requestState, serialized: preview ? null : L.serialize(battle),
					teams: [ta, tb], initialSeed, seed: battle.prng.getSeed(), state: preview ? null : L.exportState(battle, [ta, tb]),
					choices, engineChoices: choices.map((c, i) => L.engineChoice(battle.sides[i], c))});
			}
			try { choose(battle, choices); } catch (e) { break; }
		}
	}
	return out;
}
const trackerLines = lines => lines.filter(l => !IGNORED.has(l.split('|')[1] || ''));
function multisetDiff(a, b) {
	const count = new Map();
	for (const l of a) count.set(l, (count.get(l) || 0) + 1);
	for (const l of b) count.set(l, (count.get(l) || 0) - 1);
	const onlyA = [], onlyB = [];
	for (const [l, n] of count) { for (let i = 0; i < n; i++) onlyA.push(l); for (let i = 0; i < -n; i++) onlyB.push(l); }
	return {onlyA, onlyB};
}

function run() {
	const scenarios = generate();
	const base = sc => sc.state ? {id: sc.id, state: sc.state, seed: sc.seed, choices: sc.engineChoices}
		: {id: sc.id, teams: sc.teams, seed: sc.initialSeed, choices: sc.engineChoices};
	const eng = [0, 1].map(side => new Map(L.runEngine(['exact'], scenarios.map(sc => ({...base(sc), log_side: side}))).map(r => [r.id, r])));
	const stat = [{same: 0, diff: 0, missing: {}, extra: {}}, {same: 0, diff: 0, missing: {}, extra: {}}];
	const samples = [];
	const tally = (o, k) => { o[k] = (o[k] || 0) + 1; };
	for (const sc of scenarios) {
		let sdLog;
		try { sdLog = showdownLog(sc); } catch (e) { continue; }
		const channels = extractChannelMessages(sdLog.join('\n'), [1, 2]);
		for (const side of [0, 1]) {
			const e = eng[side].get(sc.id);
			if (!e || e.err) continue;
			const want = trackerLines(channels[side + 1]);
			const got = trackerLines(e.log || []);
			const {onlyA, onlyB} = multisetDiff(want, got);
			if (!onlyA.length && !onlyB.length) stat[side].same++;
			else {
				stat[side].diff++;
				for (const l of onlyA) tally(stat[side].missing, l.split('|')[1] || '');
				for (const l of onlyB) tally(stat[side].extra, l.split('|')[1] || '');
				if (samples.length < LIMIT) samples.push({id: sc.id, side: side + 1, missing: onlyA.slice(0, 4), extra: onlyB.slice(0, 4)});
			}
		}
	}
	const top = o => Object.entries(o).sort((a, b) => b[1] - a[1]).slice(0, 15).map(([k, v]) => `${k} ${v}`).join(', ');
	for (const side of [0, 1]) {
		const s = stat[side];
		console.log(`p${side + 1} channel: ${s.same}/${s.same + s.diff} scenarios identical`);
		console.log(`   engine lacks (by type): ${top(s.missing)}`);
		console.log(`   engine extra (by type): ${top(s.extra)}`);
	}
	for (const x of samples) console.log(`   [${x.id} p${x.side}] missing ${JSON.stringify(x.missing)} extra ${JSON.stringify(x.extra)}`);
}

run();
