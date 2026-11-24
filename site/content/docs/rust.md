+++
title = "Use the Rust API"
description = "Choose the blocking host client or the allocation-free device core."
weight = 8

[extra]
group = "Development and verification"
terms = "no_std embedded API Cargo library firmware"
+++

## Host client

The `std` feature provides the blocking client. The default `cli` feature adds the executable and enables `std`. To use only the host library, set `default-features = false, features = ["std"]` on the dependency. Run `bacnet-kit serve` in another terminal for the loopback examples. The snippets using `?` belong in a function that returns `Result`. Give each concurrent worker its own client. A client binds an ephemeral UDP port, serializes its own reads and matches the full peer endpoint and property reference. Use `Client::bind` to select a specific local interface.

```rust
use bacnet_kit::client::Client;
use std::time::{Duration, Instant};

let peer = "127.0.0.1:47808".parse()?;
let mut client = Client::connect(peer, Duration::from_secs(3))?;
let sample = client.sample(1)?;
if !sample.flags.fault && !sample.is_stale(Instant::now(), Duration::from_secs(10)) {
    println!("{} (BACnet unit {})", sample.value, sample.unit);
}
```

`Client::read` returns the object identity and owned application-tagged bytes. Use `Reading::scalar` for one typed value. For a BACnetARRAY pass `None` for the entire array, `Some(0)` for the length and `Some(1)` for the first element. Preserve the previous sample if a refresh fails so the original observation time remains available.

Use `Client::capabilities(instance)` to read the Device’s two capability bit strings under a shared timeout. For each, `width()` includes clear trailing bits and `positions()` yields every set position, including unknown extensions. Each string is bounded at 1,024 positions. Use `inventory_of(instance)` to enumerate actual instances. [Reference fixture worksheet](@/docs/pics.md)

## Inspect MS/TP capture bytes

`mstp::inspect(&mut record)` accepts one complete frame in caller-owned scratch. The returned `Observation` owns its metadata and has no borrow on the scratch. Keep a separate original if you need the captured bytes afterward. Extended COBS decoding can change the scratch even when a later check rejects a record. Use `mstp::MAX_RECORD` for the storage bound and `InspectError` to distinguish unsupported types, malformed wire fields and extra bytes. [Capture field guide](@/docs/mstp.md)

## Check staged credentials

Enable the `sc-pki` Cargo feature to use the same check as the CLI. The function accepts PEM bytes and a caller-supplied evaluation time. Applications can match on `CheckError` variants such as `Revoked`, `KeyMismatch` and `InsufficientLifetime`. A successful result includes both SHA-256 fingerprints and the effective expiry in Unix seconds.

```rust
use bacnet_kit::sc_pki::{check, CheckError, CredentialReport};
use std::time::SystemTime;

fn preflight(leaf: &[u8], key: &[u8], signers: &[u8], crl: Option<&[u8]>)
    -> Result<CredentialReport, CheckError>
{
    check(leaf, key, signers, crl, SystemTime::now(), 30)
}
```

## Read through an SC hub

Enable `sc-node` and call `sc_node::Node::new(uuid, credentials)` with the persistent commissioning UUID and borrowed PEM buffers. In a Tokio runtime, call `node.read(primary, backup, target_vmac, &property).await`. A mutable node serializes operations and preserves its local identity. `Outcome::reading` uses the same property representation as the UDP client. `primary_failure` records why a successful operation needed the backup. On failure, `ReadError` retains the outcomes of both attempted hubs. [Hub read workflow](@/docs/sc-read.md)

## Read a batch

Pass explicit references to the same client. A transaction error returns from `read_multiple` directly. Once a reply matches the whole request, inspect each row's `outcome` for a value or a BACnet property error.

```rust
use bacnet_kit::{ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest};

let input = ObjectIdentifier::new(ObjectType::ANALOG_INPUT, 1)?;
let refs = [Prop::PRESENT_VALUE, Prop::DESCRIPTION].map(|property_identifier| {
    ReadPropertyRequest {
        object_identifier: input,
        property_identifier,
        property_array_index: None,
    }
});
for row in client.read_multiple(&refs)? {
    match row.outcome {
        Ok(value) => println!("{:?}", value.scalar()?),
        Err(error) => eprintln!("{}: class {}, code {}", row.reference.property_identifier, error.class, error.code),
    }
}
```

Call `client.negotiate(123)?` once when the remote Device instance is known. It returns effective limits and applies them to subsequent reads on that client. A zero `max_request_segments` means the remote Device only accepts unsegmented requests. A failed refresh returns an error and preserves the previous limits. Refresh the limits after a controller reconfiguration. Each read has its own deadline and returns only after the response matches its requested references.

## Reach a routed station or a BBMD

`Client::via` takes a local interface, the router endpoint, a `RemoteStation` and a timeout. The client retains that route when reading a batch or starting a watch. Call `reader.inventory_of(77)?` to verify a commissioned Device instance and enumerate its Object_List under one deadline. To receive I-Am announcements, open a `ForeignListener` on the intended interface and drive it until the observation window ends.

```rust
use bacnet_kit::client::{Client, ForeignEvent, ForeignListener};
use std::{net::Ipv4Addr, time::{Duration, Instant}};

let interface = Ipv4Addr::UNSPECIFIED;
let router = "127.0.0.1:47808".parse()?;
let mut reader = Client::via(interface, router, "200:0x2a".parse()?, Duration::from_secs(3))?;
let sample = reader.sample(1)?;

let bbmd = "127.0.0.1:47809".parse()?;
let mut listener = ForeignListener::bind(interface, bbmd, 60)?;
let end = Instant::now() + Duration::from_secs(15);
while let Some(event) = listener.next_until(end)? {
    match event {
        ForeignEvent::Announcement(a) => println!("{} from {}", a.device.object_identifier, a.origin),
        ForeignEvent::Registration(change) => println!("{change:?}"),
    }
}
```

## Subscribe to one Analog Input

`Client::watch(device, input, options)` consumes the client and verifies the baseline. Use `latest()` for the last complete sample, including its original observation time. Call `next_until` repeatedly using a monotonic deadline. This drives subscription, renewal, notification handling and timed reads on the calling thread.

```rust
use bacnet_kit::client::{WatchEvent, WatchOptions};

let mut watch = client.watch(123, 1, WatchOptions::default())?;
let end = Instant::now() + Duration::from_secs(15);
while let Some(event) = watch.next_until(end)? {
    match event {
        WatchEvent::Observation { trigger, sample, gap } =>
            println!("{trigger:?} {} {:?} gap={gap}", sample.value, sample.flags),
        WatchEvent::Subscribed => println!("lease acknowledged"),
        WatchEvent::VerificationFailed(e) | WatchEvent::SubscriptionFailed(e) =>
            eprintln!("{e}"),
    }
}
watch.cancel()?;
```

Choose a freshness budget that allows four consecutive directed reads on the intended link. Each group shares a deadline bounded by the client timeout, a quarter of the freshness budget and a quarter of the lease. Dropping the watch closes both sockets. Use explicit cancellation to release the lease immediately. [Watch options and recovery](@/docs/watch.md)

## Embedded core

Disable default features for the allocation-free profile. The caller supplies datagrams, receive provenance and response storage. `Device::respond` returns a length for the response or `None` when the datagram needs no reply. Pass the response to the original UDP source, including its port.

```sh
cargo check --locked --no-default-features --target thumbv7em-none-eabihf
cargo doc --locked --no-deps --open
```

The default simulator contains two objects and serves ReadProperty, ReadPropertyMultiple and Who-Is. It supports the unsegmented local link and exposes an in-process fault switch. Use a product-owned vendor ID and a validated product-specific object model when developing firmware.

## Supply a command policy

Keep one `CommandSlots` per simulated device across datagrams. Pass it to `Device::respond_with_commands` for every request handled by that endpoint, including reads and discovery. The callback sees the normalized priority and an optional finite REAL. `None` relinquishes the slot. Capture transport provenance or application configuration in the callback. A grant applies to one request.

```rust
use bacnet_kit::{Device, commands::CommandSlots, wire::WireError};
use std::net::SocketAddrV4;

fn respond(device: &Device, slots: &mut CommandSlots, peer: SocketAddrV4,
           input: &[u8], output: &mut [u8]) -> Result<Option<usize>, WireError> {
    device.respond_with_commands(input, output, slots, |change| {
        peer.ip().is_loopback() && matches!(change.priority(), 8 | 16)
    })
}
```

Local UDP source addresses supply transport context. Every process with access to this loopback endpoint can request changes at the permitted priorities.
