import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

export interface RunResult {
	code: number;
	stdout: string;
	stderr: string;
}

export interface RunOpts {
	cwd?: string;
	env?: NodeJS.ProcessEnv;
}

// binaries
const binaries: Record<string, string> = {
	'win32-x64': 'x86_64-pc-windows-msvc',
	'win32-arm64': 'aarch64-pc-windows-msvc',
	'darwin-x64': 'x86_64-apple-darwin',
	'darwin-arm64': 'aarch64-apple-darwin',
	'linux-x64': 'x86_64-unknown-linux-gnu',
	'linux-arm64': 'aarch64-unknown-linux-gnu',
	'linux-x64-musl': 'x86_64-unknown-linux-musl',
	'linux-arm64-musl': 'aarch64-unknown-linux-musl',
};

// musl sniff (linux only): no glibc version in node's process report = musl
// (alpine etc.); no report at all = assume gnu. shortcut: other libcs would
// misclassify as musl. mirrors install.js's sniff, the duplication is
// deliberate, it's ~8 lines
function isMusl(report: typeof defaultReport = defaultReport): boolean {
	try {
		if (process.platform !== 'linux') return false;
		const r = report();
		return !!(r && (!r.header || !r.header.glibcVersionCompiler));
	} catch {
		return false;
	}
}

function defaultReport(): { header?: { glibcVersionCompiler?: string } } | null {
	const rep = (process as NodeJS.Process).report;
	return rep && typeof rep.getReport === 'function' ? (rep.getReport() as never) : null;
}

// the binaries-map key (musl distros get their own entries)
function vendorKey(): string {
	const base = `${process.platform}-${process.arch}`;
	return process.platform === 'linux' && isMusl() ? `${base}-musl` : base;
}

// exported for tests (musl/gnu key behavior)
export { isMusl };

// thrown only when no zap binary can be found at all
export class ZapNotFoundError extends Error {
	constructor(detail: string) {
		super(
			[
				'zap binary not found.',
				'  resolution order: instance binary → ZAP_API_PATH → @zappkg/cli vendored binary → `zap` on PATH',
				`  ${detail}`,
				'  fix: set ZAP_API_PATH or the Zap({ binary }) option, or npm install @zappkg/cli',
			].join('\n')
		);
		this.name = 'ZapNotFoundError';
	}
}

// vendored by @zappkg/cli's postinstall; this file compiles to dist/, so the
// packages dir is two levels up
function vendorBinary(): string | null {
	const t = binaries[vendorKey()];
	if (!t) return null;
	const name = process.platform === 'win32' ? 'zap.exe' : 'zap';
	const p = join(__dirname, '..', '..', 'cli', 'vendor', t, name);
	return existsSync(p) ? p : null;
}

// the module-level chain (no instance override): ZAP_API_PATH → vendored → PATH
export function resolveBinary(): string {
	if (process.env.ZAP_API_PATH) return process.env.ZAP_API_PATH;
	const vendored = vendorBinary();
	if (vendored) return vendored;
	return 'zap'; // last resort: whatever is on PATH
}

// client

// the spawn core: one binary + argv → RunResult (no shell, utf8, exit code kept)
function spawnZap(bin: string, args: string[], opts: RunOpts = {}): Promise<RunResult> {
	return new Promise((resolveP, rejectP) => {
		const child = spawn(bin, args, {
			cwd: opts.cwd,
			env: opts.env ? { ...process.env, ...opts.env } : process.env,
			shell: false,
		});
		let stdout = '';
		let stderr = '';
		child.stdout.on('data', (d: Buffer) => {
			stdout += d.toString('utf8');
		});
		child.stderr.on('data', (d: Buffer) => {
			stderr += d.toString('utf8');
		});
		child.on('error', (err: NodeJS.ErrnoException) => {
			if (err.code === 'ENOENT') {
				rejectP(new ZapNotFoundError(`\`${bin}\` could not be executed`));
			} else {
				rejectP(err);
			}
		});
		child.on('close', (code) => resolveP({ code: code ?? 1, stdout, stderr }));
	});
}

export function run(args: string[], opts: RunOpts = {}): Promise<RunResult> {
	return spawnZap(resolveBinary(), args, opts);
}

// generated surface

export * from './src/api.gen';
export { COMMANDS, ZAP_VERSION } from './src/commands.gen';
export type { CommandMeta } from './src/commands.gen';

import type {
    AddOptions,
    AllowOptions,
    CiOptions,
    ConfigOptions,
    InstallOptions,
    LoginOptions,
    LsOptions,
    MigrateOptions,
    OutdatedOptions,
    PublishOptions,
    RemoveOptions,
    SkillsOptions,
    UpgradeOptions,
    VersionOptions,
    ViewOptions,
    WhyOptions,
    ZapApi,
} from './src/api.gen';
import {
    addArgv,
    allowArgv,
    auditArgv,
    binArgv,
    cacheArgv,
    ciArgv,
    configArgv,
    createArgv,
    dlxArgv,
    doctorArgv,
    execArgv,
    initArgv,
    inspectArgv,
    installArgv,
    licensesArgv,
    loginArgv,
    logoutArgv,
    lsArgv,
    migrateArgv,
    outdatedArgv,
    packArgv,
    publishArgv,
    registryArgv,
    removeArgv,
    runArgv,
    setVersionArgv,
    skillsArgv,
    storeArgv,
    tscArgv,
    upgradeArgv,
    versionArgv,
    viewArgv,
    whoamiArgv,
    whyArgv,
    xArgv,
} from './src/api.gen';

// json schemas (the read commands' --json documents)

export interface LsJson {
	name: string;
	version: string;
	private: boolean;
	dependencies: Record<string, string>;
	devDependencies: Record<string, string>;
	workspaceMembers: { name: string; dir: string }[];
	tree?: string[];
}

export interface WhyNode {
	name: string;
	version: string | null;
	workspace?: boolean;
}

export interface WhyJson {
	pkg: string;
	chains: WhyNode[][];
}

export interface ViewJson {
	name: string;
	version: string;
	distTags: Record<string, string>;
	versions: string[];
	description?: string;
	dependencies?: Record<string, string>;
	license?: string;
	homepage?: string;
}

export interface OutdatedRow {
	name: string;
	current: string;
	wanted: string;
	latest: string;
	major?: boolean;
}

export interface OutdatedJson {
	outdated: OutdatedRow[];
	upToDate: OutdatedRow[];
}

// the mutating commands' install report (one shape for install/add/remove/ci)
export interface FetchStats {
	count: number;
	bytes: number;
	fromStore: number;
}

export interface LifecycleStats {
	ran: number;
	blocked: number;
}

export interface InstallJson {
	/** [pkg_id, manifest section that pulled it in], newly in the tree */
	added: [string, string][];
	/** ids gone vs the previous lockfile */
	removed: string[];
	/** [new id, old version, new version] */
	changed: [string, string, string][];
	fetched: FetchStats;
	linkedFiles: number;
	skippedPlatform: number;
	/** the rendered peer-dependency warning lines */
	peers: string[];
	lifecycle: LifecycleStats;
	donePackages: number;
	seconds: number;
}

export type WithData<T> = RunResult & { data: T };

function withJson<T>(res: RunResult): WithData<T> {
	let data: T;
	try {
		data = JSON.parse(res.stdout) as T;
	} catch (err) {
		// a failing command legitimately prints no json, surface the child's
		// stderr (the real reason); only a SUCCESS with unparseable stdout is
		// the version-mismatch case
		if (res.code !== 0) {
			const reason = res.stderr.trim() || `exit ${res.code}`;
			throw new Error(`zap ${res.code}: ${reason}`);
		}
		throw new Error(`zap --json output did not parse, the zap binary predates the flag? (${String(err)})`);
	}
	return { ...res, data };
}

// the api front

export interface ZapOpts {
	/** the directory zap runs in / resolves the project from; default: `process.cwd()` */
	cwd?: string;
	/** per-instance binary path; wins over the `ZAP_API_PATH` → vendor → PATH chain */
	binary?: string;
}

export class Zap implements ZapApi {
	private readonly opts: ZapOpts;

	constructor(opts: ZapOpts = {}) {
		this.opts = opts;
	}

	// instance binary: the explicit override wins, the standard chain is the
	// fallback, resolved lazily per call
	private binPath(): string {
		return this.opts.binary ?? resolveBinary();
	}

	private spawn(args: string[]): Promise<RunResult> {
		return spawnZap(this.binPath(), args, { cwd: this.opts.cwd });
	}

	// the json-returning commands: --json is forced + parsed, data is contract
	private json<T>(args: string[]): Promise<WithData<T>> {
		return this.spawn(args).then((r) => withJson<T>(r));
	}

	add(specs: string[], options?: AddOptions): Promise<WithData<InstallJson>> {
		return this.json<InstallJson>(addArgv(specs, { ...options, json: true }));
	}

	ci(options?: CiOptions): Promise<WithData<InstallJson>> {
		return this.json<InstallJson>(ciArgv({ ...options, json: true }));
	}

	init(): Promise<RunResult> {
		return this.spawn(initArgv());
	}

	install(options?: InstallOptions): Promise<WithData<InstallJson>> {
		return this.json<InstallJson>(installArgv({ ...options, json: true }));
	}

	outdated(options?: OutdatedOptions): Promise<WithData<OutdatedJson>> {
		return this.json<OutdatedJson>(outdatedArgv({ ...options, json: true }));
	}

	remove(pkgs: string[], options?: RemoveOptions): Promise<WithData<InstallJson>> {
		return this.json<InstallJson>(removeArgv(pkgs, { ...options, json: true }));
	}

	upgrade(pkgs?: string[], options?: UpgradeOptions): Promise<RunResult> {
		return this.spawn(upgradeArgv(pkgs, options));
	}

	create(template: string, args?: string[]): Promise<RunResult> {
		return this.spawn(createArgv(template, args));
	}

	dlx(spec: string, args?: string[]): Promise<RunResult> {
		return this.spawn(dlxArgv(spec, args));
	}

	exec(command: string, args?: string[]): Promise<RunResult> {
		return this.spawn(execArgv(command, args));
	}

	run(scripts: string[], shared?: string[]): Promise<RunResult> {
		return this.spawn(runArgv(scripts, shared));
	}

	tsc(args?: string[]): Promise<RunResult> {
		return this.spawn(tscArgv(args));
	}

	x(bin: string, args?: string[]): Promise<RunResult> {
		return this.spawn(xArgv(bin, args));
	}

	audit(): Promise<RunResult> {
		return this.spawn(auditArgv());
	}

	bin(name?: string): Promise<RunResult> {
		return this.spawn(binArgv(name));
	}

	cache(sub?: string): Promise<RunResult> {
		return this.spawn(cacheArgv(sub));
	}

	doctor(): Promise<RunResult> {
		return this.spawn(doctorArgv());
	}

	inspect(spec: string): Promise<RunResult> {
		return this.spawn(inspectArgv(spec));
	}

	licenses(): Promise<RunResult> {
		return this.spawn(licensesArgv());
	}

	ls(options?: LsOptions): Promise<WithData<LsJson>> {
		return this.json<LsJson>(lsArgv({ ...options, json: true }));
	}

	store(sub?: string): Promise<RunResult> {
		return this.spawn(storeArgv(sub));
	}

	view(pkg: string, field?: string, options?: ViewOptions): Promise<WithData<ViewJson>> {
		return this.json<ViewJson>(viewArgv(pkg, field, { ...options, json: true }));
	}

	why(pkg: string, options?: WhyOptions): Promise<WithData<WhyJson>> {
		return this.json<WhyJson>(whyArgv(pkg, { ...options, json: true }));
	}

	login(options?: LoginOptions): Promise<RunResult> {
		return this.spawn(loginArgv(options));
	}

	logout(): Promise<RunResult> {
		return this.spawn(logoutArgv());
	}

	pack(): Promise<RunResult> {
		return this.spawn(packArgv());
	}

	publish(options?: PublishOptions): Promise<RunResult> {
		return this.spawn(publishArgv(options));
	}

	registry(args?: string[]): Promise<RunResult> {
		return this.spawn(registryArgv(args));
	}

	version(spec?: string, options?: VersionOptions): Promise<RunResult> {
		return this.spawn(versionArgv(spec, options));
	}

	whoami(): Promise<RunResult> {
		return this.spawn(whoamiArgv());
	}

	allow(pkgs?: string[], options?: AllowOptions): Promise<RunResult> {
		return this.spawn(allowArgv(pkgs, options));
	}

	config(key?: string, value?: string, options?: ConfigOptions): Promise<RunResult> {
		return this.spawn(configArgv(key, value, options));
	}

	migrate(options?: MigrateOptions): Promise<RunResult> {
		return this.spawn(migrateArgv(options));
	}

	skills(options?: SkillsOptions): Promise<RunResult> {
		return this.spawn(skillsArgv(options));
	}

	setVersion(version?: string): Promise<RunResult> {
		return this.spawn(setVersionArgv(version));
	}
}

/** the shared default instance: process.cwd(), binary via the standard chain */
export const zapApi: ZapApi = new Zap();
