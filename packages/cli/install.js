#!/usr/bin/env node

// postinstall: fetch the prebuilt zap binary for this platform into vendor/.
// skipped when ZAP_DEV_PATH is set (dev mode) or the binary is already vendored.

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const tar = require('tar');
const { version } = require('./package.json');

const dim = (s) => (process.stdout.isTTY ? `\x1b[2m${s}\x1b[0m` : s);

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

function muslKey(platform = process.platform, report = defaultReport) {
	try {
		if (platform !== 'linux') return false;
		const r = report();
		return !!(r && (!r.header || !r.header.glibcVersionCompiler));
	} catch {
		return false;
	}
}

function defaultReport() {
	return process.report && typeof process.report.getReport === 'function' ? process.report.getReport() : null;
}

function platformKey(platform = process.platform, arch = process.arch, report) {
	return platform === 'linux' && muslKey(platform, report) ? `${platform}-${arch}-musl` : `${platform}-${arch}`;
}

function formatBinary(platform = process.platform, arch = process.arch, report) {
	return binaries[platformKey(platform, arch, report)] || null;
}

function binaryName(platform) {
	return platform === 'win32' ? 'zap.exe' : 'zap';
}

function downloadUrl({ version, platform = process.platform, arch = process.arch } = {}) {
	const binary = formatBinary(platform, arch);
	if (!binary) throw new Error(`no prebuilt zap binary for ${platform}-${arch}`);
	return `https://github.com/ninjaninja140/zap/releases/download/v${version}/zap-${binary}.tar.gz`;
}

// fetch with redirect following + a hard timeout so npm doesnt hang on us
async function download(url) {
	const res = await fetch(url, {
		redirect: 'follow',
		signal: AbortSignal.timeout(60_000),
		headers: { 'user-agent': '@zappkg/cli installer' },
	});
	if (!res.ok) throw new Error(`http ${res.status}`);
	return Buffer.from(await res.arrayBuffer());
}

// tarballs may nest the binary (zap-<triple>/zap[.exe]), extract, then locate
function findFile(dir, name) {
	for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
		const p = path.join(dir, e.name);
		if (e.isDirectory()) {
			const hit = findFile(p, name);
			if (hit) return hit;
		} else if (e.name === name) {
			return p;
		}
	}
	return null;
}

async function main() {
	const { platform, arch } = process;
	const binary = formatBinary(platform, arch);

	if (!binary) {
		console.error(`zap: unsupported platform ${platform}-${arch}, no prebuilt binary exists`);
		process.exit(1);
	}

	if (process.env.ZAP_DEV_PATH) {
		console.log(dim('> ZAP_DEV_PATH set, skipping zap download (dev mode)'));
		return;
	}

	const dir = path.join(__dirname, 'vendor', binary);
	const bin = path.join(dir, binaryName(platform));
	if (fs.existsSync(bin)) {
		console.log(dim('> zap binary already vendored, skipping download'));
		return;
	}

	const url = downloadUrl({ version, platform, arch });
	console.log(dim(`> downloading zap ${version} (${binary})...`));

	let tarball = null;
	try {
		tarball = await download(url);
		console.log(dim(`  ok: ${url}`));
	} catch (err) {
		console.log(dim(`  failed: ${url} (${err.message})`));
	}

	if (!tarball) {
		console.error('');
		console.error(`zap: couldn't download zap ${version} for ${binary}`);
		process.exit(1);
	}

	// extract to a temp dir, then move the binary into place (rename, with a
	// copy fallback in case temp and the package sit on different volumes)
	const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'zap-vendor-'));
	try {
		const tarballPath = path.join(tmp, 'zap.tar.gz');
		fs.writeFileSync(tarballPath, tarball);
		await tar.x({ cwd: tmp, file: tarballPath });
		const extracted = findFile(tmp, binaryName(platform));
		if (!extracted) throw new Error('release artifact contained no zap binary');
		fs.mkdirSync(dir, { recursive: true });
		try {
			fs.renameSync(extracted, bin);
		} catch {
			fs.copyFileSync(extracted, bin);
			fs.rmSync(extracted, { force: true });
		}
		if (platform !== 'win32') fs.chmodSync(bin, 0o755);
	} finally {
		fs.rmSync(tmp, { recursive: true, force: true });
	}
	console.log(`> zap ${version} installed (${path.relative(__dirname, bin)})`);
}

if (require.main === module) {
	main().catch((err) => {
		console.error(`zap: install failed, ${err.message}`);
		process.exit(1);
	});
}
