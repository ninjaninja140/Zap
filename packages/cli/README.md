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

# @zappkg/cli

the npm install method for zap: a tiny, fast, typescript-first node package
manager written in rust. this package ships a `zap` shim that runs a prebuilt
native binary: nothing is compiled at install time, no node-gyp, node >= 18.

## install

```sh
npm install -g @zappkg/cli
```

the postinstall script downloads the prebuilt zap binary for your platform
into `vendor/`; the `zap` shim then hands off args + exit codes to it.
supported platforms: win32/darwin/linux × x64/arm64.

## dev loop

build zap locally, then point the shim at it instead of vendoring:

```sh
cargo build
$env:ZAP_DEV_PATH = "$(pwd)\target\debug\zap.exe"   # powershell
npm install ./packages/cli                          # postinstall skips itself
npx zap --version
```