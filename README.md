# bacnet-kit

Commission BACnet devices from the command line or a Rust application. Read values, units and status over BACnet/IP, read a property through a Secure Connect hub, or inspect a captured MS/TP frame offline.

## Install and try it

Install the CLI with Homebrew, or use Cargo with Rust 1.95 or later. Both commands below include Secure Connect.

```sh
brew install hupe1980/tap/bacnet-kit
# Or with Cargo
cargo install bacnet-kit --locked --features sc-node
```

`bacnet-kit demo` starts a private loopback device, reads its temperature and exits.

```text
Analog Input 1   21.5 °C
Status           normal
```

For an interactive session, start `bacnet-kit serve` in one terminal. In another, try:

```sh
bacnet-kit inspect 127.0.0.1:47808
bacnet-kit read 127.0.0.1:47808 1
bacnet-kit batch --help
```

## Choose a workflow

| Task | Command |
| --- | --- |
| Read multiple properties or array entries | `batch` |
| Watch an Analog Input with verified readback | `watch` |
| Test priority writes and relinquishment on the simulator | `command-demo` |
| Read a routed station or receive forwarded announcements | `read` with a routed target, or `foreign` |
| Check staged credentials or read through a Secure Connect hub | `sc-check`, `sc-read` |
| Inspect one captured MS/TP frame | `mstp-frame` |
| Capture the reference device’s capability worksheet | `pics` |

The [field guide](https://hupe1980.github.io/bacnet-kit/docs/) includes worked examples, expected output and troubleshooting.

## Use the library

For the blocking BACnet/IP client, add this dependency to `Cargo.toml`:

```toml
[dependencies]
bacnet-kit = { version = "0.1", default-features = false, features = ["std"] }
```

Omit `features = ["std"]` for the allocator-free responder and MS/TP inspector. Enable `sc-node` for host-side TLS reads through a primary or failover hub. [Rust API reference](https://docs.rs/bacnet-kit)

## Development

Run `just check` for host tests and linting, `just firmware` for the Cortex-M4 fixture, or `just site` to preview the field guide. [Interoperability checks](https://hupe1980.github.io/bacnet-kit/docs/verification/)

Licensed under [MIT](https://github.com/hupe1980/bacnet-kit/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/hupe1980/bacnet-kit/blob/main/LICENSE-APACHE).
