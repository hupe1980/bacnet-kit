+++
title = "Read your first point"
description = "Run a local fixture, enumerate the object list and read a temperature."
weight = 1

[extra]
group = "Read and control"
terms = "quickstart IP UDP device object temperature"
+++

A BACnet device exposes objects. An **Analog Input** represents a measurement, and its **Present_Value** property holds the current reading. This example reads the supply temperature from Analog Input 1 on a simulated controller.

Install the CLI with Homebrew, or run `cargo install bacnet-kit --locked --features sc-node` with Rust 1.95 or later. Then take a reading on loopback.

```sh
brew install hupe1980/tap/bacnet-kit
bacnet-kit demo
```

The demo opens two loopback UDP sockets, reads Analog Input 1 and exits. Its output includes the present value, engineering unit and status.

```text
Analog Input 1   21.5 °C
Status           normal
```

## Find command syntax

Run `bacnet-kit --help` to see the commands in this build. Use `bacnet-kit batch --help` for a command’s arguments. Named options can appear before or after the target.

For shell scripts, exit status 0 means success (including help), 2 means the command line could not be parsed, and 1 means the operation failed. A batch can print successful rows before reporting a property error.

## Use separate processes

Start the reference device in one terminal. Its Device instance is 123 and its Analog Input instance is 1.

```sh
bacnet-kit serve
```

In another terminal, inspect the device and read its point.

```sh
bacnet-kit inspect 127.0.0.1:47808
bacnet-kit read 127.0.0.1:47808 1
```

The reader resolves the Device instance at the specified endpoint. It reads Object_List index zero for the count, then reads each entry individually. This also works when a peer cannot send its entire array in a single response. For a commissioned device, substitute its IPv4 address, UDP port and Analog Input instance from the point schedule. BACnet International documents both the wildcard Device identity and the array-element approach in its [Guide to the Device Object](https://thebacnetinstitute.org/wp-content/uploads/sites/5/2026/01/Guide-to-the-Device-Object.pdf).

## Interpret point quality

Restart the fixture with `serve --fault` and repeat the read. It still reports 21.5 °C. The fault flag tells the consuming application that the reported value needs attention. The other flags are in-alarm, overridden and out-of-service. The sample keeps the instant when Present_Value arrived. Units and flags are collected with separate reads within the same deadline.

## Diagnose a failed read

| Result | What to check |
| --- | --- |
| Timeout | Verify the endpoint, interface, local route and peer availability. The CLI allows three seconds for the whole operation. |
| Remote BACnet error | The peer answered. For example class 1, code 31 means unknown object. Check the instance against the inventory. |
| Reject or Abort | Inspect the reason returned by the peer and its supported service and segmentation capabilities. |
| Inventory limit | The peer reports more than 128 objects. Use the Rust property API to read specific indexed entries. |

The client uses directed reads on a trusted local IPv4 link. A matching UDP response identifies a transaction but supplies no cryptographic proof of origin.
