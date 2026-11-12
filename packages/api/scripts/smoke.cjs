// live smoke of the api layer. usage:
//   ZAP_API_PATH=<zap build> node scripts/smoke.cjs   (cwd = the zap project to drive)
const assert = require('node:assert');
const fs = require('node:fs');
const api = require('../dist/index.js');

(async () => {
	// binary reachable: the --version flag (not a registry command, so no
	// zapApi method, raw run + parse)
	const vres = await api.run(['--version']);
	const v = (vres.stdout.match(/(\d+\.\d+\.\d+\S*)/) || [])[1];
	console.log(`run(['--version']) -> ${v}`);
	assert.match(v, /^\d+\.\d+\.\d+$/);

	// the generated jsdoc survives into the declarations
	assert.ok(
		(
			await require('node:fs/promises').readFile(
				require('node:path').join(__dirname, '..', 'dist', 'src', 'api.gen.d.ts'),
				'utf8'
			)
		).includes('adds deps to package.json'),
		'generated jsdoc must survive into dist/src/api.gen.d.ts'
	);
	const cls = await require('node:fs/promises').readFile(
		require('node:path').join(__dirname, '..', 'dist', 'index.d.ts'),
		'utf8'
	);
	assert.ok(cls.includes('class Zap') && cls.includes('ZapOpts'), 'Zap class + ZapOpts in the d.ts');

	// the default instance (zapApi): the four --json read commands as parsed data
	const zl = await api.zapApi.ls();
	console.log(`zapApi.ls() -> name=${zl.data.name}, deps=${Object.keys(zl.data.dependencies).length}`);
	assert.equal(zl.data.name, 'zap-multirun-test');
	const zw = await api.zapApi.why('ms');
	console.log(`zapApi.why('ms') -> ${zw.data.chains.length} chain(s)`);
	assert.ok(zw.data.chains.length > 0 && zw.data.chains[0][0].name === 'ms');
	const vw = await api.zapApi.view('chalk');
	console.log(`zapApi.view('chalk') -> ${vw.data.versions.length} versions, latest ${vw.data.version}`);
	assert.equal(vw.data.name, 'chalk');
	const zo = await api.zapApi.outdated();
	console.log(`zapApi.outdated() -> ${zo.data.outdated.length} outdated, ${zo.data.upToDate.length} up to date`);
	assert.ok(Array.isArray(zo.data.outdated) && Array.isArray(zo.data.upToDate));

	// add/remove round-trip through a per-instance Zap (cwd pinned to the
	// fixture), dev: true must land in devDependencies, manifest restored
	const z = new api.Zap({ cwd: process.cwd() });
	const before = JSON.parse(fs.readFileSync('package.json', 'utf8'));
	const added = await z.add(['is-odd'], { dev: true });
	console.log(`z.add(['is-odd'], { dev: true }) -> code ${added.code}`);
	assert.equal(added.code, 0);
	const mid = JSON.parse(fs.readFileSync('package.json', 'utf8'));
	assert.ok(Object.keys(mid.devDependencies ?? {}).includes('is-odd'), 'is-odd should be in devDependencies');
	const removed = await z.remove(['is-odd']);
	console.log(`z.remove(['is-odd']) -> code ${removed.code}`);
	assert.equal(removed.code, 0);
	const after = JSON.parse(fs.readFileSync('package.json', 'utf8'));
	// zap keeps a section once it exists: an emptied devDependencies is the
	// only allowed residue; the original bytes go back so the fixture stays put
	if (after.devDependencies && Object.keys(after.devDependencies).length === 0 && !before.devDependencies) {
		delete after.devDependencies;
	}
	assert.deepEqual(after, before, 'manifest should be restored');
	fs.writeFileSync('package.json', JSON.stringify(before, null, 2) + '\n');

	// a command that errors → result data, not a throw
	// (note: `zap why <not-in-tree>` deliberately exits 0 with a note)
	const bad = await api.run(['view']);
	console.log(`run(['view']) with no args -> code ${bad.code}, stderr: ${bad.stderr.trim().split('\n')[0]}`);
	assert.notEqual(bad.code, 0);

	// generated metadata: version constant + the command list vs the binary's help
	assert.equal(api.ZAP_VERSION, '0.1.0');
	assert.ok(Array.isArray(api.COMMANDS) && api.COMMANDS.length >= 25, 'commands floor count');
	// --help shows per-category counts; their sum must equal COMMANDS.length
	const help = await api.run(['--help']);
	const counted = [...help.stdout.matchAll(/\((\d+) commands?\)/g)].reduce((s, m) => s + Number(m[1]), 0);
	assert.equal(counted, api.COMMANDS.length, 'help category counts sum to COMMANDS.length');
	const all = await api.run(['help', '--all']);
	for (const c of api.COMMANDS) {
		assert.ok(all.stdout.includes(c.name), `help --all lists ${c.name}`);
	}
	console.log(`COMMANDS.length = ${api.COMMANDS.length}, matches --help counts, all names in help --all`);

	console.log('smoke ok');
})().catch((err) => {
	console.error(err);
	process.exit(1);
});
