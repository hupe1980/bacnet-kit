# /// script
# requires-python = ">=3.11"
# dependencies = ["bacpypes3==0.1.0"]
# ///
"""Exercise the Rust subscriber against independent BACpypes3 object COV publishers."""

import asyncio
from pathlib import Path

from bacpypes3.apdu import SimpleAckPDU
from bacpypes3.app import Application
from bacpypes3.local.analog import AnalogInputObject
from bacpypes3.local.device import DeviceObject
from bacpypes3.local.networkport import NetworkPortObject

KIT = Path(__file__).resolve().parents[1] / "target/debug/bacnet-kit"


class Publisher(Application):
    async def do_SubscribeCOVRequest(self, apdu):
        self.controls.append(
            (
                str(apdu.pduSource),
                int(apdu.subscriberProcessIdentifier),
                apdu.issueConfirmedNotifications,
                apdu.lifetime,
            )
        )
        await super().do_SubscribeCOVRequest(apdu)

    def cov_confirmation(self, cov, future):
        self.acks.append(future.result())
        super().cov_confirmation(cov, future)


async def exercise(confirmed):
    point = AnalogInputObject(
        objectIdentifier=("analog-input", 1),
        objectName="Temperature",
        presentValue=20.0,
        units="degrees-celsius",
        covIncrement=0.1,
        statusFlags=[0, 0, 0, 0],
        outOfService=False,
    )
    device = DeviceObject(objectIdentifier=("device", 77), objectName="Publisher")
    port = NetworkPortObject(
        "127.0.0.1:0", objectIdentifier=("network-port", 1), objectName="Loopback"
    )
    app = Publisher.from_object_list([device, port, point])
    app.controls = []
    app.acks = []
    proc = None
    try:
        server = app.link_layers[port.objectIdentifier].server
        await asyncio.wait_for(server._local_transport_ready.wait(), 4)
        number = server.local_transport.get_extra_info("sockname")[1]
        proc = await asyncio.create_subprocess_exec(
            KIT,
            "watch",
            f"127.0.0.1:{number}",
            "77",
            "1",
            "--seconds",
            "4",
            "--max-age",
            "1",
            "--lease",
            "4",
            *(["--confirmed"] if confirmed else []),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        lines = []
        step = 0
        lost_at = 0
        while line := await asyncio.wait_for(proc.stdout.readline(), 8):
            line = line.decode().strip()
            lines.append(line)
            fields = line.split()
            if step == 0 and fields[:2] == ["Notification", "20"]:
                point.presentValue = 22.0
                step = 1
            elif step == 1 and fields[:2] == ["Notification", "22"]:
                # Lose the publisher's volatile contexts as on a silent restart.
                detection = app._cov_detections[point.objectIdentifier]
                for subscription in list(detection.cov_subscriptions):
                    app.cancel_subscription(subscription)
                lost_at = asyncio.get_running_loop().time()
                point.presentValue = 24.5
                step = 2
            elif (
                step == 2
                and fields[:2] == ["Verification", "24.5"]
                and "history gap" in line
            ):
                elapsed = asyncio.get_running_loop().time() - lost_at
                if elapsed >= 1:
                    raise RuntimeError(f"reconciliation exceeded 1 s: {elapsed}")
                step = 3
            elif step == 3 and fields[:2] == ["Notification", "24.5"]:
                point.presentValue = 25.0
                step = 4
            elif step == 4 and fields[:2] == ["Notification", "25"]:
                step = 5
        _, stderr = await proc.communicate()
        if proc.returncode or step != 5:
            raise RuntimeError(
                f"exit {proc.returncode}, stage {step}: {lines}, {stderr!r}"
            )
        if len(app.controls) < 3 or len({c[:2] for c in app.controls}) != 1:
            raise RuntimeError(f"missing stable renewal: {app.controls}")
        if (
            not all(c[2:] == (confirmed, 4) for c in app.controls[:-1])
            or app.controls[-1][2:] != (None, None)
            or app._cov_detections
        ):
            raise RuntimeError(f"incorrect lease or cancellation: {app.controls}")
        if confirmed and (
            len(app.acks) < 4
            or not all(isinstance(ack, SimpleAckPDU) for ack in app.acks)
        ):
            raise RuntimeError(f"missing confirmed ACKs: {app.acks}")
        print(
            f"{'confirmed' if confirmed else 'unconfirmed'}: initial, change, silent lease loss, verified recovery in {elapsed:.3f} s, renewal and cancellation"
        )
    finally:
        if proc and proc.returncode is None:
            proc.kill()
            await proc.wait()
        app.close()


async def main():
    await asyncio.gather(exercise(False), exercise(True))


if __name__ == "__main__":
    asyncio.run(main())
