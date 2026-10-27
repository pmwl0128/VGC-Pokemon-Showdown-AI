'use strict';
// Standalone vgczero tree: the tools read a built Pokemon Showdown checkout (PS_DIR, default
// showdown/ps from setup.sh) and keep the generated inputs in ../data (FAST_ENGINE_DATA
// overrides). Inside the Quark Drive repository this file is replaced by one that loads that
// track's pinned build (model_train/env/engine.cjs), which is where the engine is verified.
const path = require('path');
const cp = require('child_process');

const HERE = __dirname;
const PS_DIR = process.env.PS_DIR || path.join(HERE, 'ps');
const FORMAT = process.env.FORMAT || 'gen9championsvgc2026regmc';
const DATA_DIR = process.env.FAST_ENGINE_DATA || path.join(HERE, '..', 'data');

function gitHead(dir) {
	try {
		return cp.execFileSync('git', ['-C', dir, 'rev-parse', 'HEAD'], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore']}).trim();
	} catch {
		return null;
	}
}

module.exports = {
	root: path.join(HERE, '..'),
	PS_DIR,
	FORMAT,
	TRAINING_FORMAT: FORMAT,
	pin: gitHead(PS_DIR),
	buildHash: null,
	DATA_DIR,
	DEX_FILE: path.join(DATA_DIR, 'dex.json.gz'),
	TEAMS_FILE: path.join(DATA_DIR, 'teams.json.gz'),
	ENGINE_DIR: path.join(HERE, '..', 'engine'),
	TARGET_DIR: path.join(HERE, '..', 'target'),
};
