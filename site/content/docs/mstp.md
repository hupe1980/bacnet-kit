+++
title = "Inspect an MS/TP frame"
description = "Read the station addresses and check the integrity of one captured serial frame offline."
weight = 10

[extra]
group = "Network and security"
terms = "MS/TP MSTP serial RS-485 CRC capture"
+++

BACnet MS/TP carries frames between stations on a shared RS-485 serial link. Copy one complete MS/TP frame from a capture, starting with the `55 FF` preamble. The inspector validates the header, verifies the data checksum when present, and checks the record boundary before printing the frame type, addresses and lengths.

## Try a captured token

```sh
bacnet-kit mstp-frame '55 ff 00 01 02 00 00 40'
```

```text
Frame        TOKEN (0)
Source       2
Destination  1
Data octets  0
Wire octets  8
Pad          none
```

This token was captured from station 2 to station 1. In capture order the destination comes first. Uppercase, lowercase and contiguous hex all work. [ABB capture example](https://library.e.abb.com/public/2b037b07870b4af3831e88afe0bcf1a2/LVD-EOTKN123U-EN_WiresharkforBACnetMSTPtrafficcapture_REVA.pdf)

| Bytes | Field | Reading |
| --- | --- | --- |
| `55 FF` | Preamble | Start of the frame |
| `00` | Type | Token |
| `01 02` | Destination, source | Station 1 receives from station 2 |
| `00 00` | Header Length | Zero data octets |
| `40` | Header CRC | Matches the preceding five header octets |

## Read a binary record

```sh
bacnet-kit mstp-frame --raw < capture.bin
```

Use this form for exported packet bytes. In Wireshark, select a single MS/TP packet and export its packet bytes, including the link header. A whole pcap or pcapng file also contains capture headers. The inspector accepts one record of up to 1,517 octets, including an optional final `FF`. Binary input ends at EOF. The output contains metadata only.

## Interpret a result

**Data octets** counts the decoded data. **Wire octets** counts everything supplied, including the preamble, checksums and optional pad. In the example above the inspector reports 502 data octets and 517 wire octets. Wireshark’s header Length is 507. For extended frames that field counts encoded data and encoded CRC minus two. [ASHRAE frame format and encoding](https://bacnet.org/wp-content/uploads/sites/4/2022/08/Add-135-2012-an-at-au-av-aw-ax-az-Final-2.pdf)

<details class="watch-case"><summary>The header or data CRC failed</summary><p>Check the selected record begins at the preamble and includes its final integrity field. Export the original bytes again. A single capture cannot distinguish an export error from corruption on the wire.</p></details>

<details class="watch-case"><summary>The record contains trailing bytes</summary><p>Select exactly one frame. A single FF pad is accepted after a complete frame. A second pad or a second packet causes an error.</p></details>

<details class="watch-case"><summary>The frame type is unsupported</summary><p>The inspector handles standard BACnet types 0 through 7 and extended data types 32 and 33. Other protocols and proprietary types need their own interpretation.</p></details>

A successful result describes the selected link frame. Token circulation, inter-octet timing and driver turnaround need timestamped traces from the target board. [Independent vectors and embedded verification](@/docs/verification.md#offline-ms-tp-frames)
