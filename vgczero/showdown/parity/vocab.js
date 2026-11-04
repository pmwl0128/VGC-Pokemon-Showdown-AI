'use strict';
// ③ vocabulary fixture. The v3 encoder's global id space is derived from
// `prepared/vocabulary.json` (B6) plus the pinned dex (species formes, effect ids, ...). The native
// encoder rebuilds the same index/offset from `lists` here, so the two agree without extending the
// engine dex. Rebuild after any change to the pin, the prepared vocabulary, or encoder.cjs.
//
//   node vocab.js [--out <fixtures/vocab.json.gz>]
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const pinned = require('../pinned');
const {VOCAB, T, C, F, VERSION} = require('../../../../env/encoder.cjs');

function arg(name, dflt) {
	const i = process.argv.indexOf(name);
	return i >= 0 ? process.argv[i + 1] : dflt;
}
const OUT = arg('--out', path.join(pinned.DATA_DIR, 'fixtures', 'vocab.json.gz'));

fs.mkdirSync(path.dirname(OUT), {recursive: true});
// Ordered by field (insertion order = offset order); `values` gives each field's local ids.
const fields = Object.entries(VOCAB.fields).map(([name, f]) => ({name, offset: f.offset, size: f.size, values: VOCAB.lists[name]}));
const payload = {VERSION, T, C, F, size: VOCAB.size, hash: VOCAB.hash, fields};
fs.writeFileSync(OUT, zlib.gzipSync(JSON.stringify(payload)));
const nfields = fields.length;
console.log(`wrote ${OUT}: ${nfields} fields, vocab size ${VOCAB.size}, hash ${VOCAB.hash.slice(0, 12)}`);
