+++
title = "Read through a Secure Connect hub"
description = "Read one property from a commissioned VMAC and see how the primary and backup hubs handled the request."
weight = 7

[extra]
group = "Network and security"
terms = "BACnet/SC TLS VMAC failover hub"
+++

BACnet Secure Connect (SC) carries BACnet traffic over mutually authenticated WebSockets. Use `sc-read` when you know the node's six-byte virtual MAC address (VMAC) and the property you want. The command opens a BACnet/SC connection, performs one ReadProperty transaction and closes the link. A successful result names the hub that delivered the reading.

## Prepare the node

Obtain the primary and optional failover `wss` URIs, the target VMAC, and approved direct signing certificates from the site administrator. Assign the commissioning node a UUID once and retain it across runs. The kit generates a local Random-48 VMAC and keeps it across both hub attempts. Keep the operational key on the commissioning host. [Stage and verify credentials](@/docs/sc-credentials.md)

## Read a property

Replace the example hub addresses, target VMAC, node UUID and file paths with the commissioned values. The target in this example is Analog Input 1 on station `020000001234`.

```sh
bacnet-kit sc-read \
  wss://primary.example/sc 020000001234 analog-input:1/present-value \
  --failover wss://backup.example/sc \
  --uuid b530755d-3ba0-4cd4-ba49-b56347009157 \
  --cert node.pem --key node.key --issuers site.pem
```

Supply `--crl node-issuer.crl.pem` and `--peer-crl hub-issuer.crl.pem` when the corresponding revocation information is available. Each supplied CRL must be current, applicable and signed. Local CRLs are the supported revocation source. Each connection requires TLS 1.3, an authenticated hub with a direct site signer, a request for the node certificate, the hub WebSocket subprotocol and an SC Connect-Accept. The hub vouches for the source VMAC on a relayed reading. [ASHRAE connection security](https://www.ashrae.org/file%20library/technical%20resources/standards%20and%20guidelines/standards%20addenda/135_2020_cp_20241129.pdf)

The verified C fixture returns the following after its primary stops.

```text
Primary attempt  TCP connection failed
Hub              failover
analog-input:1/present-value  0
```

Each endpoint gets at most five seconds. A link failure can trigger one new read through the configured backup. A correlated BACnet Error, Reject or Abort completes the transaction and is returned immediately. The command supports unsegmented, local-link properties, including explicit array indices. Read `units` and `status-flags` when interpreting an Analog Input value.

## Interpret a failure

<details class="watch-case"><summary>TCP connection or TLS authentication failed</summary><p>Check the configured listener, the workstation clock, both operational identities and their direct signing certificates. The hub must request the client certificate. The same trust checks run on a backup connection.</p></details>

<details class="watch-case"><summary>WebSocket upgrade or Connect-Accept failed</summary><p>Check the hub URI path and its hub subprotocol. Confirm the node UUID is unique and inspect the hub's admission diagnostics. A successful TLS connection alone leaves the BACnet link unavailable.</p></details>

<details class="watch-case"><summary>The property read was outstanding</summary><p>The node reached the SC read stage and did not obtain a correlated result. Check the commissioned VMAC and the field node's current hub. On failover the kit starts a fresh read and reports the primary outcome alongside the successful value.</p></details>

To reproduce both paths without site equipment, use the local fixture with two C hubs and one field device. [Independent primary and failover trial](@/docs/verification.md#live-secure-connect-hubs)
