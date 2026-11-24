# /// script
# requires-python = ">=3.11"
# dependencies = ["cryptography==50.0.2", "websockets==15.0.1"]
# ///
"""Exercise sc-read on loopback with temporary identities and optional pinned C peers."""

import asyncio
import contextlib
import ipaddress
import logging
import os
import socket
import ssl
import struct
import subprocess
import tempfile
import unittest
import uuid
from datetime import UTC, datetime, timedelta
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import NameOID
from websockets.asyncio.client import connect
from websockets.asyncio.server import serve
from websockets.exceptions import ConnectionClosed

KIT = Path(__file__).resolve().parents[1] / "target/debug/bacnet-kit"
HUB_PROTOCOL = "hub.bsc.bacnet.org"
FIELD = bytes.fromhex("020000001234")
HUB = bytes.fromhex("020000000001")
POINT = "analog-input:1/present-value"
PEM = serialization.Encoding.PEM


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def uri(p):
    return f"wss://127.0.0.1:{p}/"


def issue(folder, label, issuer=None, with_ip=False):
    k = ec.generate_private_key(ec.SECP256R1())
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, label)])
    ca = issuer is None
    signer, authority = (k, subject) if ca else issuer
    now = datetime.now(UTC)
    builder = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(authority)
        .public_key(k.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(days=1))
        .not_valid_after(now + timedelta(days=7))
        .add_extension(x509.BasicConstraints(ca=ca, path_length=None), True)
        .add_extension(
            x509.KeyUsage(True, False, False, False, False, ca, ca, False, False), True
        )
        .add_extension(x509.SubjectKeyIdentifier.from_public_key(k.public_key()), False)
        .add_extension(
            x509.AuthorityKeyIdentifier.from_issuer_public_key(signer.public_key()),
            False,
        )
    )
    if with_ip:
        builder = builder.add_extension(
            x509.SubjectAlternativeName(
                [x509.IPAddress(ipaddress.ip_address("127.0.0.1"))]
            ),
            False,
        )
    (folder / f"{label}.pem").write_bytes(
        builder.sign(signer, hashes.SHA256()).public_bytes(PEM)
    )
    (folder / f"{label}.key").write_bytes(
        k.private_bytes(
            PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()
        )
    )
    return k, subject


class HubReads(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        # Expected TLS alert rejections are ordinary connection outcomes.
        asyncio.get_running_loop().set_debug(False)

    def setUp(self):
        stage = tempfile.TemporaryDirectory()
        self.addCleanup(stage.cleanup)
        self.stage = Path(stage.name)
        self.id = str(uuid.uuid4())
        self.site = issue(self.stage, "site")
        issue(self.stage, "node", self.site)
        issue(self.stage, "hub", self.site)
        issue(self.stage, "other", issue(self.stage, "untrusted"))
        self.received = []
        self.invokes = []
        self.peer_errors = []
        self.addCleanup(lambda: self.assertEqual(self.peer_errors, []))
        self.logger = logging.getLogger("test_sc_hub.websocket")
        self.logger.propagate = False
        if not self.logger.handlers:
            self.logger.addHandler(logging.NullHandler())

    def context(self, identity="hub", require_client=True):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_3
        ctx.load_cert_chain(
            self.stage / f"{identity}.pem", self.stage / f"{identity}.key"
        )
        ctx.load_verify_locations(self.stage / "site.pem")
        ctx.verify_mode = ssl.CERT_REQUIRED if require_client else ssl.CERT_NONE
        return ctx

    async def run_read(self, primary, *extra, station="020000001234", ok=True):
        proc = await asyncio.create_subprocess_exec(
            KIT,
            "sc-read",
            primary,
            station,
            POINT,
            "--uuid",
            self.id,
            "--cert",
            "node.pem",
            "--key",
            "node.key",
            "--issuers",
            "site.pem",
            *extra,
            cwd=self.stage,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            out, err = await asyncio.wait_for(proc.communicate(), 13)
        finally:
            if proc.returncode is None:
                proc.kill()
                await proc.wait()
        self.assertEqual(proc.returncode == 0, ok, (out, err))
        return (out + err).decode()

    async def fixture(self, handler, ctx=None, protocol=HUB_PROTOCOL):
        async def checked(ws):
            try:
                await handler(ws)
            except ConnectionClosed:
                pass
            except Exception as exc:
                self.peer_errors.append(str(exc))
                raise

        server = await serve(
            checked,
            "127.0.0.1",
            0,
            logger=self.logger,
            ssl=ctx or self.context(),
            subprotocols=[protocol],
            open_timeout=3,
            close_timeout=1,
        )

        async def cleanup():
            server.close()
            await server.wait_closed()

        self.addAsyncCleanup(cleanup)
        return uri(server.sockets[0].getsockname()[1])

    async def accept(self, ws):
        req = await ws.recv()
        self.assertEqual(req[:2], b"\x06\0")
        self.assertEqual(len(req), 30)
        self.received.append(req[4:26])
        await ws.send(
            b"\x07\0"
            + req[2:4]
            + HUB
            + uuid.uuid4().bytes
            + struct.pack(">HH", 5705, 1497)
        )
        frame = await ws.recv()
        self.invokes.append(frame[14])
        return frame

    async def answer(self, ws):
        frame = await self.accept(ws)
        self.assertEqual(frame[4:10], FIELD)
        invoke = frame[14]
        self.assertEqual(frame[15], 12)
        prefix = b"\x01\x08\0\x16"
        reply = (
            bytes([1, 0, 0x30, invoke, 12])
            + frame[16:]
            + bytes.fromhex("3e4441bc00003f")
        )
        # The first two responses have the right invoke ID but the wrong source or reference.
        other = reply.replace(bytes.fromhex("41bc0000"), bytes.fromhex("42c60000"))
        await ws.send(prefix + HUB + other)
        await ws.send(bytes.fromhex("010c0016") + FIELD + bytes([255]) * 6 + other)
        wrong_invoke = other[:3] + bytes([invoke ^ 1]) + other[4:]
        await ws.send(prefix + FIELD + wrong_invoke)
        await ws.send(
            prefix + FIELD + other.replace(bytes.fromhex("1955"), bytes.fromhex("1956"))
        )
        await ws.send(prefix + FIELD + reply)
        await self.disconnect(ws)

    async def disconnect(self, ws):
        with contextlib.suppress(ConnectionClosed):
            end = await ws.recv()
            if end[:2] == b"\x08\0":
                await ws.send(b"\x09\0" + end[2:4])

    # 008-sc-hub-read/SR-001
    async def test_mutual_auth(self):
        async def unexpected(ws):
            self.fail("peer admitted before mutual authentication")

        for ctx, diagnosis in (
            (self.context("other"), "TLS peer authentication"),
            (self.context(require_client=False), "did not request"),
        ):
            endpoint = await self.fixture(unexpected, ctx)
            self.assertIn(diagnosis, await self.run_read(endpoint, ok=False))

        async def unused(ws):
            await ws.wait_closed()

        endpoint = await self.fixture(unused, protocol="dc.bsc.bacnet.org")
        self.assertIn("WebSocket upgrade", await self.run_read(endpoint, ok=False))
        endpoint = await self.fixture(self.answer)
        self.assertIn("23.5", await self.run_read(endpoint))

    # 008-sc-hub-read/SR-002
    async def test_correlated_reply_and_terminal_error(self):
        endpoint = await self.fixture(self.answer)
        self.assertIn("23.5", await self.run_read(endpoint))

        async def error(ws):
            frame = await self.accept(ws)
            await ws.send(
                b"\x01\x08\0\x04"
                + FIELD
                + bytes([1, 0, 0x50, frame[14], 12, 0x91, 2, 0x91, 32])
            )
            await self.disconnect(ws)

        failed = await self.fixture(error)
        self.assertIn(
            "class 2, code 32",
            await self.run_read(failed, "--failover", endpoint, ok=False),
        )
        self.assertEqual(len(self.received), 2)

    # 008-sc-hub-read/SR-003
    async def test_failover_and_deadline(self):
        async def lost(ws):
            await self.accept(ws)
            await ws.close()

        primary = await self.fixture(lost)
        backup = await self.fixture(self.answer)
        result = await self.run_read(primary, "--failover", backup)
        self.assertIn("property read outstanding", result)
        self.assertIn("failover", result)
        self.assertIn("23.5", result)
        self.assertEqual(self.received[0], self.received[1])
        self.assertNotEqual(self.invokes[0], self.invokes[1])
        rejected = await self.fixture(self.answer, self.context("other"))
        self.assertIn(
            "failover: TLS peer authentication",
            await self.run_read(primary, "--failover", rejected, ok=False),
        )

        async def unrelated(ws):
            req = await ws.recv()
            # A mismatched Connect-Accept cannot release a ReadProperty request.
            await ws.send(b"\x07\0\xff\xff" + req[4:])
            await ws.wait_closed()

        slow = await self.fixture(unrelated)
        self.assertIn("Connect-Accept", await self.run_read(slow, ok=False))

    # 008-sc-hub-read/SR-003
    async def test_receive_bounds(self):
        async def silent(ws):
            frame = await self.accept(ws)
            unrelated = (
                bytes.fromhex("01080016")
                + HUB
                + bytes([1, 0, 0x30, frame[14], 12])
                + frame[16:]
                + bytes.fromhex("3e4441bc00003f")
            )
            while True:
                await ws.send(unrelated)
                await asyncio.sleep(0.15)

        silent_uri = await self.fixture(silent)
        start = asyncio.get_running_loop().time()
        self.assertIn(
            "deadline during property read outstanding",
            await self.run_read(silent_uri, ok=False),
        )
        self.assertLess(asyncio.get_running_loop().time() - start, 7)

        async def oversized(ws):
            await ws.recv()
            await ws.send([bytes(3000), bytes(3000)])
            await ws.wait_closed()

        huge = await self.fixture(oversized)
        self.assertIn("Connect-Accept", await self.run_read(huge, ok=False))
        for uri_text in (
            "ws://127.0.0.1/",
            "wss://127.0.0.1:99999/",
            "wss://u@127.0.0.1/",
        ):
            self.assertIn("invalid hub", await self.run_read(uri_text, ok=False))

    # 008-sc-hub-read/SR-001
    async def test_revoked_hub(self):
        ca, subject = self.site
        leaf = x509.load_pem_x509_certificate((self.stage / "hub.pem").read_bytes())
        now = datetime.now(UTC)
        entry = (
            x509.RevokedCertificateBuilder()
            .serial_number(leaf.serial_number)
            .revocation_date(now)
            .build()
        )
        crl = (
            x509.CertificateRevocationListBuilder()
            .issuer_name(subject)
            .last_update(now - timedelta(hours=1))
            .next_update(now + timedelta(days=1))
            .add_extension(x509.CRLNumber(1), False)
            .add_extension(
                x509.AuthorityKeyIdentifier.from_issuer_public_key(ca.public_key()),
                False,
            )
            .add_revoked_certificate(entry)
            .sign(ca, hashes.SHA256())
        )
        (self.stage / "hub.crl").write_bytes(crl.public_bytes(PEM))
        endpoint = await self.fixture(self.answer)
        self.assertIn(
            "revoked",
            await self.run_read(endpoint, "--peer-crl", "hub.crl", ok=False),
        )
        self.assertEqual(self.received, [])

    async def locate_field(self, hub):
        """Ask only the local fixture Device 4200 for its binding."""
        ctx = ssl.create_default_context(cafile=self.stage / "site.pem")
        ctx.minimum_version = ssl.TLSVersion.TLSv1_3
        ctx.load_cert_chain(self.stage / "observer.pem", self.stage / "observer.key")
        last = None
        for _ in range(30):
            try:
                async with (
                    asyncio.timeout(2),
                    connect(
                        hub,
                        ssl=ctx,
                        subprotocols=[HUB_PROTOCOL],
                        proxy=None,
                        close_timeout=0.1,
                    ) as ws,
                ):
                    await ws.send(
                        b"\x06\0\0\x01"
                        + bytes.fromhex("0200a0b0c0d0")
                        + uuid.uuid4().bytes
                        + struct.pack(">HH", 5705, 1478)
                    )
                    accepted = await ws.recv()
                    if accepted[:4] != bytes.fromhex("07000001"):
                        raise ValueError("observer Connect-Accept pending")
                    # Directed instance range, sent on the test hub's local link.
                    await ws.send(
                        bytes.fromhex("01040002ffffffffffff010010080a10681a1068")
                    )
                    async for frame in ws:
                        if frame[0] != 1 or frame[1] not in (8, 12):
                            continue
                        offset = 16 if frame[1] == 12 else 10
                        if frame[offset:].startswith(
                            bytes.fromhex("01001000c402001068")
                        ):
                            return frame[4:10].hex()
            except (OSError, ConnectionClosed, TimeoutError, ValueError) as exc:
                last = exc
            await asyncio.sleep(0.2)
        raise AssertionError(f"Device 4200 did not announce on {hub}: {last}")

    @unittest.skipUnless(
        os.environ.get("BACNET_SC_PEER_BIN"),
        "set BACNET_SC_PEER_BIN to run the independent C topology",
    )
    async def test_independent_hubs(self):
        build = Path(os.environ["BACNET_SC_PEER_BIN"]).resolve()
        for name in ("primary", "backup", "field", "observer"):
            issue(self.stage, name, self.site, with_ip=True)
        first, second = port(), port()
        while first == second:
            second = port()

        def start(name, instance, listen=None):
            work = self.stage / name
            work.mkdir()
            log = (work / "peer.log").open("w+")
            self.addCleanup(log.close)
            for filename in ("site.pem", f"{name}.pem", f"{name}.key"):
                (work / filename).hardlink_to(self.stage / filename)
            env = {
                **os.environ,
                "BACNET_DATALINK": "bsc",
                "BACNET_SC_ISSUER_1_CERTIFICATE_FILE": "site.pem",
                "BACNET_SC_OPERATIONAL_CERTIFICATE_FILE": f"{name}.pem",
                "BACNET_SC_OPERATIONAL_CERTIFICATE_PRIVATE_KEY_FILE": f"{name}.key",
                "BACNET_SC_PRIMARY_HUB_URI": uri(listen or first),
                "BACNET_SC_FAILOVER_HUB_URI": uri(listen or second),
            }
            env.pop("BACNET_SC_SELFSIGNED_ENABLED", None)
            if listen:
                env["BACNET_SC_HUB_FUNCTION_BINDING"] = f"lo:{listen}"
            process = subprocess.Popen(
                [build / ("bacschub" if listen else "bacserv"), str(instance), name],
                env=env,
                cwd=work,
                stdout=log,
                stderr=subprocess.STDOUT,
            )

            def stop():
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=3)

            self.addCleanup(stop)
            return process

        primary = start("primary", 4001, first)
        start("backup", 4002, second)
        start("field", 4200)
        try:
            station = await self.locate_field(uri(first))
            result = await self.run_read(
                uri(first), "--failover", uri(second), station=station
            )
            self.assertIn("Hub              primary", result)
            self.assertIn(POINT + "  0\n", result)
            primary.terminate()
            await asyncio.to_thread(primary.wait, 5)
            self.assertEqual(await self.locate_field(uri(second)), station)
            recovered = await self.run_read(
                uri(first), "--failover", uri(second), station=station
            )
            self.assertIn("Hub              failover", recovered)
            self.assertIn(POINT + "  0\n", recovered)
            print(
                f"Device 4200 at {station}: primary and failover both returned Analog Input 1 = 0"
            )
        except Exception:
            for name in ("primary", "backup", "field"):
                print(
                    (self.stage / name / "peer.log").read_text(errors="replace")[-1600:]
                )
            raise


if __name__ == "__main__":
    unittest.main(verbosity=2)
