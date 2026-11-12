// argv builder unit checks: the generated builders' flag mapping is the drift
// surface, a spec typo (--devv) shows up here. run with `npm test` after a
// build (tests the compiled dist/).
const test = require('node:test');
const assert = require('node:assert');
const { addArgv, lsArgv, viewArgv, runArgv, installArgv, publishArgv, removeArgv, ciArgv } = require('../dist/src/api.gen.js');

test('addArgv maps options to the real flags', () => {
	assert.deepEqual(addArgv(['chalk'], { dev: true }), ['add', 'chalk', '--dev']);
	assert.deepEqual(addArgv(['chalk', 'ms@^2']), ['add', 'chalk', 'ms@^2']);
	assert.deepEqual(addArgv(['t'], { global: true }), ['add', 't', '--global']);
	assert.deepEqual(addArgv(['t'], { dev: false, global: false }), ['add', 't']);
	assert.deepEqual(addArgv(['t'], { json: true }), ['add', 't', '--json']);
});

test('lsArgv carries the json mode the wrapper forces', () => {
	assert.deepEqual(lsArgv({ json: true }), ['ls', '--json']);
	assert.deepEqual(lsArgv({ all: true, json: true }), ['ls', '--all', '--json']);
	assert.deepEqual(lsArgv({ global: true, json: true }), ['ls', '--global', '--json']);
});

test('viewArgv positionals + options', () => {
	assert.deepEqual(viewArgv('chalk', 'version'), ['view', 'chalk', 'version']);
	assert.deepEqual(viewArgv('chalk', undefined, { json: true }), ['view', 'chalk', '--json']);
});

test('runArgv separates shared args with --', () => {
	assert.deepEqual(runArgv(['build']), ['run', 'build']);
	assert.deepEqual(runArgv(['a', 'b'], ['--watch']), ['run', 'a', 'b', '--', '--watch']);
	assert.deepEqual(runArgv(['a'], []), ['run', 'a']);
});

test('installArgv + publishArgv value flags', () => {
	assert.deepEqual(installArgv({ frozenLockfile: true }), ['install', '--frozen-lockfile']);
	assert.deepEqual(installArgv({ json: true }), ['install', '--json']);
	assert.deepEqual(removeArgv(['chalk'], { json: true }), ['remove', 'chalk', '--json']);
	assert.deepEqual(ciArgv({ json: true }), ['ci', '--json']);
	assert.deepEqual(publishArgv({ tag: 'next', otp: '123456', dryRun: true }), [
		'publish',
		'--tag',
		'next',
		'--otp',
		'123456',
		'--dry-run',
	]);
});
