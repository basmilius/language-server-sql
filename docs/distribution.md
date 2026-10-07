# Distribution

Every release builds the server for four platforms and attaches the archives to the GitHub release, with a checksum file each and one descriptor that an installer pins.

| Platform       | Cargo target                | Archive  | Executable                |
| -------------- | --------------------------- | -------- | ------------------------- |
| `darwin-arm64` | `aarch64-apple-darwin`      | `tar.gz` | `sql-language-server`     |
| `linux-arm64`  | `aarch64-unknown-linux-gnu` | `tar.gz` | `sql-language-server`     |
| `linux-x64`    | `x86_64-unknown-linux-gnu`  | `tar.gz` | `sql-language-server`     |
| `win32-x64`    | `x86_64-pc-windows-msvc`    | `zip`    | `sql-language-server.exe` |

The platform names are Node's `<process.platform>-<process.arch>`. macOS is Apple silicon only. There is no Intel Mac, musl Linux or Windows on Arm build; a platform without an entry is unsupported, not something to fall back from.

An archive is named `sql-language-server-v<version>-<platform>.<format>` and holds the executable, the license, `THIRD-PARTY.md`, the licenses of the Rust dependencies and a `native-source.json` that records the build. Next to it, `<archive>.sha256` holds its checksum.

## The descriptor

`sql-language-server-release.json` describes one release:

```json
{
    "version": "0.1.0",
    "sourceRevision": "<the commit the archives were built from>",
    "assets": {
        "darwin-arm64": {
            "url": "https://github.com/basmilius/language-server-sql/releases/download/v0.1.0/sql-language-server-v0.1.0-darwin-arm64.tar.gz",
            "sha256": "<checksum>",
            "format": "tar.gz",
            "executable": "sql-language-server"
        }
    }
}
```

`version` is the release's and must match `--version`. An installer pins a descriptor and, for the platform it runs on:

1. Takes the asset of the platform, or stops when there is none.
2. Downloads the archive and checks its SHA-256 against `sha256` before unpacking anything.
3. Unpacks it into the app's own folder, keeps the executable bit, and checks `--version` against `version`.

## Metadata

`native-source.json` holds the server `version` and the platforms with their targets. `python3 scripts/test-native-release.py` fails when it disagrees with `Cargo.toml`.

## Local archives

For a local archive of a binary you built:

```sh
python3 scripts/native-release.py asset \
    --binary target/release/sql-language-server \
    --platform darwin-arm64 --tag v0.1.0-local --output artifacts
```

The tag is `v<version>` or ends in `-local`. The script checks the binary's version and the pins, not its architecture, so pick the platform that matches. `native-release.py merge --input artifacts --output descriptor.json` needs one archive and descriptor for every platform and checks every checksum.
