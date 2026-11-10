# zap todo

## shipped
- [x] core install/add/remove with pnp + node_modules linkers, lockfile, global store
- [x] workspaces (globs, cross-member deps, `workspace:*`)
- [x] `.zaprc` family (yaml/yml/json/jsonc/toml/plain) + `$schema` auto-injection
- [x] ts-first: managed/user typescript, `@types/*` auto-install, `zap tsc`
- [x] `zap x` local-first bins, `dlx`, `run`/`r`/`task`, `exec`
- [x] publish suite: `pack`, `publish` (web-auth login, interactive OTP, nameAliases), `whoami`, `version`
- [x] registry switching (`zap registry add/use/list/current`, `--registry` override)
- [x] dev utils: `view/info`, `licenses`, `audit`, `doctor`, `outdated`, `upgrade`, `store`, `bin`, `cache clean`
- [x] `.zapignore` for pack
- [x] linux/macos portability (unix shims, symlinks, rustls) - linux verified in WSL, macos rides CI
- [x] CI caching (ZAP_HOME, `--frozen-lockfile`), Dockerfile, build-only workflows

## successful runs
- [x] first real CI run on github (verifies macos + publishes artifacts)

## next up
- [ ] node API for zap as a native npm package (napi-rs: install/add/remove/resolve/list/why/pack/publish)
- [ ] put zap.exe / zap shim on PATH on both machines (or `zap self install`)
- [ ] `zap outdated` interactive upgrade mode (yarn upgrade-interactive style)
- [ ] publish: git tag + changelog generation on `zap version`
- [ ] lifecycle scripts sandboxing (allowlist by package hash)
- [ ] colour and make our terminal more fun?

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

## planned side projects
- [ ] rust registry server
- [ ] cloudflare workers registry, same thing as the rust registry, but on workers instead of docker or bare-metal
- [ ] docker image suite
