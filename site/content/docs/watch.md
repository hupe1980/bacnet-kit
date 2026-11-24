+++
title = "Watch an Analog Input"
description = "Follow a changing point, recognize a missed update and see when the last good reading arrived."
weight = 3

[extra]
group = "Read and control"
terms = "COV change of value subscribe freshness"
+++

A watch starts with a verified reading from a known Device and Analog Input. On a publisher that supports change of value (COV), a change can wake the reader immediately. A timer also checks the point so a lost notification cannot leave an old temperature looking fresh.

## Try it on loopback

Start `bacnet-kit serve` in one terminal. In another, watch Device 123, Analog Input 1 for five seconds with a two-second freshness budget.

```sh
bacnet-kit watch 127.0.0.1:47808 123 1 --seconds 5 --max-age 2
```

The local fixture serves reads and rejects SubscribeCOV. Expect a `Baseline` of 21.5 °C, a `COV unavailable` message and periodic `Verification` readings. Start the fixture with `--fault` to see the same value with a fault flag.

## Watch a publisher

BACnet/IP carries point values and status in cleartext. Use a trusted link and an operator terminal authorized to display the point. Use the commissioned endpoint and instances in place of these loopback example values. This watch requests confirmed notifications and runs long enough to see a renewal.

```sh
bacnet-kit watch 127.0.0.1:47808 77 1 --confirmed --lease 60 --max-age 10 --seconds 35
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--seconds` | 15 | Observation window after the baseline, followed by explicit cancellation. |
| `--lease` | 60 | Lifetime in seconds. Renewal starts halfway through. An abandoned lease expires at the publisher. |
| `--max-age` | 10 | Freshness budget in seconds. The next check starts early enough to allow time for a readback. |
| `--confirmed` | off | Acknowledge each matching confirmed notification. Both modes use the same verification reads. |

The host subscriber accepts unsegmented object notifications for one Analog Input over directed BACnet/IPv4. Each observation checks Device.Object_Identifier and reads the input's Present_Value, Units and Status_Flags. The timestamp belongs to the value read. These are successive reads, so a rapidly changing controller can update between properties. Multiple notifications can share a readback.

## Interpret the output

These selected lines show a publisher whose subscription table was cleared while the point changed.

```text
Baseline      20 °C  normal
Subscribed    4 s lease
Notification  22 °C  normal
Verification  24.5 °C  normal  · history gap
Subscribed    4 s lease
Notification  25 °C  normal
```

<details class="watch-case">
<summary>What does “history gap” tell me?</summary>
<p>The current reading succeeded. The timer found a change without a matching notification, the previous observation had expired, or a lease or readback had failed. Intermediate changes may be missing. Renewal can restore push delivery after a publisher restart.</p>
</details>

<details class="watch-case">
<summary>What happens if a readback fails?</summary>
<p>The watcher reports the error and the age of the last complete reading. Its timestamp stays fixed until all four reads succeed. The CLI returns a failure status if it finishes with a failed or stale reading, or if explicit cancellation fails.</p>
</details>

To exercise push delivery and silent subscription loss on your own machine, run `cargo build --locked` followed by `uv run scripts/check-cov.py`. The test starts its own publishers on private loopback ports. [Independent verification](@/docs/verification.md)
