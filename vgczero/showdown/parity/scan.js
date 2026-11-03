'use strict';
// Dump one exact.js scenario (seed.battle.step): pinned full log, channel, engine log + draws.
// Usage: node scan.js <seed> <battle> <step> [sample=0.35]
const path = require('path');
const crypto = require('crypto');
const L = require(__dirname + '/lib');
const pinned = require(__dirname + '/../pinned');
const { extractChannelMessages } = require(path.join(pinned.PS_DIR, 'dist', 'sim', 'battle'));
const SEED = Number(process.argv[2] || 3), G = Number(process.argv[3] || 0), STEP = Number(process.argv[4] || 1);
const SAMPLE = Number(process.argv[5] || 0.35);
const sodium = tag => 'sodium,' + crypto.createHash('sha256').update(`exact/${tag}`).digest('hex');
const sortedPreview = o => [...o.slice(0, 2).sort((a, b) => a - b), ...o.slice(2, 4).sort((a, b) => a - b)];
function choose(battle, choices) {
	for (let i = 0; i < 2; i++) {
		const c = choices[i];
		if (!c) continue;
		const side = battle.sides[i];
		if (!battle.choose(side.id, L.choiceString(side, c))) throw new Error('choice rejected: ' + side.id);
	}
}
const pool = L.loadPool(); const sup = L.supportedTeams(); const rand = new L.Rand(SEED);
for (let g = 0; g <= G; g++) {
	const ta = pool[rand.pick(sup)], tb = pool[rand.pick(sup)];
	const initialSeed = sodium(`${SEED}.${g}`);
	const battle = L.newBattle(ta, tb, initialSeed);
	let steps = 0;
	while (!battle.ended && steps < 120) {
		steps++;
		const choices = battle.sides.map(s => L.randomChoice(battle, s, rand, { allyDamage: rand.chance(0.1) }));
		const preview = battle.requestState === 'teampreview';
		if (preview) for (const c of choices) c.preview = sortedPreview(c.preview);
		const take = rand.chance(preview ? 0.5 : SAMPLE);
		if (g === G && steps === STEP) {
			const state = preview ? null : L.exportState(battle, [ta, tb]);
			const seed = battle.prng.getSeed();
			const engineChoices = choices.map((c, i) => L.engineChoice(battle.sides[i], c));
			console.log(`=== x${SEED}.${g}.${steps} kind=${battle.requestState} take=${take}`);
			console.log(`teams: ${ta} vs ${tb}`);
			console.log(`choices: ${JSON.stringify(engineChoices)}`);
			const logStart = battle.log.length;
			choose(battle, choices);
			const sd = battle.log.slice(logStart);
			console.log(`=== showdown full log (${sd.length}) ===`);
			console.log(sd.join('\n'));
			const ch = extractChannelMessages(sd.join('\n'), [-1]);
			console.log(`=== showdown channel -1 (${(ch[-1] || []).length}) ===`);
			console.log((ch[-1] || []).join('\n'));
			const e = L.runEngine(['exact'], [{ id: 's', state, seed, choices: engineChoices }])[0];
			const draws = (e.draws || []).map(d => d.slice(0, 3));
			console.log(`=== engine log (${(e.log || []).length}) err=${e.err || ''} draws=${JSON.stringify(draws)} ===`);
			console.log((e.log || []).join('\n'));
			process.exit(0);
		}
		try { choose(battle, choices); }
		catch (e) { break; }
	}
}
console.log('scenario not reached (battle ended early?)');
