#!/usr/bin/env node
// shim npm puts on the path: resolve the real zap binary and hand off
// args + stdio + exit code. execArgv is stripped so a consuming project's
// node flags can't leak into zap's runtime (zap manages NODE_OPTIONS for
// its own children).
const { spawn } = require('node:child_process');
const path = require('node:path');
const { triple, resolveBinary } = require('../lib/config.js');

const bin = resolveBinary({ root: path.join(__dirname, '..') });
if (!bin) {
	const t = triple(process.platform, process.arch);
	console.error(`zap: no zap binary found for ${process.platform}-${process.arch}${t ? ` (${t})` : ''}`);
	console.error(`  the postinstall download didn't complete or was skipped. fixes:`);
	console.error(`    - set ZAP_DEV_PATH to a zap build on disk (dev escape hatch)`);
	console.error(`    - set ZAP_DOWNLOAD_BASE to a host serving zap-<triple>.tar.gz and reinstall`);
	console.error(`    - reinstall: npm install -g @zappkg/cli`);
	process.exit(1);
}

const child = spawn(bin, process.argv.slice(2), { stdio: 'inherit', execArgv: [] });
child.on('error', (err) => {
	console.error(`zap: failed to launch ${bin}, ${err.message}`);
	if (process.env.ZAP_DEV_PATH) console.error('  ZAP_DEV_PATH is set, check that the path exists');
	process.exit(1);
});
child.on('exit', (code, signal) => {
	process.exit(signal ? 1 : (code ?? 1));
});
