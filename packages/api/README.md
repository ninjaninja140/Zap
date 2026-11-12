```
          ██          
         ██          ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁
       ███           ░        ░░░      ░░░       ░░
     ████            ▒▒▒▒▒▒  ▒▒▒  ▒▒▒▒  ▒▒  ▒▒▒▒  ▒
    █████████        ▓▓▓▓  ▓▓▓▓▓  ▓▓▓▓  ▓▓       ▓▓
        ████         ██  ███████        ██  ███████
       ███           █        ██  ████  ██  ███████
      ██             ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
     ██         
```

# @zappkg/api

programmatic zap from node, the node api for the
[zap](https://github.com/ninjaninja140/zap) package manager

the api that cam drive zap programmatically: fully typed and jsdoc-ified for convenience :)

## usage

```ts
import { Zap } from '@zappkg/api';

// the shared default instance: process.cwd(), binary via
// ZAP_API_PATH → @zappkg/cli vendored binary → PATH
const zapApi = new Zap();

await zapApi.add(['chalk'], { dev: true });         // → ["add", "chalk", "--dev"]
await zapApi.install({ frozenLockfile: true });

const l = await zapApi.ls();                        // parsed ls --json (LsJson)
const w = await zapApi.why('chalk');                // parsed chains (WhyJson)
const v = await zapApi.view('chalk');               // packument summary (ViewJson)
const o = await zapApi.outdated();                  // {outdated, upToDate} rows

// per-instance knobs: run zap in another project, use a specific binary
const z = new Zap({
  cwd: 'C:/dev/my-project',
  binary: 'C:/dev/Zap/target/debug/zap.exe',        // wins over the chain above
});
await z.ls();                                       // that project's listing
await z.why('chalk');
```

## dev

```sh
node scripts/smoke.cjs   # with ZAP_API_PATH set + cwd in a zap project
```
