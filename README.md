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

## Why does Zap exist?

- **its fast**: parallel metadata resolution + parallel tarball downloads, and a global content-addressable store (`~/.zap/store`), repeat installs hardlink out of the store and re-download nothing.
- **its also small**: one ~3MB binary, no node runtime required to install.
- **typescript-first**: a `tsconfig.json` (or a pinned version in config) makes zap manage the typescript compiler and `@types/*` for deps that don't ship types. `zap tsc` just works, similar to Yarn berry.
- **no node_modules** (default): zap installs a custom pnp runtime (`.pnp.cjs` + ESM resolve hooks) that maps every `require`/`import` through a flat resolution map. strict per-dependent resolution with a fallback pool for packages that under-declare deps.
- **node_modules when you want it**: set `linker: node-modules` in config for a pnpm-style junction layout, plain `node` works with no hooks, editors get native resolution for free.
- **workspaces**: glob-based monorepo support with one shared resolution tree; members resolve to their real dirs, cross-member deps work in both linkers.
- **reproducible**: `.zap-lock.json` records the resolved tree with integrity hashes; unchanged manifests skip resolution.
- **publish-ready**: `zap pack` / `zap publish` / `zap whoami` / `zap version` speak the npm protocol, so private registries via `ZAP_REGISTRY` work too.
- **safe**: every tarball is sha512-verified, and lifecycle scripts never run unless you opt in (`lifecycle: true`).

## commands

| command | what it does |
|---|---|
| `zap init` | create a package.json |
| `zap add <pkg>[@range]...` | add deps (`--dev`/`-D`; workspace member names auto-link as `workspace:*`) |
| `zap remove <pkg>...` | remove deps |
| `zap i[nstall]` | install root + workspaces, `--offline`, `--force`, `--refresh` |
| `zap run <script> [args]` | run a package.json script with the linker active |
| `zap <script>` | shorthand for run |
| `zap exec <cmd> [args]` | run any command with the linker active |
| `zap x\|dlx <pkg> [args]` | run a package's bin one-off (like npx) |
| `zap create <tmpl> [args]` | scaffold via create-* initializers (like npm create) |
| `zap tsc [args]` | run the zap-managed typescript compiler |
| `zap version [major\|minor\|patch\|x.y.z]` | bump package.json version |
| `zap pack` | build an npm tarball |
| `zap publish [--tag t] [--access a] [--otp c] [--dry-run]` | publish to the registry |
| `zap whoami` | print the authenticated user |
| `zap upgrade [pkg...]` | no args: re-resolve in ranges; with args: bump to latest |
| `zap outdated` | show deps behind the registry (exit 1 when outdated) |
| `zap ls [--all]` | list deps + workspace members |
| `zap why <pkg>` | dependency chains |
| `zap bin [name]` | print bin paths |
| `zap store [path\|size]` | inspect the global store |
| `zap cache clean` | wipe store + caches |

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