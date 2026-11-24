+++
title = "Command and relinquish a setpoint"
description = "See which priority wins, inspect a masked command and recover the default."
weight = 4

[extra]
group = "Read and control"
terms = "priority array write relinquish setpoint"
+++

A commandable Analog Value keeps 16 priority slots. Its effective setpoint comes from the highest-priority occupied slot. Run the loopback demonstration to set and release two slots and read back the effective value after each write.

```sh
bacnet-kit command-demo
```

| Action | Priority | Slot after write | Effective setpoint |
| --- | --- | --- | --- |
| Set 19 °C | 16 | 19 °C | 19 °C |
| Set 22 °C | 8 | 22 °C | 22 °C |
| Set 18 °C | 16 | 18 °C | 22 °C |
| Relinquish | 8 | NULL | 18 °C |
| Relinquish | 16 | NULL | 20 °C |

A smaller priority number takes precedence. At the third step the fixture accepts 18 °C, but priority 8 still holds 22 °C. Releasing priority 8 exposes the stored 18 °C. When all 16 slots are empty, Present_Value comes from Relinquish_Default. [OPC Foundation BACnet command prioritization](https://reference.opcfoundation.org/specs/OPC-30030/3.2.1)

{% include "priority-lab.html" %}

## Connect an independent workstation

Run the fixture and direct the workstation to Device 123 at `127.0.0.1:47808`. Command mode adds Analog Value 1 to the object list. Its application policy accepts priorities 8 and 16 from the local host. Restarting the process clears both slots.

```sh
bacnet-kit serve --commands
```

Read Priority_Array at index 0 for its length, or at indices 1–16 for individual slots. Write a finite REAL to Present_Value to set a slot, or BACnet NULL to relinquish it. Omitting priority selects 16. Priority 6 is reserved in this fixture. Writes to the array itself are denied. The kit also serves Units, Status_Flags and Relinquish_Default. [ASHRAE commandable value-object properties](https://bacnet.org/wp-content/uploads/sites/4/2022/08/Add-135-2010af.pdf)

## If the acknowledgement is lost

Try `bacnet-kit command-demo --lose-ack`. The simulator commits the third command and drops its UDP acknowledgement. The demo prints the readback and records the missing ACK.

When a real acknowledgement times out, the write outcome is unknown. Read the requested priority slot and Present_Value before deciding what to do next. These reads describe the state at observation time. Another writer may have changed it since the original request. Plant feedback requires a separate measured point. The demonstration operates entirely on simulator memory.
