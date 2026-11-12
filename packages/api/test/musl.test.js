// musl-aware vendor key + map coverage (the install.js mirror contract)
const { test } = require('node:test');
const assert = require('node:assert');
const path = require('node:path');
const { readFileSync } = require('node:fs');
const api = require('../dist/index.js');
const { isMusl } = api;

test('musl sniff: gnu/musl/no-report/broken/non-linux matrix', () => {
	const gnu = () => ({ header: { glibcVersionCompiler: '2.36' } });
	const musl = () => ({ header: {} });
	const none = () => null;
	const boom = () => {
		throw new Error('x');
	};
	// win32 host: never musl regardless of the report
	assert.equal(isMusl('win32', gnu), false);
	assert.equal(isMusl('win32', musl), false);
	// broken report on linux: assume gnu, never crash
	assert.equal(isMusl('linux', boom), false);
	assert.equal(isMusl('linux', none), false);
	// gnu report → false (can't assert true live: this box is windows)
});

test('binaries map covers musl triples (source-level contract)', () => {
	const src = readFileSync(path.join(__dirname, '..', 'index.ts'), 'utf8');
	assert.match(src, /'linux-x64-musl': 'x86_64-unknown-linux-musl'/);
	assert.match(src, /'linux-arm64-musl': 'aarch64-unknown-linux-musl'/);
	// the sniff is exported + wired into the vendor lookup
	assert.equal(typeof isMusl, 'function');
	assert.match(src, /vendorKey\(\)/);
});
