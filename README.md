```
          █          
         ██          ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁
       ███           ░        ░░░      ░░░       ░░
     ████            ▒▒▒▒▒▒  ▒▒▒  ▒▒▒▒  ▒▒  ▒▒▒▒  ▒
    █████████        ▓▓▓▓  ▓▓▓▓▓  ▓▓▓▓  ▓▓       ▓▓
        ███          ██  ███████        ██  ███████
       ███           █        ██  ████  ██  ███████
      ██             ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
      █         
```

A tiny, fast, ts-first node package manager written in rust, with a custom plug'n'play (pnp) runtime.

## Why does zap exist?

- **its fast**: parallel metadata resolution + parallel tarball downloads, and a global content-addressable store (`~/.zap/store`), repeat installs hardlink out of the store and re-download nothing.
- **its also small**: one ~3MB binary, no node runtime required to install.
- **typescript-first**: a `tsconfig.json` (or a pinned version in config) makes zap manage the typescript compiler and `@types/*` for deps that don't ship types. `zap tsc` just works, similar to Yarn berry.
- **no node_modules** (default): zap installs a custom pnp runtime (`.pnp.cjs` + ESM resolve hooks) that maps every `require`/`import` through a flat resolution map. strict per-dependent resolution with a fallback pool for packages that under-declare deps.
- **node_modules when you want it**: set `linker: node-modules` in config for a pnpm-style junction layout, plain `node` works with no hooks, editors get native resolution for free.
- **workspaces**: glob-based monorepo support with one shared resolution tree; members resolve to their real dirs, cross-member deps work in both linkers.
- **multi-run**: `zap run a b c` runs all listed scripts at once in a live split-pane view, bordered panes with per-script status, colours passed through, paged when they don't fit.
- **platform-aware**: per-platform packages (esbuild, rollup, native typescript) download only the variant matching your os/cpu, no linux distros on windows machines.
- **peer-aware**: packages declaring `peerDependencies` get a warning at install when the peer is missing from your project or its version doesn't satisfy the range, with a recommendation, never auto-installed.
- **reproducible**: `.zap-lock.json` records the resolved tree with integrity hashes; unchanged manifests skip resolution.
- **publish-ready**: `zap pack` / `zap publish` / `zap whoami` speak the npm protocol, so private registries via `ZAP_REGISTRY` work too.
- **safe**: every tarball is sha512-verified, and lifecycle scripts never run unless you opt in (`lifecycle: true`).
- **migrate in**: `zap migrate` converts npm/yarn/pnpm/bun/deno projects to zap.

## commands

| command | what it does |
|---|---|
| `zap init` | create a package.json (+ zap runtime ignores in .gitignore) |
| `zap add <pkg>[@range]...` | add deps (`--dev`/`-D`; workspace member names auto-link as `workspace:*`); `-g` installs globally |
| `zap remove <pkg>...` | remove deps; `-g` removes a global package |
| `zap i[nstall]` | install root + workspaces, `--offline`, `--force`, `--refresh`; hand-edited manifests sync on install (missing deps resolve in, removed ones prune out of lockfile + runtime) |
| `zap ci` | clean install, production deps only (docker/CI builds) |
| `zap run <script> [args]` | run a package.json script with the linker active (see running scripts below) |
| `zap run a b c` | run several scripts concurrently in a live split-pane view |
| `zap <script>` | shorthand for run |
| `zap exec <cmd> [args]` | run any command with the linker active |
| `zap x\|dlx <pkg> [args]` | run a package's bin one-off (like npx), works inside or outside any project |
| `zap create <tmpl> [args]` | scaffold via create-* initializers (like npm create); adds zap runtime ignores to the project's .gitignore |
| `zap tsc [args]` | run the zap-managed typescript compiler |
| `zap pack` | build an npm tarball |
| `zap publish [--tag t] [--access a] [--otp c] [--dry-run]` | publish to the registry |
| `zap whoami` | print the authenticated user |
| `zap login` / `zap logout` | authenticate with (or clear credentials for) the registry |
| `zap registry [add\|use\|list\|current]` | switch registries (private -> public flow) |
| `zap upgrade [pkg...]` | no args: re-resolve in ranges; with args: bump to latest; `-i`: pick exact versions per package (no args: all-packages interactive upgrader) |
| `zap outdated` | show deps behind the registry (exit 1 when outdated); `-i` interactively picks upgrades, left/right cycles each package's target: keep (default) → latest of its major → absolute latest |
| `zap version [major\|minor\|patch\|x.y.z]` | bump package.json version (+ git commit/tag `v*`, changelog entry; `--no-git-tag-version`, `-m <msg>`) |
| `zap ls [--all]` | list deps + workspace members |
| `zap why <pkg>` | dependency chains (`--project` scans source files) |
| `zap view\|info <pkg>` | registry metadata for a package (cached) |
| `zap licenses` | license summary of installed packages |
| `zap audit` | check installed packages for known advisories |
| `zap doctor` | sanity checks: lockfile sync, runtime, ts, store |
| `zap bin [name]` | print bin paths |
| `zap store [path\|size]` | inspect the global store |
| `zap cache clean` | wipe store + caches |
| `zap config [-g] [<key> [value]]` | read or set config (no rc file = new `.zaprc.yaml`) |
| `zap migrate` | convert npm/yarn/pnpm/bun/deno projects to zap (+ zap runtime ignores in .gitignore) |
| `zap allow [pkg...]` | allowlist packages whose lifecycle scripts may run (`--remove` to revoke) |
| `zap skills` | inject zap usage into CLAUDE.md / AGENTS.md (`--remove` to strip) |

every command also takes `--help` (`zap install --help`, `zap bump --help`): its usage, a
short explanation and an example use case, in place.

## global installs

`zap add -g <pkg>[@range]` installs a package into its own isolated pnp project under
`~/.zap/global/<name>/` and shims its bins into `~/.zap/bin/` (each shim carries the
package's pnp runtime hooks, so they work from any cwd). add `%ZAP_HOME%\bin` (or
`~/.zap/bin`) to PATH to use them everywhere, zap prints the hint once. one isolated dir
per package means globals can never conflict with each other. `zap remove -g <pkg>` drops
the dir + its shims; `zap ls -g` lists what's installed.

## running scripts

`zap run <script>` streams one script plainly. give `run` several script names and they all
execute at once in a live split-pane view:

- one bordered pane per script with its own status (`✓ exit 0` / `✗ exit N`); the view holds
  after everything finishes until you press enter
- panes page with the left/right arrow keys when the grid doesn't fit the terminal
- script colours are kept (colour is forced on for the pane children), cursor-manipulating
  escape sequences are filtered so the grid stays intact
- arguments after `--` are appended to every script: `zap run test:a test:b -- --watch`
- the exit code is the first failing script's, and a compact summary prints on exit

every `zap run` also honours npm-style hooks: the literal `pre<script>` runs first and
`post<script>` runs last when they exist in package.json. a failing `pre<script>` cancels
the script and its post hook.

## lifecycle scripts

package lifecycle scripts never run unless you opt in with `lifecycle: true`. when enabled,
installs run your root project's own `preinstall`/`install`/`postinstall` in npm order, and
dependency scripts run **only for allowlisted packages**: a package's id and integrity hash
must match an entry in `.zap-allow.json` (next to package.json, commit it).

- `zap allow`, list allowlisted packages + any resolved packages with scripts still pending
- `zap allow <pkg>...`, approve (records the lockfile's hash; a dep bump = new hash = scripts
  blocked again until re-approved)
- `zap allow --remove <pkg>...`, revoke

a package with lifecycle scripts that isn't allowlisted is skipped with a one-line note at
install. workspace members never get lifecycle runs.

trust everything instead? `zap allow --all` sets `lifecycle-allow-all: true` in your `.zaprc`
and deletes the allowlist file, every package's scripts run, no gate. `zap allow --remove
--all` reverts to hash-verified mode.

## configuration

first match walking up from cwd: `.zaprc.yaml`, `.zaprc.yml`, `.zaprc.json`, `.zaprc.jsonc`, `.zaprc.toml`, `.zaprc`, then the same set without the dot. every format is a flat key/value map.

```yaml
linker: pnp            # pnp | node-modules
registry: https://...  # registry override
typescript: "5.9.3"    # pin the compiler version (auto-installed on change)
concurrency: 16        # download parallelism
strict: false          # pnp: disable the fallback pool (fully strict resolution)
save-exact: false      # zap add writes exact versions instead of ^
prefer-offline: false  # always use cached metadata when present
cache-ttl: 6           # metadata cache hours
lifecycle: false       # opt in to preinstall/install/postinstall scripts
store-dir: path        # custom global store location
token: ...             # registry auth token (prefer env in CI)
editor-node-modules: true
```

env: `ZAP_LINKER`, `ZAP_REGISTRY`, `ZAP_TOKEN`/`NPM_TOKEN`, `ZAP_CONCURRENCY`, `ZAP_STRICT`, `ZAP_HOME`.

## ci / docker caching

everything zap persists lives under one directory, `~/.zap` by default, or `$ZAP_HOME` when set, and the store is append-only and content-addressed, so caches compose perfectly with restore-keys.

**github actions** (add to any project using zap):

```yaml
- uses: actions/cache@v4
  with:
    path: ~/.zap
    key: zap-${{ runner.os }}-${{ hashFiles('**/.zap-lock.json') }}
    restore-keys: |
      zap-${{ runner.os }}-
- run: zap install --frozen-lockfile
```

**docker** (buildkit cache mount, installs survive layer invalidation):

```dockerfile
RUN --mount=type=cache,target=/root/.zap zap install
```

zap will also soon ship Docker images for your desired version of node backend for your code, and your desired version of zap to manage it

## typescript

drop a `tsconfig.json` anywhere in the project and zap takes over the toolchain:

- installs the latest typescript into the shared store, shims `tsc`/`tsserver` into `.zap/bin` (so `zap run build` with `tsc ...` works), and keeps it out of your dependencies.
- pin a version with `typescript: "5.9.3"` in any rc file, version changes install automatically.
- scans every direct dep (root + members); deps without their own types get the matching `@types/*` installed as zap-managed type deps (recorded in the lockfile, never in package.json).
- under pnp, `zap tsc` builds a junction farm at `.zap/ts-nm` and points `NODE_PATH` at it so the compiler resolves types; in node-modules mode it's all native.

## workspaces

declare `"workspaces": ["apps/*", "packages/*"]` in the root package.json. every member's deps resolve into one shared tree; `zap add` inside a member edits that member's manifest and installs at the root; members depending on each other just work (use `workspace:*` or the member name).

## limitations (v0.1)

- pnp + editor intellisense: tsserver doesn't resolve pnp maps natively, use the node-modules linker for editor work, or run `zap tsc` for checking
- exports resolution covers exact entries; wildcard export patterns fall back to the filesystem
- lifecycle scripts are opt-in and never run for workspace members
- `files`/`.npmignore` handling in `zap pack` is a pragmatic subset of npm's rules

<hr>

<small>This project was made with small bits of AI assistance, however anything generated has still been reviewed by a human who can read, write and understand Rust!</small>