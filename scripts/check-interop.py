"""Exercise both BACnet/IP roles on loopback with the pinned bacnet-stack examples."""

import argparse
import os
import socket
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from functools import partial
from pathlib import Path


def port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def env_for(port_number):
    env = {k: v for k, v in os.environ.items() if not k.startswith("BACNET_")}
    env.update(
        BACNET_DATALINK="bip",
        BACNET_IFACE="lo0" if sys.platform == "darwin" else "lo",
        BACNET_IP_PORT=str(port_number),
    )
    return env


@contextmanager
def server(args, cwd, env):
    proc = subprocess.Popen(
        args, cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    try:
        yield proc
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def read(args, cwd, env=None, expected=0):
    result = subprocess.run(
        args,
        cwd=cwd,
        env=env,
        text=True,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=10,
    )
    if result.returncode != expected:
        raise RuntimeError(f"{args} exited {result.returncode}: {result.stdout}")
    return result.stdout.strip()


def require(text, expected):
    if expected not in text:
        raise RuntimeError(f"expected {expected!r} in {text!r}")


def ready(proc, op):
    deadline = time.monotonic() + 12
    while True:
        if proc.poll() is not None:
            raise RuntimeError(f"server exited {proc.returncode}")
        try:
            return op()
        except (RuntimeError, subprocess.TimeoutExpired):
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.05)


def check_capabilities(c_read, c_call, commands):
    # 009-capability-evidence/SR-001
    services = {12, 14, 34} | ({15} if commands else set())
    types = {0, 8} | ({2} if commands else set())
    for prop, width, active in [(97, 50, services), (96, 65, types)]:
        expected = ["true" if i in active else "false" for i in range(width)]
        if c_read(f"8 123 {prop}") != "{" + ",".join(expected) + "}":
            raise RuntimeError(f"incorrect width or positions in property {prop}")

    point = ["present-value", "units", "status-flags"]
    objects = {
        "8 123": [
            "object-list",
            "vendor-identifier",
            "max-apdu-length-accepted",
            "segmentation-supported",
            "protocol-services-supported",
            "protocol-object-types-supported",
        ],
        "0 1": point,
    }
    if commands:
        objects["2 1"] = point + ["priority-array", "relinquish-default"]
    for obj, members in objects.items():
        prop = f"{obj} 371"
        raw = c_read(prop)
        if not (raw.startswith("{") and raw.endswith("}")):
            raise RuntimeError(f"{prop}: expected an array, got {raw!r}")
        actual = raw[1:-1].split(",")
        if sorted(actual) != sorted(members) or c_read(f"{prop} 0") != str(
            len(members)
        ):
            raise RuntimeError(f"{prop}: inconsistent property list {actual!r}")
        for index, name in enumerate(actual, 1):
            if c_read(f"{prop} {index}") != name:
                raise RuntimeError(f"{prop}: incorrect member at index {index}")
        require(
            c_call("bacrp", f"{prop} {len(actual) + 1}", expected=1).lower(),
            "invalid-array-index",
        )
        require(c_call("bacrpm", f"{obj} 371[0]"), f"property-list: [0]{len(actual)}")
    print(f"Verified service/type widths and Property_List for {len(objects)} objects")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "build",
        type=Path,
        help="CMake directory containing bacrp, bacrpm, bacwp and bacserv",
    )
    opts = parser.parse_args()
    build = opts.build.resolve()
    kit = Path(__file__).resolve().parents[1] / "target/debug/bacnet-kit"
    with tempfile.TemporaryDirectory() as work:
        number = port()
        endpoint = f"127.0.0.1:{number}"
        with server([build / "bacserv", "4200"], work, env_for(number)) as proc:
            inventory = ready(proc, lambda: read([kit, "inspect", endpoint], work))
            require(inventory, "Device 4200   ·   71 objects")
            sample = read([kit, "read", endpoint, "1"], work)
            require(sample, "Analog Input 1   0 percent")
            require(sample, "normal")
            require(
                read([kit, "read", endpoint, "4194000"], work, expected=1),
                "class 1, code 31",
            )
            batch = read(
                [
                    kit,
                    "batch",
                    endpoint,
                    "analog-input:1/present-value",
                    "analog-input:1/4194000",
                    "device:4200/object-list/0",
                    "device:4200/object-list/1",
                ],
                work,
                expected=1,
            )
            expected_batch = [
                "analog-input:1/present-value  0",
                "analog-input:1/4194000  UNKNOWN_PROPERTY (class 2, code 32)",
                "device:4200/object-list/0  71",
                "device:4200/object-list/1  DEVICE:4200",
                "bacnet-kit: 1 of 4 property reads failed",
            ]
            if batch.splitlines() != expected_batch:
                raise RuntimeError(f"unexpected batch results: {batch}")
            print(inventory.splitlines()[0], sample, batch, sep="\n")
        local = port()
        while local == number:
            local = port()

        def c_call(tool, fields, expected=0):
            return read(
                [
                    build / tool,
                    "--mac",
                    endpoint,
                    "--dnet",
                    "0",
                    "123",
                    *fields.split(),
                ],
                work,
                env_for(local),
                expected,
            )

        def c_read(fields):
            return c_call("bacrp", fields)

        with server([kit, "serve", endpoint], work, env_for(number)) as proc:
            for fields, expected in [
                ("8 4194303 75", "(device, 123)"),
                ("8 4194303 76 0", "2"),
                ("8 123 76 0", "2"),
                ("8 123 76 1", "(device, 123)"),
                ("8 123 76 2", "(analog-input, 1)"),
                ("0 1 85", "21.500000"),
            ]:
                actual = ready(proc, partial(c_read, fields))
                if actual != expected:
                    raise RuntimeError(
                        f"{fields}: expected {expected!r}, got {actual!r}"
                    )
                print(f"bacrp {fields} -> {actual}")

            check_capabilities(c_read, c_call, commands=False)
            mixed = c_call("bacrpm", "0 1 85,28,85 8 123 76[0]")
            rows = [line.strip() for line in mixed.splitlines() if ":" in line]
            if rows != [
                "present-value: 21.500000",
                "description: BACnet Error: property: unknown-property",
                "present-value: 21.500000",
                "object-list: [0]2",
            ]:
                raise RuntimeError(f"independent RPM decoder returned {rows!r}")
            print(mixed)
            alias = c_call("bacrpm", "8 4194303 75,76[0]")
            require(alias, "object-identifier: (device, 123)")
            require(alias, "object-list: [0]2")

        with server(
            [kit, "serve", endpoint, "--commands"], work, env_for(number)
        ) as proc:
            inventory = ready(proc, lambda: read([kit, "inspect", endpoint], work))
            require(inventory, "3 objects")

            def check_value(fields, expected):
                actual = c_read(fields)
                if actual != expected:
                    raise RuntimeError(
                        f"{fields}: expected {expected!r}, got {actual!r}"
                    )

            check_capabilities(c_read, c_call, commands=True)
            check_value("8 123 76 3", "(analog-value, 1)")
            check_value("2 1 87 0", "16")
            check_value("2 1 104", "20.000000")
            optional = c_call("bacrpm", "2 1 80")
            require(optional, "priority-array:")
            require(optional, "relinquish-default: 20.000000")
            for priority, tag, value, slot, effective in [
                (0, 4, 19, "19.000000", "19.000000"),
                (8, 4, 22, "22.000000", "22.000000"),
                (16, 4, 18, "18.000000", "22.000000"),
                (8, 0, 0, "Null", "18.000000"),
                (16, 0, 0, "Null", "20.000000"),
            ]:
                require(
                    c_call("bacwp", f"2 1 85 {priority} -1 {tag} {value}"),
                    "Acknowledged",
                )
                check_value(f"2 1 87 {priority or 16}", slot)
                check_value("2 1 85", effective)
                print(f"priority {priority or 16}: slot {slot}, effective {effective}")
            denied = c_call("bacwp", "2 1 85 7 -1 4 99", expected=1)
            require(denied.lower(), "write-access-denied")
            check_value("2 1 87 7", "Null")
            check_value("2 1 85", "20.000000")


if __name__ == "__main__":
    main()
