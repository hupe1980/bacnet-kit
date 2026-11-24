+++
title = "Interoperability checks"
description = "Reproduce commissioning reads, credential checks and embedded tests with independent peers."
weight = 11

[extra]
group = "Development and verification"
terms = "testing QEMU Cortex-M4 interoperability evidence"
+++

Run these checks from the source checkout. The C peer is bacnet-stack at commit `5922a38bee436923dec159bdea9bd21445ec2f38`. The Python BACnet/IP fixtures use BACpypes3 0.1.0. [Pinned C peer](https://github.com/bacnet-stack/bacnet-stack/tree/5922a38bee436923dec159bdea9bd21445ec2f38)

## Independent UDP peers

Clone the pinned C source into `peer` and build its BACnet/IP tools.

```sh
git clone https://github.com/bacnet-stack/bacnet-stack.git peer
git -C peer checkout 5922a38bee436923dec159bdea9bd21445ec2f38
cmake -S peer -B peer/build -DBACNET_STACK_BUILD_APPS=ON -DBACDL_BIP=ON
cmake --build peer/build --target server readprop readpropm writeprop
cargo build --locked
python3 scripts/check-interop.py peer/build
```

The Rust client inventories C Device 4200 and reads Analog Input 1. In the reverse direction, the C tools read the Rust fixture’s objects, check ordered batch results, write at priorities 8 and 16, and verify relinquishment. Priority 7 must be denied.

## Segmented transfers and COV

```sh
uv run scripts/check-segmentation.py
uv run scripts/check-cov.py
```

The segmentation fixture reads a 1,680-character value across seven reply segments and sends 24 property references across two request segments. The COV fixture exercises confirmed and unconfirmed notifications. It drops a subscription, changes the point, and checks that a verification read finds the change within one second. Renewal restores notifications and cancellation removes the subscription.

## Routed read and foreign registration

`uv run scripts/check-routing.py` inventories Device 77 and reads Analog Input 1 behind a router. It also verifies forwarded I-Am announcements, refused renewal, lease expiry and re-registration with the same endpoint.

To exercise actual subnet broadcasts on Linux, install iproute2 and util-linux. The script creates temporary network namespaces for the BBMD, gateway and workstation.

```sh
peer_python=$(uv run --with bacpypes3==0.1.0 python -c 'import sys; print(sys.executable)')
sudo scripts/check-routing-subnets.sh "$peer_python"
```

## Staged Secure Connect credentials

```sh
cargo build --locked --features sc-pki
uv run tests/test_sc_credentials.py
```

OpenSSL generates node keys and CSRs. The fixture issues its own certificates and CRLs, then checks direct signers, key mismatch, expiry, signer rollover, revocation and malformed files.

## Live Secure Connect hubs

On Linux, install the development headers for OpenSSL and libwebsockets, plus pkg-config. Using the same C checkout, build the hubs and field device.

```sh
cmake -S peer -B peer/build-sc -DBACNET_STACK_BUILD_APPS=ON -DBACDL_BSC=ON -DBACDL_BIP=OFF
cmake --build peer/build-sc --target sc-hub server --parallel 4
cargo build --locked --features sc-node
BACNET_SC_PEER_BIN="$PWD/peer/build-sc" uv run tests/test_sc_hub.py
```

The node reads Device 4200 through the primary hub, then repeats the read through the backup after the primary stops. The Python peer separately exercises TLS authentication, revocation, WebSocket negotiation, unrelated replies and deadlines.

## Offline MS/TP frames

```sh
scripts/check-mstp-vectors.sh peer
cargo test --locked --test mstp
```

The C encoder reproduces the three binary data fixtures. Rust tests also inspect a captured token and reject corrupt, truncated and concatenated records. Physical RS-485 timing and driver turnaround require a trace from the target board.

## Run the bare-metal fixture

Install QEMU’s ARM system emulator, then run:

```sh
rustup target add thumbv7em-none-eabihf
just firmware
```

The Cortex-M4 image executes a read, a mixed batch, a supervised write with readback, and an extended MS/TP frame inspection without an allocator.

## Fixture capability checks

The C reader checks the service and object-type bit strings and each object's Property_List. The Rust worksheet reads every listed property in each fixture mode. These checks describe the reference device. A manufacturer’s product PICS and BTL listing require separate evidence. [Capture the worksheet](@/docs/pics.md)
