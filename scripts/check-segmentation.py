# /// script
# requires-python = ">=3.11"
# dependencies = ["bacpypes3==0.1.0"]
# ///
"""Exercise both asymmetric BACpypes3 server profiles on private loopback ports."""

import asyncio
from pathlib import Path

from bacpypes3.apdu import ComplexAckPDU, SegmentAckPDU
from bacpypes3.app import Application, DeviceInfo, DeviceInfoCache
from bacpypes3.basetypes import Segmentation
from bacpypes3.local.device import DeviceObject
from bacpypes3.local.networkport import NetworkPortObject

KIT = Path(__file__).resolve().parents[1] / "target/debug/bacnet-kit"


class LoopbackCache(DeviceInfoCache):
    async def get_device_info(self, addr):
        # A constrained path makes the independent transmitter use short segments.
        return DeviceInfo(
            device_instance=1,
            device_address=addr,
            max_apdu_length_accepted=1476,
            segmentation_supported=Segmentation.segmentedReceive,
            max_npdu_length=256,
        )


async def exercise(transmit):
    direction = "segmented-transmit" if transmit else "segmented-receive"
    description = "Temperature history. " * 80
    device = DeviceObject(
        objectIdentifier=("device", 77),
        objectName="Segment fixture",
        description=description,
        segmentationSupported=direction,
        maxApduLengthAccepted=50,
        maxSegmentsAccepted=16,
    )
    port = NetworkPortObject(
        "127.0.0.1:0", objectIdentifier=("network-port", 1), objectName="Loopback"
    )
    app = Application.from_object_list(
        [device, port], device_info_cache=LoopbackCache() if transmit else None
    )
    sent = []
    original = app.asap.request

    async def record(apdu):
        if (transmit and isinstance(apdu, ComplexAckPDU) and apdu.apduSeg) or (
            not transmit and isinstance(apdu, SegmentAckPDU)
        ):
            sent.append(apdu.apduSeq)
        await original(apdu)

    app.asap.request = record
    try:
        server = app.link_layers[port.objectIdentifier].server
        await asyncio.wait_for(server._local_transport_ready.wait(), timeout=4)
        _, number = server.local_transport.get_extra_info("sockname")
        refs = (
            ["device:77/description"]
            if transmit
            else ["device:77/vendor-identifier"] * 24
        )
        proc = await asyncio.create_subprocess_exec(
            KIT,
            "batch",
            "--device",
            "77",
            f"127.0.0.1:{number}",
            *refs,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=10)
        if proc.returncode:
            raise RuntimeError(stderr.decode())
        expected = (
            [f'{refs[0]}  "{description}"'] if transmit else [f"{r}  999" for r in refs]
        )
        if stdout.decode().splitlines() != expected:
            raise ValueError(f"{direction}: unexpected property rows: {stdout!r}")
        if len(sent) < 2 or sent != list(range(len(sent))):
            raise ValueError(f"{direction}: unexpected segment sequence {sent}")
        print(f"{direction}: {len(sent)} segments, {len(refs)} correlated rows")
    finally:
        app.close()


async def main():
    await exercise(True)
    await exercise(False)


if __name__ == "__main__":
    asyncio.run(main())
