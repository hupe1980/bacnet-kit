+++
title = "Check Secure Connect credentials"
description = "Verify a staged node key, its direct signer and its remaining lifetime before a maintenance window."
weight = 6

[extra]
group = "Network and security"
terms = "BACnet/SC PKI TLS CRL CSR certificate signer rotation"
+++

Use this offline check before handing a software identity to a connecting BACnet/SC node. It reads local PEM files and reports which approved signer issued the certificate. Keep the node key on the staging host. Obtain the approved signing certificates and any available certificate revocation list (CRL) from the site administrator.

## Stage a new identity

Use the Homebrew CLI or install with Cargo feature `sc-pki`. Create a private staging directory for the identity. OpenSSL generates a P-256 key and signs the certificate signing request (CSR) with SHA-256.

```sh
umask 077
stage=$(mktemp -d)
cd "$stage"
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out node.key
openssl req -new -sha256 -key node.key -subj /CN=commissioning-node -out node.csr
```

Send only `node.csr` to the issuer. Place the returned operational certificate in `node.pem` and the site's approved direct signing certificates in `signers.pem`. Each input must be a regular PEM file up to 256 KiB. The checker accepts one leaf, one unencrypted software key, up to eight signers and one optional issuer CRL.

## Check the window

For a 30-day maintenance window, run the following with the issuer's local CRL.

```sh
bacnet-kit sc-check --cert node.pem --key node.key --issuers signers.pem \
  --crl site.crl.pem --min-days 30
```

A successful check exits zero and prints the leaf and signer SHA-256 fingerprints, the earlier expiry in UTC, whole days remaining and the revocation result. The horizon covers both certificates. CRL currency is evaluated at the time of the check. If the site has no CRL, omit `--crl`. The output then reads `unknown (no CRL supplied)`. A supplied CRL must be signed by the issuer, applicable to the leaf and within its update window.

The checker validates a connecting node's signing identity using the workstation clock. Direct signature verification and available revocation evidence follow the connection-security rules in Annex AB. DNS names and the operating system trust store play no part in this check. A live mutual TLS handshake, hub admission and an application read need separate commissioning evidence. [ASHRAE connection security, AB.7.4](https://www.ashrae.org/file%20library/technical%20resources/standards%20and%20guidelines/standards%20addenda/135_2020_cp_20241129.pdf)

## Rotate a signer

1. Stage the replacement key and signed certificate in a second private directory. Keep the deployed identity available during the trial.
2. Combine the approved old and new signing certificates in one PEM bundle. Run the check once per identity, supplying the corresponding issuer CRL when available. Compare the printed signer fingerprint with the administrator's record.
3. Check the replacement again using only the new signer and the intended maintenance window. If two eligible signers verify the same leaf, the checker selects the pair with the latest effective expiry.
4. Record the public fingerprints. Schedule activation and verify a connection and a read with the intended hub before retiring the old credentials.

<details class="watch-case"><summary>The key or direct signer failed</summary><p>Confirm the certificate was issued for this CSR and that the bundle contains its immediate signing CA. A root above an issuing intermediate cannot verify the leaf directly. Compare the signer's SHA-256 fingerprint with the site record.</p></details>

<details class="watch-case"><summary>The validity or revocation check failed</summary><p>Check the workstation's UTC clock and both certificate expiry dates. Fetch the current CRL for the direct signer from the site administrator. Replace a revoked identity and request fresh credentials when the maintenance window exceeds the remaining lifetime.</p></details>

After preflight, check live admission by reading a commissioned point. [Read through a Secure Connect hub](@/docs/sc-read.md)

The independent fixture generates disposable identities and exercises rollout, retirement and rejection. [Repeat the credential checks](@/docs/verification.md#staged-secure-connect-credentials)
