// Zap class unit checks: instance cwd, binary override precedence, lazy
// resolution, default-instance behavior. the live tests need a zap binary
// (set ZAP_API_PATH; they skip otherwise). run with `npm test` after a build.
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const api = require('../dist/index.js');

// a minimal project: manifest + lockfile, enough for `zap ls --json`
function miniFixture(name) {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), `zap-class-${name}-`));
	fs.writeFileSync(
		path.join(dir, 'package.json'),
		JSON.stringify({ name, version: '1.0.0', private: true }, null, 2) + '\n'
	);
	fs.writeFileSync(
		path.join(dir, '.zap-lock.json'),
		JSON.stringify({ version: 2, rootHash: 'x', root: {}, packages: {} }) + '\n'
	);
	return dir;
}

const dirA = miniFixture('fixture-a');
const dirB = miniFixture('fixture-b');

// the live tests skip when no binary resolves (fresh clone, CI without a build)
const probe = spawnSync(api.resolveBinary(), ['--version'], { encoding: 'utf8', timeout: 15000 });
const skip = probe.error || probe.status !== 0 ? 'no zap binary (set ZAP_API_PATH)' : false;

test('instance cwd: two instances, two fixtures, two names', { skip }, async () => {
	const a = new api.Zap({ cwd: dirA });
	const b = new api.Zap({ cwd: dirB });
	assert.equal((await a.ls()).data.name, 'fixture-a');
	assert.equal((await b.ls()).data.name, 'fixture-b');
});

test('binary override beats the env chain', { skip }, async () => {
	// ZAP_API_PATH resolves fine, but the instance override is broken → the
	// override was chosen (that's the precedence contract)
	const broken = new api.Zap({ cwd: dirA, binary: path.join(dirA, 'no-such-zap.exe') });
	await assert.rejects(
		() => broken.ls(),
		(err) => err instanceof api.ZapNotFoundError
	);
});

test('binary override supplies the binary when the chain has nothing', { skip }, async () => {
	const saved = process.env.ZAP_API_PATH;
	delete process.env.ZAP_API_PATH;
	try {
		const z = new api.Zap({ cwd: dirA, binary: saved });
		assert.equal((await z.ls()).data.name, 'fixture-a');
	} finally {
		if (saved) process.env.ZAP_API_PATH = saved;
	}
});

test('lazy resolution: constructing never throws, calling without a binary does', async () => {
	const saved = process.env.ZAP_API_PATH;
	delete process.env.ZAP_API_PATH;
	try {
		const z = new api.Zap({ cwd: dirA, binary: path.join(dirA, 'nope.exe') });
		await assert.rejects(
			() => z.ls(),
			(err) => err instanceof api.ZapNotFoundError
		);
	} finally {
		if (saved) process.env.ZAP_API_PATH = saved;
	}
});

test('default instance: module zapApi and new Zap() both use process.cwd()', { skip }, async () => {
	const saved = process.cwd();
	process.chdir(dirA);
	try {
		assert.equal((await api.zapApi.ls()).data.name, 'fixture-a');
		assert.equal((await new api.Zap().ls()).data.name, 'fixture-a');
	} finally {
		process.chdir(saved);
	}
});
