# savetracker

Track your game saves while you play. Annotate them while you play, or come
back to that later.

savetracker watches game save directories (local or remote), snapshots every
change, diffs versions, and optionally uses [Ollama](https://ollama.com) to
describe what changed in plain language.

## Features

- Watch save files over local filesystem, SSH, FTP, or HTTP
- Auto-detect file formats (JSON, YAML, TOML, XML, INI, binary)
- Configurable format definitions for game-specific decoding pipelines
- Interactive TUI with version history, diffs, and note-taking
- Connection status for remote backends (connected/degraded/lost)
- Ollama integration for AI-powered change descriptions

## Install

```
cargo install --path savetracker
```

## Usage

### Watch saves interactively

```
savetracker watch ./saves -i
```

### Watch with AI descriptions

```
savetracker watch ./saves -i --live --model mistral
```

### Watch remote saves over SSH

```
savetracker watch ssh://user@host/path/to/saves -i --key-path ~/.ssh/id_rsa
```

### Analyze existing snapshots

```
savetracker analyze .savetracker/snapshots
```

### Supported protocols

| Protocol | Example |
|----------|---------|
| Local | `./saves` or `file:///path/to/saves` |
| SSH | `ssh://user@host:22/path` |
| SFTP | `sftp://user@host/path` |
| FTP | `ftp://host/path` |
| HTTP | `https://example.com/saves` |

### Flags

```
savetracker [OPTIONS] <COMMAND>

Options:
  --ollama-url <URL>       Ollama API URL [default: http://localhost:11434]
  --model <NAME>           Ollama model (implies --live for watch)
  --snapshot-dir <PATH>    Custom snapshot storage directory
  --debounce-ms <MS>       Debounce time in milliseconds [default: 2000]
  --max-snapshots <N>      Maximum snapshots to keep [default: 50]
  --format <NAME>          Force a specific save format by name

Commands:
  watch     Monitor a save directory for changes
  analyze   Analyze previously captured snapshots
```

#### watch

```
savetracker watch <URL> [OPTIONS]

Options:
  -i, --interactive        Interactive TUI mode
  --live                   Analyze changes with Ollama in real-time
  --max-versions <N>       Max versions to display in TUI
  --idle-timeout <SECS>    Auto-jump to latest after idle [default: 15]
  --poll-interval <SECS>   Remote polling interval [default: 5]
  --loss-timeout <SECS>    Connection loss threshold [default: 30]
  --key-path <PATH>        SSH key file
  --password <STRING>      Remote authentication password
```

#### Dynamic parameters

Format definitions can declare parameters (e.g., Steam ID for encryption key
derivation). Pass them with `--d:`:

```
savetracker watch ./saves -i --d:steam-id=76561198012345678
```

Parameters are resolved in order: path extraction (automatic), `--d:` flags
(manual), error if required and missing.

## Save format definitions

Format definitions are TOML files that describe how to decode game-specific save
files. They live in `etc/formats/` (built-in, shipped with the binary) and
`~/.config/savetracker/formats/` (user-added).

When a save file is detected, savetracker scores each definition by extension
match, path pattern match, and magic byte prefix match. The highest-scoring
definition's pipeline runs to decode the file. If nothing matches, generic
auto-detection kicks in.

### Schema

```toml
[format]
name = "my_game"
display_name = "My Game"

[detect]
extensions = [".sav"]
magic_bytes = "deadbeef"   # optional, hex prefix

[detect.platform.windows]
path_patterns = ["**/My Game/Saves/**/*.sav"]

[detect.platform.linux]
path_patterns = ["**/compatdata/*/pfx/**/My Game/**/*.sav"]

# Ordered decode pipeline — runs top to bottom
[[pipeline]]
type = "aes_ecb_decrypt"
key_hex = "00112233..."
key_transform = "xor_prefix"
key_transform_param = "steam_id"
key_transform_bytes = 8

[[pipeline]]
type = "pkcs7_unpad"

[[pipeline]]
type = "zlib_decompress"

# What the decoded bytes are (auto-detected if omitted)
[output]
format = "yaml"

# Parameters — extracted from path or provided via --d: flags
[params.steam_id]
flag = "steam-id"
description = "Steam ID (64-bit decimal)"
required = true
extract_from_path = "**/Saves/{}/Profiles/**"
```

### Pipeline layers

| Layer | Fields | Description |
|-------|--------|-------------|
| `gzip_decompress` | | Gzip decompression |
| `zlib_decompress` | | Zlib decompression |
| `zstd_decompress` | | Zstandard decompression |
| `lz4_decompress` | | LZ4 decompression |
| `aes_ecb_decrypt` | `key_hex`, `key_transform`?, `key_transform_param`?, `key_transform_bytes`? | AES-256 ECB |
| `aes_cbc_decrypt` | `key_hex`, `iv_hex`, same transform fields | AES-256 CBC |
| `pkcs7_unpad` | | Remove PKCS#7 padding |
| `xor` | `key_hex` | XOR with repeating key |
| `skip_bytes` | `count` | Strip leading bytes |
| `take_bytes` | `offset`, `length` | Extract byte range |

### Key transforms

The `key_transform` field names an operation applied to the key before
decryption:

- `xor_prefix` — XOR the first `key_transform_bytes` bytes of the key with a
  parameter value interpreted as a little-endian u64. The parameter is named by
  `key_transform_param`.

### Path extraction

Parameters can be auto-extracted from the file path using `extract_from_path`.
The pattern uses `**` for any path segments and `{}` to capture the value:

```
**/SaveGames/{}/Profiles/**
```

Given path `.../SaveGames/76561198012345678/Profiles/client/1.sav`, this
captures `76561198012345678` as the parameter value.

### Detection scoring

| Match type | Score |
|------------|-------|
| File extension | 1 |
| Path pattern (glob) | 5 |
| Magic bytes (hex prefix) | 10 |

### Contributing format definitions

Add a `.toml` file to `etc/formats/` following the schema above and open a PR.
Test your definition with `cargo test` — the format module tests parse all
embedded definitions.

## TUI keybindings

| Key | Action |
|-----|--------|
| Tab / Shift+Tab | Navigate between versions |
| Alt+D | Toggle detail diff overlay |
| Alt+E | Open notes in external editor ($EDITOR) |
| PageUp / PageDown | Scroll diff |
| Ctrl+C | Quit |

## License

BSD-2-Clause
