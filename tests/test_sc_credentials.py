# /// script
# requires-python = ">=3.11"
# dependencies = ["cryptography==50.0.2"]
# ///
"""Exercise offline issuance, rollover and failure reporting through the shipped CLI."""

import os
import subprocess
import tempfile
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

BINARY = Path(__file__).resolve().parents[1] / "target/debug/bacnet-kit"
PEM = serialization.Encoding.PEM
SITE = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Local site signer")])
PRIVATE_LABEL = "private-fixture-label"
NOW = datetime.now(UTC)
DAY = timedelta(days=1)


def issue(
    subject,
    public,
    issuer,
    signer,
    *,
    ca=False,
    start=-1,
    end=40,
    eku=None,
    sign_ca=None,
):
    ku = x509.KeyUsage(
        digital_signature=True,
        content_commitment=False,
        key_encipherment=False,
        data_encipherment=False,
        key_agreement=False,
        key_cert_sign=ca if sign_ca is None else sign_ca,
        crl_sign=ca,
        encipher_only=False,
        decipher_only=False,
    )
    builder = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(public)
        .serial_number(x509.random_serial_number())
        .not_valid_before(NOW + start * DAY)
        .not_valid_after(NOW + end * DAY)
        .add_extension(x509.BasicConstraints(ca=ca, path_length=None), critical=True)
        .add_extension(ku, critical=True)
        .add_extension(
            x509.SubjectKeyIdentifier.from_public_key(public), critical=False
        )
        .add_extension(
            x509.AuthorityKeyIdentifier.from_issuer_public_key(signer.public_key()),
            critical=False,
        )
    )
    if eku:
        builder = builder.add_extension(x509.ExtendedKeyUsage([eku]), critical=False)
    return builder.sign(signer, hashes.SHA256())


def key():
    return ec.generate_private_key(ec.SECP256R1())


class Credentials(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.stage = Path(tmp.name)
        self.csr = self.request("node")
        self.old, self.new = key(), key()
        self.old_ca, self.new_ca = self.ca(self.old), self.ca(self.new)
        self.leaf = self.node(self.old)
        self.install(self.leaf, self.old_ca)
        self.key_bytes = (self.stage / "node.key").read_bytes()

    def request(self, name):
        for args in (
            [
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-out",
                f"{name}.key",
            ],
            [
                "req",
                "-new",
                "-sha256",
                "-key",
                f"{name}.key",
                "-subj",
                f"/CN={PRIVATE_LABEL}",
                "-out",
                f"{name}.csr",
            ],
        ):
            subprocess.run(
                ["openssl", *args], cwd=self.stage, check=True, capture_output=True
            )
        csr = x509.load_pem_x509_csr((self.stage / f"{name}.csr").read_bytes())
        self.assertTrue(csr.is_signature_valid)
        return csr

    def ca(self, signer, **kw):
        return issue(SITE, signer.public_key(), SITE, signer, ca=True, **kw)

    def node(self, signer, **kw):
        return issue(self.csr.subject, self.csr.public_key(), SITE, signer, **kw)

    def install(self, leaf, *signers):
        (self.stage / "node.pem").write_bytes(leaf.public_bytes(PEM))
        (self.stage / "cas.pem").write_bytes(
            b"".join(c.public_bytes(PEM) for c in signers)
        )

    def check(self, *args, failure=None, key_file="node.key", cert_file="node.pem"):
        proc = subprocess.run(
            [
                BINARY,
                "sc-check",
                "--cert",
                cert_file,
                "--key",
                key_file,
                "--issuers",
                "cas.pem",
                *args,
            ],
            cwd=self.stage,
            capture_output=True,
            text=True,
            check=False,
            timeout=5,
        )
        self.assertNotIn(PRIVATE_LABEL, proc.stdout + proc.stderr)
        self.assertNotIn("-----BEGIN", proc.stdout + proc.stderr)
        if failure is None:
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual(proc.stderr, "")
            self.assertIn("Credentials ready", proc.stdout)
        else:
            self.assertNotEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout, "")
            self.assertIn(failure, proc.stderr)
        return proc.stdout

    # 007-sc-credentials/SR-001
    def test_direct_identity(self):
        subprocess.run(
            [
                "openssl",
                "verify",
                "-purpose",
                "sslclient",
                "-CAfile",
                "cas.pem",
                "node.pem",
            ],
            cwd=self.stage,
            check=True,
            capture_output=True,
        )
        self.assertIn(self.leaf.fingerprint(hashes.SHA256()).hex(), self.check())
        self.install(self.leaf, self.new_ca)
        self.check(failure="direct issuer")
        (self.stage / "other.key").write_bytes(
            self.new.private_bytes(
                PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()
            )
        )
        self.check(key_file="other.key", failure="does not match")
        middle_name = x509.Name(
            [x509.NameAttribute(NameOID.COMMON_NAME, "Intermediate")]
        )
        middle = issue(middle_name, self.new.public_key(), SITE, self.old, ca=True)
        leaf = issue(self.csr.subject, self.csr.public_key(), middle_name, self.new)
        self.install(leaf, self.old_ca)
        self.check(failure="direct issuer")
        self.install(leaf, middle)
        self.check()
        self.install(
            self.node(self.old, eku=ExtendedKeyUsageOID.SERVER_AUTH), self.old_ca
        )
        self.check(failure="client authentication")
        self.install(self.leaf, self.ca(self.old, sign_ca=False))
        self.check(failure="CA signing authority")
        self.install(self.old_ca, self.old_ca)
        self.check(failure="signing leaf")

    # 007-sc-credentials/SR-002
    def test_rollover(self):
        second = self.request("replacement")
        replacement = issue(
            second.subject,
            second.public_key(),
            SITE,
            self.new,
            eku=ExtendedKeyUsageOID.CLIENT_AUTH,
        )
        (self.stage / "replacement.pem").write_bytes(replacement.public_bytes(PEM))
        staged = {"cert_file": "replacement.pem", "key_file": "replacement.key"}
        self.install(self.leaf, self.old_ca, self.new_ca)
        self.assertIn(
            self.old_ca.fingerprint(hashes.SHA256()).hex(),
            self.check("--min-days", "30"),
        )
        self.assertIn(
            self.new_ca.fingerprint(hashes.SHA256()).hex(),
            self.check("--min-days", "30", **staged),
        )
        self.check(cert_file="replacement.pem", failure="does not match")
        self.install(self.leaf, self.new_ca)
        self.check(failure="direct issuer")
        self.check("--min-days", "30", **staged)
        self.install(self.node(self.old, end=3), self.old_ca)
        self.check("--min-days", "4", failure="minimum lifetime")
        short = self.ca(self.old, end=3)
        self.install(self.leaf, short)
        self.check("--min-days", "4", failure="minimum lifetime")
        for pair in ((short, self.old_ca), (self.old_ca, short)):
            self.install(self.leaf, *pair)
            self.assertIn(
                self.old_ca.fingerprint(hashes.SHA256()).hex(),
                self.check("--min-days", "30"),
            )
        for window in ({"start": -5, "end": -2}, {"start": 2, "end": 40}):
            self.install(self.leaf, self.ca(self.old, **window))
            self.check(failure="issuer is outside")
            self.install(self.node(self.old, **window), self.old_ca)
            self.check(failure="certificate is outside")
        self.assertEqual((self.stage / "node.key").read_bytes(), self.key_bytes)

    # 007-sc-credentials/SR-002
    def test_revocation(self):
        self.assertIn("unknown (no CRL supplied)", self.check())
        for signer, start, end, revoked, failure in (
            (self.old, -1, 1, False, None),
            (self.old, -1, 1, True, "revoked"),
            (self.old, -3, -1, False, "update window"),
            (self.old, 1, 3, False, "update window"),
            (self.new, -1, 1, False, "revocation list"),
        ):
            builder = (
                x509.CertificateRevocationListBuilder()
                .issuer_name(SITE)
                .last_update(NOW + start * DAY)
                .next_update(NOW + end * DAY)
                .add_extension(x509.CRLNumber(1), False)
                .add_extension(
                    x509.AuthorityKeyIdentifier.from_issuer_public_key(
                        signer.public_key()
                    ),
                    False,
                )
            )
            if revoked:
                entry = (
                    x509.RevokedCertificateBuilder()
                    .serial_number(self.leaf.serial_number)
                    .revocation_date(NOW - DAY)
                    .build()
                )
                builder = builder.add_revoked_certificate(entry)
            (self.stage / "site.crl").write_bytes(
                builder.sign(signer, hashes.SHA256()).public_bytes(PEM)
            )
            output = self.check("--crl", "site.crl", failure=failure)
            if failure is None:
                self.assertIn("checked against supplied CRL", output)

    # 007-sc-credentials/SR-003
    def test_input_boundaries(self):
        leaf = self.leaf.public_bytes(PEM)
        for content in (
            leaf * 2,
            leaf + self.key_bytes,
            self.key_bytes,
            leaf + PRIVATE_LABEL.encode(),
        ):
            (self.stage / "node.pem").write_bytes(content)
            self.check(failure="invalid operational certificate")
        self.install(self.leaf, *([self.old_ca] * 9))
        self.check(failure="invalid issuer certificate")
        self.install(self.leaf, self.old_ca)
        for content, message in (
            (self.key_bytes * 2, "private key"),
            (b"x" * 262145, "256 KiB"),
        ):
            (self.stage / "bad.key").write_bytes(content)
            self.check(key_file="bad.key", failure=message)
        self.check(key_file=".", failure="regular file")
        if hasattr(os, "mkfifo"):
            native_key = PRIVATE_LABEL.encode() + b"\xff"
            os.link(
                self.stage / "node.key",
                os.path.join(os.fsencode(self.stage), native_key),
            )
            self.check(key_file=native_key)
            os.mkfifo(self.stage / "pipe.key")
            self.check(key_file="pipe.key", failure="regular file")
        self.check("--min-days", "-1", failure="Usage:")
        self.check("--cert", PRIVATE_LABEL, failure="Usage:")
        self.assertEqual((self.stage / "node.key").read_bytes(), self.key_bytes)


if __name__ == "__main__":
    unittest.main(verbosity=2)
