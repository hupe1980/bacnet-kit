+++
title = "Build a capability worksheet"
description = "Capture the reference device’s live properties and trace candidate PICS claims to independent tests."
weight = 9

[extra]
group = "Development and verification"
terms = "PICS BIBB conformance worksheet"
+++

A Protocol Implementation Conformance Statement (PICS) describes a product’s BACnet capabilities. Run `pics` to capture the capabilities of a fresh reference device. Each command starts its own fixture on a private loopback port, reads it, and exits. Save both worksheets to review what enabling commands changes.

## Capture a configuration

```sh
bacnet-kit pics > read-only.md
bacnet-kit pics --commands > supervised.md
```

A successful command writes a complete Markdown worksheet. On a mismatch it exits with an error and identifies the failed check on stderr. Use a new filename for each capture you want to keep. Shell redirection opens the file before the command runs.

| Configuration | Objects observed | Executable services |
| --- | --- | --- |
| Read-only | Device 123 and Analog Input 1 | ReadProperty, ReadPropertyMultiple, Who-Is |
| Supervised commands | Also Analog Value 1 | Also WriteProperty |

Both captures use reads only. To exercise the supervised write policy and observe a relinquishment, run the separate command demonstration. [Supervised commands](@/docs/commands.md)

## Read the evidence

The worksheet compares the actual object inventory with the selected configuration. For each object it checks the whole `Property_List`, the length at index zero and each one-based member. It then reads all listed properties and the four implicit properties using ReadPropertyMultiple. The independent C peer checks these arrays and rejects an index past the end. [Reproduce the peer checks](@/docs/verification.md#fixture-capability-checks)

<details class="watch-case"><summary>Why does the device answer Who-Is while its I-Am bit is clear?</summary><p>The bit string describes services the device executes. The fixture executes Who-Is and initiates I-Am in response. The worksheet records both directions in its device-binding row.</p></details>

<details class="watch-case"><summary>What do 50 service bits and 65 object-type bits establish?</summary><p>Those are the transmitted widths. This fixture uses the Protocol Revision 30 vocabulary from the 2024 edition. A separate read of Protocol_Revision returns Unknown_Property. On a real device, a supported type can have zero installed instances. Use Object_List to discover the instances actually present.</p></details>

## Prepare a product PICS

Record the shipping vendor identity, model, firmware, claimed revision and configured data link. Review each candidate BACnet Interoperability Building Block (BIBB) against Annex K, select the applicable Annex L profile and check its required objects and properties, including Network Port where applicable. The worksheet records fixture observations and points to the behavior tests. The manufacturer issues the product’s Protocol Implementation Conformance Statement using Annex A. [BACnet Committee PICS guidance](https://bacnet.org/conformance-pics/) · [Device object guide](https://thebacnetinstitute.org/wp-content/uploads/sites/5/2026/01/Guide-to-the-Device-Object.pdf)
