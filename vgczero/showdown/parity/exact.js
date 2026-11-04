'use strict';
// Exact (same-seed) parity: every sampled decision point of random battles is replayed once in
// the pinned Showdown, with this track's verified tie whitelist, and once in the engine's
// Showdown-compatible RNG mode from the same sodium seed. Unlike turns.js, which compares
// outcome distributions, both the next-decision state and the full list of RNG draws must be
// identical; the first differing draw is reported with Showdown's event stack.
//
//   node exact.js [--battles 100] [--seed 1] [--sample 0.35] [--limit 20] [--dump file] [--log]
//                 [--require species,...] [--only id,...] [--no-whitelist]
//
// The engine binary must be built with the trace feature:
//   cargo build --release -p vgczero-engine --bin parity --features trace
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const L = require('./lib');
const pinned = require('../pinned');
const { instrument } = require('../../showdown/oracle.cjs');
const { compileWhitelist } = require('../../showdown/tie_policy.cjs');

function arg(name, dflt) {
	const i = process.argv.indexOf(name);
	return i >= 0 ? process.argv[i + 1] : dflt;
}
const BATTLES = Number(arg('--battles', 100));
const SEED = Number(arg('--seed', 1));
const SAMPLE = Number(arg('--sample', 0.35));
const LIMIT = Number(arg('--limit', 20));
const DUMP = arg('--dump', null);
const REQUIRE = arg('--require', null);
const ONLY = arg('--only', null);
const WHITELIST = process.argv.includes('--no-whitelist') ? null : path.join(pinned.root,
	'model_train/artifacts/evaluation/fast-engine-state-20261024/distribution/whitelist.verified.json');
const policy = WHITELIST ? compileWhitelist(JSON.parse(fs.readFileSync(WHITELIST, 'utf8'))) : null;
// --log: channel-parity baseline (contract 5). Showdown's omniscient channel (-1, the secret line of
// every |split|) against the engine's log, both reduced to the line types the tracker consumes.
const LOGCMP = process.argv.includes('--log');
const { extractChannelMessages } = require(path.join(L.PS_DIR, 'dist', 'sim', 'battle'));
const { IGNORED } = require(path.join(pinned.root, 'model_train', 'env', 'tracker.cjs'));

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

/** Draws as [lo, hi, value]; random(n) is [0, n], a float draw is [-1, -1, value]. */
function showdownDraw(r) {
	const a = r.args.map(x => (x && x.undefined ? undefined : x));
	if (a[0] === undefined) return [-1, -1, r.result];
	if (!a[1]) return [0, Math.floor(a[0]), r.result];
	return [Math.floor(a[0]), Math.floor(a[1]), r.result];
}

/** Showdown's cached speed (pokemon.speed) of each active, unfainted Pokemon. */
function withCachedSpeed(battle, f) {
	for (const side of battle.sides) {
		for (const p of side.active) if (p && !p.fainted) f[`${side.id}.${L.teamIdx(p)}.spdc`] = p.speed;
	}
	return f;
}

/** Replay one scenario in Showdown with the whitelist patch; returns outcome and draws. */
function showdownExact(sc) {
	let b;
	if (sc.serialized) {
		b = L.deserialize(sc.serialized);
	} else {
		b = L.newBattle(sc.teams[0], sc.teams[1], sc.initialSeed);
	}
	const seedBefore = b.prng.getSeed();
	const trace = instrument(b, policy ? { onTie: policy.onTie, recordRng: true, allowDeterminization: true } : { recordRng: true });
	const eachLog = [];
	if (process.env.EXACT_EACH) {
		const each = b.eachEvent;
		b.eachEvent = function (eventid, ...rest) {
			eachLog.push(`${eventid} ` + this.getAllActive().map(p => `${p.side.id}${'ab'[p.position]}:${p.speed}`).join(' '));
			return each.call(this, eventid, ...rest);
		};
	}
	const logStart = b.log.length;
	choose(b, sc.choices);
	return {
		seedBefore, seedAfter: b.prng.getSeed(),
		features: withCachedSpeed(b, L.outcome(b, logStart)),
		draws: trace.rng.map(showdownDraw),
		context: trace.rng.map(r => [r.move ? `[${r.move}]` : '[-]', ...r.context.map(c => `${c.method}:${c.event}${c.effect ? '@' + (c.effect.id || c.effect) : ''}`)].join(' > ')),
		ties: trace.ties.map(t => ({ mode: t.mode, artificial: t.artificial, event: t.category.event, effects: t.category.effects, rng: t.rng_index, group: t.group })),
		log: b.log.slice(logStart), eachLog,
	};
}

function generate() {
	const pool = L.loadPool();
	const sup = L.supportedTeams();
	const req = REQUIRE ? REQUIRE.split(',') : null;
	const supA = req ? sup.filter(i => pool[i].mons.some(m => req.includes(m.species))) : sup;
	const rand = new L.Rand(SEED);
	const out = [];
	for (let g = 0; g < BATTLES; g++) {
		const ta = pool[rand.pick(supA)], tb = pool[rand.pick(sup)];
		const initialSeed = sodium(`${SEED}.${g}`);
		const battle = L.newBattle(ta, tb, initialSeed);
		let steps = 0;
		while (!battle.ended && steps < 120) {
			steps++;
			const choices = battle.sides.map(s => L.randomChoice(battle, s, rand, { allyDamage: rand.chance(0.1) }));
			const preview = battle.requestState === 'teampreview';
			if (preview) for (const c of choices) c.preview = sortedPreview(c.preview);
			if (rand.chance(preview ? 0.5 : SAMPLE)) {
				out.push({
					id: `x${SEED}.${g}.${steps}`,
					kind: battle.requestState,
					serialized: preview ? null : L.serialize(battle),
					teams: [ta, tb], initialSeed,
					seed: battle.prng.getSeed(),
					state: preview ? null : L.exportState(battle, [ta, tb]),
					choices,
					engineChoices: choices.map((c, i) => L.engineChoice(battle.sides[i], c)),
				});
			}
			try {
				choose(battle, choices);
			} catch (e) {
				break;
			}
		}
	}
	return out;
}

function firstDiff(a, b) {
	const n = Math.min(a.length, b.length);
	for (let i = 0; i < n; i++) if (a[i][0] !== b[i][0] || a[i][1] !== b[i][1] || a[i][2] !== b[i][2]) return i;
	return a.length === b.length ? -1 : n;
}

/** Lines the tracker acts on (drops '', t:, -crit, -supereffective, ... per tracker.cjs IGNORED). */
function trackerLines(lines) {
	return lines.filter(l => !IGNORED.has(l.split('|')[1] || ''));
}

/** Multiset difference of exact line strings: Showdown-only and engine-only lines. */
function multisetDiff(a, b) {
	const count = new Map();
	for (const l of a) count.set(l, (count.get(l) || 0) + 1);
	for (const l of b) count.set(l, (count.get(l) || 0) - 1);
	const onlyA = [], onlyB = [];
	for (const [l, n] of count) {
		for (let i = 0; i < n; i++) onlyA.push(l);
		for (let i = 0; i < -n; i++) onlyB.push(l);
	}
	return { onlyA, onlyB };
}

const lineType = l => l.split('|')[1] || '';

function main() {
	const t0 = Date.now();
	const scenarios = generate().filter(sc => !ONLY || ONLY.split(',').includes(sc.id));
	const engineIn = scenarios.map(sc => sc.state ?
		{ id: sc.id, state: sc.state, seed: sc.seed, choices: sc.engineChoices } :
		{ id: sc.id, teams: sc.teams, seed: sc.initialSeed, choices: sc.engineChoices });
	const engineOut = new Map(L.runEngine(['exact'], engineIn).map(r => [r.id, r]));
	const counts = { exact: 0, state: 0, draws: 0, skipped: 0, showdownError: 0 };
	const byKind = {};
	const firstEvents = {};
	const logStats = { n: 0, same: 0, diff: 0, missing: {}, extra: {} };
	const logSamples = [];
	const tally = (o, k) => { o[k] = (o[k] || 0) + 1; };
	const failures = [];
	for (const sc of scenarios) {
		const e = engineOut.get(sc.id);
		const kind = (byKind[sc.kind] = byKind[sc.kind] || { n: 0, exact: 0 });
		if (!e || e.err) {
			counts.skipped++;
			if (process.env.EXACT_ERR) console.log(`  [${sc.id}] engine error: ${e ? e.err : "no result"}`, process.env.EXACT_ERR === "state" ? JSON.stringify(sc.state.sides.map(s => s.active.map(i => [s.mons[i].species, s.mons[i].volatiles, s.mons[i].last_move, s.mons[i].hp, s.mons[i].fainted, s.mons[i].item, (s.mons[i].moves || []).map(m => [m.id, m.pp])]))) + " choices=" + JSON.stringify(sc.engineChoices) + " sdchoices=" + JSON.stringify(sc.choices) : "");
			continue;
		}
		kind.n++;
		let s;
		try { s = showdownExact(sc); } catch (err) { counts.showdownError++; continue; }
		if (LOGCMP) {
			const sd = trackerLines(extractChannelMessages(s.log.join('\n'), [-1])[-1]);
			const en = trackerLines(e.log || []);
			const { onlyA, onlyB } = multisetDiff(sd, en);
			logStats.n++;
			if (!onlyA.length && !onlyB.length) logStats.same++;
			else {
				logStats.diff++;
				for (const l of onlyA) tally(logStats.missing, lineType(l));
				for (const l of onlyB) tally(logStats.extra, lineType(l));
				if (logSamples.length < LIMIT) logSamples.push({ id: sc.id, missing: onlyA.slice(0, 6), extra: onlyB.slice(0, 6) });
			}
		}
		const keys = new Set([...Object.keys(s.features), ...Object.keys(e.features)]);
		const stateDiff = [...keys].filter(k => String(s.features[k]) !== String(e.features[k]));
		const di = firstDiff(s.draws, e.draws);
		if (!stateDiff.length && di < 0) { counts.exact++; kind.exact++; continue; }
		if (stateDiff.length) counts.state++; else counts.draws++;
		const where = di < 0 ? '(draws equal)' : di < s.context.length ? s.context[di] : '(engine drew more)';
		const parts = where.split(' > ');
		const ev = parts.length > 1 ? parts.slice(-2).join(' > ') : where;
		firstEvents[ev] = (firstEvents[ev] || 0) + 1;
		failures.push({
			id: sc.id, kind: sc.kind, stateDiff: stateDiff.slice(0, 12).map(k => [k, s.features[k], e.features[k]]),
			firstDraw: di, showdownDraws: s.draws.length, engineDraws: e.draws.length,
			showdownAt: s.draws.slice(Math.max(0, di - 2), di + 3), engineAt: e.draws.slice(Math.max(0, di - 2), di + 3),
			eachLog: s.eachLog, where, showdownTrace: s.draws.map((d, i) => [...d, s.context[i]]), engineTrace: e.draws,
			showdownLog: s.log, engineLog: e.log, ties: s.ties, choices: sc.choices, state: ONLY ? sc.state : undefined,
		});
	}
	const compared = counts.exact + counts.state + counts.draws;
	console.log(`exact parity: ${scenarios.length} scenarios from ${BATTLES} battles in ${((Date.now() - t0) / 1000).toFixed(1)}s` +
		` (whitelist ${policy ? 'on' : 'off'})`);
	console.log(`  compared ${compared}: ${counts.exact} exact (${(100 * counts.exact / Math.max(1, compared)).toFixed(1)}%),` +
		` ${counts.state} state differs, ${counts.draws} same state but draws differ;` +
		` ${counts.skipped} engine-unsupported, ${counts.showdownError} Showdown replay errors`);
	console.log('  by kind:', Object.entries(byKind).map(([k, v]) => `${k} ${v.exact}/${v.n}`).join(', '));
	if (LOGCMP) {
		const top = o => Object.entries(o).sort((a, b) => b[1] - a[1]).slice(0, 20).map(([k, v]) => `${k} ${v}`).join(', ');
		console.log(`  channel parity (omniscient channel vs engine log, tracker-relevant lines): ${logStats.same}/${logStats.n} scenarios identical`);
		console.log('    showdown lines the engine lacks, by type:', top(logStats.missing));
		console.log('    engine lines showdown lacks, by type:', top(logStats.extra));
		for (const x of logSamples) console.log(`    [${x.id}] missing ${JSON.stringify(x.missing)}\n      extra ${JSON.stringify(x.extra)}`);
	}
	console.log('  first divergence (Showdown event stack, innermost two):');
	for (const [k, v] of Object.entries(firstEvents).sort((a, b) => b[1] - a[1]).slice(0, 25)) console.log(`    ${String(v).padStart(4)}  ${k}`);
	if (process.env.EXACT_EACH) {
		for (const f of failures.slice(0, LIMIT)) console.log(`  [${f.id}] showdown eachEvent:`, (f.eachLog || []).join(' | '));
	}
	for (const f of failures.slice(0, LIMIT)) {
		console.log(`\n  [${f.id} ${f.kind}] first draw #${f.firstDraw} of ${f.showdownDraws}/${f.engineDraws} at ${f.where}`);
		console.log(`      showdown ${JSON.stringify(f.showdownAt)}  engine ${JSON.stringify(f.engineAt)}`);
		for (const [k, a, b] of f.stateDiff) console.log(`      ${k}: showdown ${a} engine ${b}`);
	}
	if (DUMP) fs.writeFileSync(DUMP, JSON.stringify(failures, null, 1));
	// A --log run must also fail on channel divergence, not only on state/draw divergence:
	// the state and draws can match while tracker-relevant lines (item use, heals, Haze, ...)
	// differ, which is exactly what the channel comparison is there to catch.
	const logDiff = LOGCMP ? logStats.diff : 0;
	process.exitCode = counts.state + counts.draws + logDiff ? 1 : 0;
}

main();
