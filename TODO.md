# zap todo

## finished
- [x] core install/add/remove with pnp + node_modules linkers, lockfile, global store
- [x] workspaces (globs, cross-member deps, `workspace:*`)
- [x] `.zaprc` family (yaml/yml/json/jsonc/toml/plain) + `$schema` auto-injection
- [x] ts-first: managed/user typescript, `@types/*` auto-install, `zap tsc`
- [x] `zap x` local-first bins, `dlx`, `run`/`r`/`task`, `exec`
- [x] publish suite: `pack`, `publish` (web-auth login, interactive OTP, nameAliases), `whoami`
- [x] registry switching (`zap registry add/use/list/current`, `--registry` override)
- [x] dev utils: `view/info`, `licenses`, `audit`, `doctor`, `outdated`, `upgrade`, `store`, `bin`, `cache clean`
- [x] `zap config` (read/set, global with -g), `zap skills` (inject usage into CLAUDE.md/AGENTS.md), `zap ci` (clean production-only install)
- [x] `.zapignore` for pack
- [x] linux/macos portability (unix shims, symlinks, rustls) - linux verified in WSL, macos rides CI
- [x] CI caching (ZAP_HOME, `--frozen-lockfile`), Dockerfile, build-only workflows
- [x] zap migrate command
- [x] per-command heading/banner art (`zap.emblem.txt`, `zap.heading.txt`)
- [x] command registry refactor: one file per command under `src/cmds/<group>/`, build.rs-generated registry drives dispatch, headings and help automatically
- [x] multi-run: `zap run a b c` runs all listed scripts concurrently in a split-pane alternate-screen view (`zap.text.txt` wordmark, per-pane status/exit codes), args after `--` go to every script
- [x] npm-style pre/post script hooks: literal `pre<script>`/`post<script>` run around every `zap run`
- [x] root project lifecycle scripts during install (`preinstall`/`install`/`postinstall`, npm order, opt-in `lifecycle: true` alongside dep lifecycles)
- [x] lifecycle scripts sandboxing: `.zap-allow.json` hash allowlist — with `lifecycle: true`, only packages whose pkg-id + integrity hash are allowlisted (via `zap allow`) run their scripts; root lifecycle unaffected
- [x] `zap allow --all`: trust-all mode (`lifecycle-allow-all` in .zaprc, allowlist file removed) + `zap allow --remove --all` to revert
- [x] extra: move commands system to file-based handler system
- [x] extra: categorised commands in help menu
- [x] zap headings added
- [x] multi-command
- [x] lifecycle scripts sandboxing (allowlist by package hash)


## successful runs
- [x] first real CI run on github (verifies macos + publishes artifacts)

## next up
- [ ] node API for zap as a native npm package (napi-rs: install/add/remove/resolve/list/why/pack/publish)
- [ ] `zap outdated` interactive upgrade mode (yarn upgrade-interactive style)
- [ ] `zap version` for real (currently an honest not-implemented stub): major|minor|patch|x.y.z bumps + git tag + changelog generation
- [ ] colour and make our terminal more fun? [in progress]
- [ ] zap installer, try from zap.nn140.uk/versions/latest:dev:v0.0.0, if fail then try from github releases
- [ ] "zap console" a tui for using zap for essentially an entire command window, only usable in non-cicd environments

## later / ideas
- [ ] editor story for pnp: tsserver integration or pnpify-style wrapper
- [ ] audit `--audit-level` filtering + fix suggestions
- [ ] dedupe command (yarn-deduplicate style: collapse transitive version forks)
- [ ] workspaces: per-member lockfile sections, `zap workspaces list/run-all`
- [ ] zap plugins/extension points (yarn v1 plugin spirit)
- [ ] offline mirror (yarn offline-mirror style: commit tarballs to git)
- [ ] make a graphical zap installer with gpuix for a rust learning experiment
- [ ] zap website at nn140.uk to host versions, downloads and usage docs
- [ ] graphical manager with gpuix for zap? (cache, project management, etc)
- [ ] plugin framework to go into .zap/plugins
- [ ] hooks framework to go into .zap/hooks
- [ ] custom commands framework to go into .zap/commands

## planned side projects
- [ ] rust registry server
- [ ] cloudflare workers registry, same thing as the rust registry, but on workers instead of docker or bare-metal
- [ ] docker image suite
