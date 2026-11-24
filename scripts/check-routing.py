# /// script
# requires-python = ">=3.11"
# dependencies = ["bacpypes3==0.1.0"]
# ///
"""Read through a BACpypes3 router and receive its BBMD's forwarded field broadcasts."""

import argparse
import asyncio
import ipaddress
import socket
from pathlib import Path

from bacpypes3.app import Application
from bacpypes3.local.analog import AnalogInputObject
from bacpypes3.local.device import DeviceObject
from bacpypes3.local.networkport import NetworkPortObject
from bacpypes3.vlan import VirtualNetwork

KIT = Path(__file__).resolve().parents[1] / "target/debug/bacnet-kit"


def device(instance, name):
    return DeviceObject(objectIdentifier=("device", instance), objectName=name)


def field_port(instance, mac):
    return NetworkPortObject(
        objectIdentifier=("network-port", instance),
        objectName="Field port",
        networkType="virtual",
        protocolLevel="bacnet-application",
        networkInterfaceName="Field",
        networkNumber=200,
        macAddress=bytes([mac]),
    )


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--uplink", default="127.0.0.1/32", type=ipaddress.IPv4Interface
    )
    parser.add_argument(
        "--client-netns", help="run the Rust CLI inside this Linux network namespace"
    )
    opts = parser.parse_args()
    cli = (
        ["ip", "netns", "exec", opts.client_netns, KIT] if opts.client_netns else [KIT]
    )
    host = str(opts.uplink.ip)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind((host, 0))
        port = s.getsockname()[1]
    endpoint = f"{host}:{port}"
    uplink = NetworkPortObject(
        f"{opts.uplink}:{port}",
        objectIdentifier=("network-port", 1),
        objectName="Commissioning",
        networkNumber=100,
        bacnetIPMode="bbmd",
        bbmdBroadcastDistributionTable=[],
    )
    VirtualNetwork("Field")
    router = Application.from_object_list(
        [device(19, "Router"), uplink, field_port(2, 1)]
    )
    sensor = AnalogInputObject(
        objectIdentifier=("analog-input", 1),
        objectName="Temperature",
        presentValue=23.5,
        units="degrees-celsius",
        statusFlags=[0, 0, 0, 0],
        outOfService=False,
        covIncrement=0.1,
    )
    field = Application.from_object_list(
        [device(77, "Field device"), field_port(1, 42), sensor]
    )
    sink = None
    listener = None
    try:
        bbmd = router.link_layers[uplink.objectIdentifier]
        await asyncio.wait_for(bbmd.server._local_transport_ready.wait(), 4)
        if opts.uplink.network.prefixlen == 32:
            # A /32 has no subnet broadcast socket. Deliver the local broadcast copy
            # to an isolated sink; the BBMD sends its separate FDT copies over UDP.
            sink = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sink.bind((host, 0))
            bbmd.server.broadcast_address = sink.getsockname()
        target = f"200:0x2a@{endpoint}"

        async def check_command(args, expected):
            proc = await asyncio.create_subprocess_exec(
                *cli,
                *args,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
            )
            try:
                output, _ = await asyncio.wait_for(proc.communicate(), 8)
            finally:
                if proc.returncode is None:
                    proc.kill()
                    await proc.wait()
            text = output.decode()
            if proc.returncode or not all(value in text for value in expected):
                raise RuntimeError(f"{args[0]}: {text}")
            print(text.strip(), flush=True)

        await check_command(["read", target, "1"], ["23.5 °C", "normal"])
        await check_command(
            ["inspect", target, "--device", "77"], ["Device 77", "ANALOG_INPUT 1"]
        )
        await check_command(
            [
                "batch",
                target,
                "analog-input:1/present-value",
                "device:77/object-list/0",
            ],
            ["analog-input:1/present-value  23.5", "device:77/object-list/0  3"],
        )
        await check_command(
            [
                "watch",
                target,
                "77",
                "1",
                "--confirmed",
                "--seconds",
                "1",
                "--lease",
                "4",
            ],
            ["Subscribed    4 s lease", "Notification  23.5 °C"],
        )
        if field._cov_detections:
            raise RuntimeError("routed cancellation left a live COV subscription")
        listener = await asyncio.create_subprocess_exec(
            *cli,
            "foreign",
            endpoint,
            "--ttl",
            "2",
            "--seconds",
            "7",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        register = bbmd.register_foreign_device
        first = True
        expired = False
        reports = 0
        renewed = False
        lease_address = None
        while raw := await asyncio.wait_for(listener.stdout.readline(), 10):
            line = raw.decode().strip()
            print(line, flush=True)
            if line == "Registered":
                rows = bbmd.bbmdFDT
                if len(rows) != 1 or rows[0].fdTTL != 2:
                    raise RuntimeError("the foreign registration is missing")
                addr = rows[0].fdAddress
                if (
                    opts.client_netns
                    and ipaddress.IPv4Address(addr.addrTuple[0]) in opts.uplink.network
                ):
                    raise RuntimeError(f"workstation {addr} is on the BBMD subnet")
                if lease_address is not None and addr != lease_address:
                    raise RuntimeError("listener moved during renewal")
                lease_address = addr
                field.i_am()
            elif line.startswith("Device "):
                if line != f"Device 77  ·  origin {endpoint}  ·  station 200:0x2a":
                    raise RuntimeError(f"unexpected announcement {line!r}")
                reports += 1
                if first:
                    first = False
                    bbmd.delete_foreign_device_table_entry(lease_address)
                    bbmd.register_foreign_device = lambda addr, ttl: 0x0030
            elif line == "Expired":
                expired = True
                bbmd.register_foreign_device = register
            elif line == "Renewed" and expired:
                renewed = True
        _, stderr = await listener.communicate()
        if listener.returncode or not (reports >= 2 and expired and renewed):
            raise RuntimeError(
                f"registration recovery: reports={reports} expired={expired} renewed={renewed} {stderr!r}"
            )
    finally:
        if listener and listener.returncode is None:
            listener.kill()
            await listener.wait()
        if sink:
            sink.close()
        field.close()
        router.close()


if __name__ == "__main__":
    asyncio.run(main())
