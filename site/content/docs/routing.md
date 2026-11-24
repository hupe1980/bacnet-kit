+++
title = "Commission across networks"
description = "Read a known station through a router or receive forwarded device announcements through a BBMD."
weight = 5

[extra]
group = "Network and security"
terms = "router BBMD foreign device broadcast"
+++

A BACnet router delivers a directed request to a station on another BACnet network. A BACnet broadcast management device (BBMD) forwards broadcasts to a registered workstation on another IP subnet. One installation can use both roles.

## Read through a router

Ask for the BACnet network number, the station's link-layer address and the router's IPv4 endpoint. In this example the operator wants Analog Input 1 at station 42 on network 200. Substitute your commissioned values.

```sh
bacnet-kit read 200:0x2a@127.0.0.1:47808 1
```

<dl class="route-parts">
<div><dt>200</dt><dd>The remote BACnet network number.</dd></div>
<div><dt>0x2a</dt><dd>The station address on that network. Decimal 42 names the same one-octet station.</dd></div>
<div><dt>127.0.0.1:47808</dt><dd>The immediate UDP endpoint of the BACnet router.</dd></div>
</dl>

Use network numbers 1–65534. For a multibyte MAC, use two hexadecimal digits per octet and keep leading zeros. The reader accepts one to seven octets. The same target syntax works with `inspect`, `batch` and `watch`. For a known Device instance, use `inspect TARGET --device 77` to verify its identity and enumerate its objects. This also works with peers that reject the wildcard Device read. A directly connected device takes its usual `IP:PORT` target.

## Listen through a BBMD

Use the configured BBMD endpoint. This example listens for 35 seconds and requests a 20-second lease. Renewals start halfway through that lease.

```sh
bacnet-kit foreign 127.0.0.1:47808 --ttl 20 --seconds 35
```

`Registered` means the BBMD acknowledged registration. A quiet workstation can stay registered while no devices announce themselves. Each delivered I-Am identifies the Device and the originating IPv4 endpoint carried inside the forwarded frame. If the NPDU also carries a routed source, the CLI prints its network and MAC as `station`. The first output line identifies the BBMD that delivered the datagram.

<details class="watch-case">
<summary>What happens if the BBMD refuses a renewal?</summary>
<p>The CLI prints the refusal code and retries. A previously acknowledged lease stays valid until its local expiry. After <code>Expired</code>, announcement delivery resumes only when registration succeeds again. The command exits with failure if its observation window ends without a live lease. Closing the listener leaves the BBMD to age out its entry.</p>
</details>

<details class="watch-case">
<summary>Why does a directed read time out while announcements arrive?</summary>
<p>Check the configured router, destination network and station MAC separately. Forwarded announcements establish that the BBMD can reach the workstation. A directed read also needs a return path from the selected station through the configured router.</p>
</details>

Use authorized, trusted networks for commissioning. Plain BACnet/IP carries point data and addresses in cleartext. The listener makes no discovery broadcast and stores no device directory. [BACnet Committee foreign devices](https://bacnet.org/foreign-devices/) · [BACpypes3 route-aware addresses](https://bacpypes3.readthedocs.io/en/latest/gettingstarted/addresses.html)

## Try the independent fixture

```sh
cargo build --locked
uv run scripts/check-routing.py
```

The fixture provides both endpoints and drives the temperature read, registration, expiry and recovery. [Two-subnet test and results](@/docs/verification.md#routed-read-and-foreign-registration)
