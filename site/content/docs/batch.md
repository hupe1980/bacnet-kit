+++
title = "Read several properties at once"
description = "Build a batch, read an array entry and see which properties succeeded."
weight = 2

[extra]
group = "Read and control"
terms = "RPM segmentation APDU multiple array"
+++

ReadPropertyMultiple (RPM) asks a device for several properties in one transaction. Each property has its own result. Try a batch with two successful reads and one missing property on loopback.

```sh
bacnet-kit batch-demo
```

```text
analog-input:1/present-value  21.5
analog-input:1/description  UNKNOWN_PROPERTY (class 2, code 32)
device:123/object-list/0  2
```

The temperature and object count are usable. This fixture has no Description on Analog Input 1, so the middle row carries its own BACnet error. The demo expects that outcome and exits successfully.

## Build a request

Start `bacnet-kit serve` in a separate terminal. Read the temperature and the first inventory entry with one request to Device 123.

```sh
bacnet-kit batch 127.0.0.1:47808 analog-input:1/present-value device:123/object-list/1
```

{% include "batch-builder.html" %}

Each argument names an object type, instance and property. The optional final number selects a BACnetARRAY index. Zero reads the length, one reads the first element, and omitting the index reads the whole property. Object types and property identifiers also accept decimal numbers, so `0:1/85` selects Analog Input 1, Present_Value.

## Read across a small APDU limit

An application protocol data unit (APDU) carries a service request or reply. If the commissioning sheet gives you the Device instance, pass it with `--device`. This reads Max_APDU_Length_Accepted and Segmentation_Supported. For a device that receives segments, it also checks Max_Segments_Accepted. Property references still name the objects you want to read.

```sh
bacnet-kit batch --device 123 127.0.0.1:47808 analog-input:1/units device:123/object-list/0
```

| Peer capability | What happens during a read |
| --- | --- |
| Transmit only | Keep the request within the peer’s APDU limit. The reply can arrive in segments. |
| Receive only | The client can segment an oversized request. Select properties whose reply fits one APDU on the peer. |
| Both | The request and reply can each span segments. |
| None | Keep both messages small. For a large array, read the length and individual entries. |

The BACnet/IP client can receive up to 16 segments with a maximum APDU of 1,476 bytes each. The largest assembled service body is 23,536 bytes. The client acknowledges one segment at a time and can resend an outstanding request segment twice. The CLI gives capability discovery and the subsequent batch up to three seconds each. The loopback fixture reports no segmentation. The independent tests exercise both one-way capabilities. [Interoperability results](@/docs/verification.md) · [ASHRAE segmentation procedures](https://bacnet.org/wp-content/uploads/sites/4/2023/01/135_2020_cf_20221130.pdf)

## Interpret the result

| Outcome | What the caller sees |
| --- | --- |
| Every property succeeds | One row per argument, in order. Repeated arguments produce repeated rows. The command exits successfully. |
| A property fails | All rows print, followed by a failure count on standard error and a nonzero exit code. |
| The entire transaction fails | Timeout, BACnet Error, Reject or Abort, with no property rows. For a size-related Abort, try fewer references or select individual array elements. |

The client accepts 1–32 explicit references to concrete objects at one IPv4 endpoint. It allows three seconds for the CLI transaction. A batch reduces round trips. Values in the same batch can have different sample times. The CLI displays unrecognized or composite encodings as tagged bytes. [ASHRAE ReadPropertyMultiple service procedure](https://bacnet.org/wp-content/uploads/sites/4/2022/08/Add-2001-135a.pdf)

The embedded responder also accepts ALL, REQUIRED and OPTIONAL. It expands the fixture's supported properties within a budget of 32 results, charging one for an empty selection. An object with no optional properties returns an empty result list for OPTIONAL. [ASHRAE empty property-list example](https://bacnet.org/wp-content/uploads/sites/4/2022/08/Add-135-2016bl.pdf)
