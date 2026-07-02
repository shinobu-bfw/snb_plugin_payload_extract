# snb_plugin_payload_extract

A [Shinobu](https://github.com/shinobu-bfw/shinobu) plugin that extracts, lists,
inspects, and KernelSU-patches Android partitions from a `payload.bin` / OTA zip
URL. It is a port of [`payload_extract_bot-rs`](https://github.com/YuKongA/payload_extract_bot-rs)
to a standard snb plugin.

The plugin emits and receives events through the bot runtime, so it needs an
adapter (e.g. `snb_adapter_tg`) to talk to a chat platform.

## Commands

| Command | Args | Notes |
| --- | --- | --- |
| `/dump` (`/dumper`) | `<url> <p1,p2,...>` | Extract partitions and upload them. |
| `/list` | `<url>` | List partitions and sizes. |
| `/meta` (`/metadata`) | `<url>` | Show OTA metadata. |
| `/patch` | `<url> <partition> [kmi]` | KernelSU-patch `boot`(`b`) / `init_boot`(`ib`) / `vendor_boot`(`vb`). |
| `/update` | — | Admin only. Re-download the latest `ksud`. |
| `/status` | — | Admin only. Show host system info. |

## Platforms

`/dump`, `/list`, and `/meta` are pure Rust and run anywhere.

`/patch` and `/update` shell out to `ksud`, downloaded on demand from KernelSU CI
into `data/PayloadExtract/bin/<os>/<arch>/`. Supported targets:

| OS | Arch | ksud target |
| --- | --- | --- |
| linux | x86_64 / aarch64 | `*-unknown-linux-musl` |
| android | x86_64 / aarch64 | `*-linux-android` |
| macos | x86_64 / aarch64 | `*-apple-darwin` |
| windows | x86_64 | `x86_64-pc-windows-gnu` |

KernelSU CI does not publish a Windows aarch64 `ksud`, so `/patch` and `/update`
are unavailable on Windows arm64.

## Configuration

On first load a default config is written to `configs/PayloadExtract/config.toml`:

```toml
SUPPORTED_PARTITIONS = ["abl*", "boot", "dtbo", "init_boot", "modem",
    "modemfirmware", "recovery", "system_dlkm", "vbmeta*", "vendor_boot",
    "vendor_dlkm", "xbl*"]
ADMIN_USERS = []
```

- `SUPPORTED_PARTITIONS` — partitions allowed for `/dump` (empty = allow all).
  Entries may use glob wildcards: `*` (any run of characters) and `?` (a single
  character), e.g. `xbl*` allows `xbl_a`, `xbl_config_b`, ….
- `ADMIN_USERS` — user IDs allowed to run `/update` and `/status` (an adapter that
  marks the sender as admin is also accepted).

The Telegram token / API URL are configured on the adapter
(`configs/TGAdapter/config.toml`), not here.

## Build

```shell
cargo build-plugin payload_extract          # or: cargo xtask build-plugin payload_extract
```

This emits `snb_plugin_payload_extract.{so,dylib,dll}` into `target/`, ready to
load like any other plugin.
