#!/usr/bin/env node
// shim npm puts on the path: resolve the real zap binary and hand off
// args + stdio + exit code. execArgv is stripped so a consuming project's
// node flags can't leak into zap's runtime (zap manages NODE_OPTIONS for
// its own children).
const { spawn } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');

const binaries = {
	'win32-x64': 'x86_64-pc-windows-msvc',
	'win32-arm64': 'aarch64-pc-windows-msvc',
	'darwin-x64': 'x86_64-apple-darwin',
	'darwin-arm64': 'aarch64-apple-darwin',
	'linux-x64': 'x86_64-unknown-linux-gnu',
	'linux-arm64': 'aarch64-unknown-linux-gnu',
	'linux-x64-musl': 'x86_64-unknown-linux-musl',
	'linux-arm64-musl': 'aarch64-unknown-linux-musl',
};

// npm names → rust triple (mirrors install.js's map, the shim stays
// self-contained, no lib/ dependency)
function triple(platform, arch) {
	return binaries[`${platform}-${arch}`] || null;
}

// resolve the real zap binary: ZAP_DEV_PATH override, then the vendored
// download; null = not found (the caller prints the install-method hints)
function resolveBinary({ env = process.env, root, exists = (p) => fs.existsSync(p) } = {}) {
	if (env.ZAP_DEV_PATH) return env.ZAP_DEV_PATH;
	const t = triple(process.platform, process.arch);
	if (!t) return null;
	const candidate = path.join(root, 'vendor', t, process.platform === 'win32' ? 'zap.exe' : 'zap');
	return exists(candidate) ? candidate : null;
}

const bin = resolveBinary({ root: path.join(__dirname, '..') });
if (!bin) {	const t = triple(process.platform, process.arch);
	console.error(`zap: no zap binary found for ${process.platform}-${process.arch}${t ? ` (${t})` : ''}`);
	console.error(`  the postinstall download didn't complete or was skipped. fixes:`);
	console.error(`    - set ZAP_DEV_PATH to a zap build on disk (dev escape hatch)`);
	console.error(`    - reinstall: npm install -g @zappkg/cli`);
	process.exit(1);
}

// mark the invocation so zap can tell how it was installed (npm shim vs
// script/binary); detection only for now, update flows key off it later
process.env.__ZAP_VIA = 'npm';

const child = spawn(bin, process.argv.slice(2), { stdio: 'inherit', execArgv: [] });
child.on('error', (err) => {
	console.error(`zap: failed to launch ${bin}, ${err.message}`);
	if (process.env.ZAP_DEV_PATH) console.error('  ZAP_DEV_PATH is set, check that the path exists');
	process.exit(1);
});
child.on('exit', (code, signal) => {
	process.exit(signal ? 1 : (code ?? 1));
});
