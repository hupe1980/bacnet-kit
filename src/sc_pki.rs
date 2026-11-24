//! Offline readiness checks for a BACnet/SC node's staged TLS client credentials.
//! Trust comes from the supplied direct issuers. The caller controls the clock.
use rustls::pki_types::{
    CertificateDer, CertificateRevocationListDer, PrivateKeyDer, UnixTime, pem::PemObject,
};
use sha2::{Digest, Sha256};
use std::{fmt, time::SystemTime};
use webpki::{BorrowedCertRevocationList, EndEntityCert, RevocationOptionsBuilder};
use x509_parser::prelude::*;

/// Maximum size of each PEM input.
pub const MAX_PEM_BYTES: usize = 256 * 1024;
const MAX_ISSUERS: usize = 8;
const DAY: i64 = 86_400;

/// A diagnosis that can safely be displayed even for untrusted input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckError {
    OversizedInput,
    InvalidCertificate,
    InvalidIssuer,
    InvalidKey,
    InvalidCrl,
    CrlOutsideWindow,
    InvalidClock,
    InvalidLeafUsage,
    LeafOutsideWindow,
    KeyMismatch,
    NoDirectIssuer,
    InvalidIssuerUsage,
    Revoked,
    ClientAuthForbidden,
    ServerAuthForbidden,
    IssuerOutsideWindow,
    InsufficientLifetime,
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OversizedInput => "PEM input exceeds 256 KiB",
            Self::InvalidCertificate => "invalid operational certificate",
            Self::InvalidIssuer => "invalid issuer certificate",
            Self::InvalidKey => "expected one supported, unencrypted private key",
            Self::InvalidCrl => "invalid or inapplicable issuer revocation list",
            Self::CrlOutsideWindow => "revocation list is outside its update window",
            Self::InvalidClock => "invalid evaluation time",
            Self::InvalidLeafUsage => "operational certificate cannot serve as a TLS signing leaf",
            Self::LeafOutsideWindow => "operational certificate is outside its validity window",
            Self::KeyMismatch => "private key does not match the operational certificate",
            Self::NoDirectIssuer => {
                "no approved direct issuer verified this operational certificate"
            }
            Self::InvalidIssuerUsage => {
                "issuer bundle contains a certificate without CA signing authority"
            }
            Self::Revoked => "operational certificate is revoked",
            Self::ServerAuthForbidden => {
                "operational certificate does not permit TLS server authentication"
            }
            Self::ClientAuthForbidden => {
                "operational certificate does not permit TLS client authentication"
            }
            Self::IssuerOutsideWindow => "direct issuer is outside its validity window",
            Self::InsufficientLifetime => {
                "leaf or direct issuer expires before the requested minimum lifetime"
            }
        })
    }
}
impl std::error::Error for CheckError {}

/// Public facts about the selected credential pair.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CredentialReport {
    pub leaf_sha256: String,
    pub issuer_sha256: String,
    /// Earlier of the leaf and issuer expiry, seconds since the Unix epoch.
    pub expires_at: i64,
    pub remaining_days: u64,
    /// True only after a current, applicable and signed CRL cleared the leaf.
    pub crl_checked: bool,
}

const CERT: &str = "CERTIFICATE";
const CRL: &str = "X509 CRL";

// The provider PEM decoder skips unrelated objects. Validate the entire envelope
// first so an unexpected key, extra certificate or trailing data is never ignored.
fn blocks<'a>(
    pem: &'a [u8],
    labels: &[&str],
    limit: usize,
    error: CheckError,
) -> Result<Vec<&'a [u8]>, CheckError> {
    if pem.len() > MAX_PEM_BYTES {
        return Err(CheckError::OversizedInput);
    }
    let mut rest = std::str::from_utf8(pem).map_err(|_| error)?.trim_ascii();
    let mut found = Vec::new();
    while !rest.is_empty() {
        let (header, body) = rest.split_once('\n').ok_or(error)?;
        let label = header
            .trim_end_matches('\r')
            .strip_prefix("-----BEGIN ")
            .and_then(|h| h.strip_suffix("-----"))
            .ok_or(error)?;
        if !labels.contains(&label) || found.len() == limit {
            return Err(error);
        }
        let footer = format!("\n-----END {label}-----");
        let (encoded, trailing) = body.split_once(&footer).ok_or(error)?;
        if encoded.is_empty()
            || !encoded
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/=\r\n".contains(&c))
            || !(trailing.is_empty() || trailing.starts_with('\n') || trailing.starts_with("\r\n"))
        {
            return Err(error);
        }
        found.push(&rest.as_bytes()[..rest.len() - trailing.len()]);
        rest = trailing.trim_ascii();
    }
    if found.is_empty() {
        return Err(error);
    }
    Ok(found)
}

fn cert<'a>(der: &'a [u8], error: CheckError) -> Result<X509Certificate<'a>, CheckError> {
    let (rest, cert) = parse_x509_certificate(der).map_err(|_| error)?;
    if !rest.is_empty() {
        return Err(error);
    }
    Ok(cert)
}

fn current_crl(der: &[u8], now: i64) -> Result<webpki::CertRevocationList<'_>, CheckError> {
    let (rest, list) =
        CertificateRevocationList::from_der(der).map_err(|_| CheckError::InvalidCrl)?;
    if !rest.is_empty() {
        return Err(CheckError::InvalidCrl);
    }
    if list.last_update().timestamp() > now
        || list.next_update().is_none_or(|t| t.timestamp() <= now)
    {
        return Err(CheckError::CrlOutsideWindow);
    }
    Ok(BorrowedCertRevocationList::from_der(der)
        .map_err(|_| CheckError::InvalidCrl)?
        .into())
}

fn match_key(
    private_key: &[u8],
    leaf: &X509Certificate<'_>,
) -> Result<std::sync::Arc<dyn rustls::sign::SigningKey>, CheckError> {
    let key_pem = blocks(
        private_key,
        &["PRIVATE KEY", "EC PRIVATE KEY", "RSA PRIVATE KEY"],
        1,
        CheckError::InvalidKey,
    )?[0];
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|_| CheckError::InvalidKey)?;
    let signing = rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(key)
        .map_err(|_| CheckError::InvalidKey)?;
    if signing
        .public_key()
        .is_none_or(|spki| spki.as_ref() != leaf.public_key().raw)
    {
        return Err(CheckError::KeyMismatch);
    }
    Ok(signing)
}

fn select_issuer(
    issuers: &[u8],
    end: &EndEntityCert<'_>,
    leaf_expiry: i64,
    time: UnixTime,
    revocation: Option<webpki::RevocationOptions<'_>>,
    purpose: webpki::KeyUsage,
) -> Result<(i64, String), CheckError> {
    let now = time.as_secs() as i64;
    let mut chosen: Option<(i64, String)> = None;
    let mut failure = CheckError::NoDirectIssuer;
    for pem in blocks(issuers, &[CERT], MAX_ISSUERS, CheckError::InvalidIssuer)? {
        let ca_der = CertificateDer::from_pem_slice(pem).map_err(|_| CheckError::InvalidIssuer)?;
        let ca = cert(&ca_der, CheckError::InvalidIssuer)?;
        let usage = ca.key_usage().map_err(|_| CheckError::InvalidIssuer)?;
        if !ca
            .basic_constraints()
            .map_err(|_| CheckError::InvalidIssuer)?
            .is_some_and(|bc| bc.value.ca)
            || usage.as_ref().is_some_and(|ku| !ku.value.key_cert_sign())
        {
            return Err(CheckError::InvalidIssuerUsage);
        }
        let anchor =
            webpki::anchor_from_trusted_cert(&ca_der).map_err(|_| CheckError::InvalidIssuer)?;
        let verified = end.verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            std::slice::from_ref(&anchor),
            &[],
            time,
            purpose,
            revocation,
            None,
        );
        match verified {
            Ok(_) => (),
            Err(webpki::Error::CertRevoked) => {
                return Err(CheckError::Revoked);
            }
            Err(webpki::Error::RequiredEkuNotFoundContext(_)) => {
                return Err(if purpose == webpki::KeyUsage::client_auth() {
                    CheckError::ClientAuthForbidden
                } else {
                    CheckError::ServerAuthForbidden
                });
            }
            Err(webpki::Error::UnknownIssuer) => continue,
            Err(
                webpki::Error::UnknownRevocationStatus
                | webpki::Error::InvalidCrlSignatureForPublicKey
                | webpki::Error::IssuerNotCrlSigner
                | webpki::Error::UnsupportedCrlSignatureAlgorithmContext(_)
                | webpki::Error::UnsupportedCrlSignatureAlgorithmForPublicKeyContext(_),
            ) => {
                failure = CheckError::InvalidCrl;
                continue;
            }
            Err(_) => continue,
        }
        if revocation.is_some() && usage.is_some_and(|ku| !ku.value.crl_sign()) {
            failure = CheckError::InvalidCrl;
            continue;
        }
        if !(ca.validity().not_before.timestamp()..=ca.validity().not_after.timestamp())
            .contains(&now)
        {
            failure = CheckError::IssuerOutsideWindow;
            continue;
        }
        let expiry = leaf_expiry.min(ca.validity().not_after.timestamp());
        let candidate = (expiry, format!("{:x}", Sha256::digest(&ca_der)));
        if chosen.as_ref().is_none_or(|best| candidate > *best) {
            chosen = Some(candidate);
        }
    }
    chosen.ok_or(failure)
}

/// Verify staged PEM material for a node initiating a connection. Accepts one
/// operational certificate and key, up to eight direct CAs, and one optional
/// issuer CRL. The minimum lifetime covers both certificates in whole days.
pub fn check(
    certificate: &[u8],
    private_key: &[u8],
    issuers: &[u8],
    crl: Option<&[u8]>,
    at: SystemTime,
    min_days: u32,
) -> Result<CredentialReport, CheckError> {
    let (_, report) = node_identity(certificate, private_key, issuers, crl, at)?;
    if report.remaining_days < u64::from(min_days) {
        return Err(CheckError::InsufficientLifetime);
    }
    Ok(report)
}

pub(crate) fn node_identity(
    certificate: &[u8],
    private_key: &[u8],
    issuers: &[u8],
    crl: Option<&[u8]>,
    at: SystemTime,
) -> Result<(rustls::sign::CertifiedKey, CredentialReport), CheckError> {
    let duration = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| CheckError::InvalidClock)?;
    let time = UnixTime::since_unix_epoch(duration);
    let der = CertificateDer::from_pem_slice(
        blocks(certificate, &[CERT], 1, CheckError::InvalidCertificate)?[0],
    )
    .map_err(|_| CheckError::InvalidCertificate)?;
    let leaf = operational_leaf(&der, time)?;
    let key = match_key(private_key, &leaf)?;
    let report = verify_leaf(
        &der,
        &leaf,
        issuers,
        crl,
        time,
        webpki::KeyUsage::client_auth(),
    )?;
    Ok((rustls::sign::CertifiedKey::new(vec![der], key), report))
}

pub(crate) fn operational_leaf<'a>(
    der: &'a CertificateDer<'_>,
    time: UnixTime,
) -> Result<X509Certificate<'a>, CheckError> {
    let now = i64::try_from(time.as_secs()).map_err(|_| CheckError::InvalidClock)?;
    let leaf = cert(der, CheckError::InvalidCertificate)?;
    if leaf
        .basic_constraints()
        .map_err(|_| CheckError::InvalidCertificate)?
        .is_some_and(|c| c.value.ca)
        || leaf
            .key_usage()
            .map_err(|_| CheckError::InvalidCertificate)?
            .is_some_and(|ku| !ku.value.digital_signature())
    {
        return Err(CheckError::InvalidLeafUsage);
    }
    if !(leaf.validity().not_before.timestamp()..=leaf.validity().not_after.timestamp())
        .contains(&now)
    {
        return Err(CheckError::LeafOutsideWindow);
    }
    Ok(leaf)
}

pub(crate) fn verify_leaf(
    der: &CertificateDer<'_>,
    leaf: &X509Certificate<'_>,
    issuers: &[u8],
    crl: Option<&[u8]>,
    time: UnixTime,
    purpose: webpki::KeyUsage,
) -> Result<CredentialReport, CheckError> {
    let now = i64::try_from(time.as_secs()).map_err(|_| CheckError::InvalidClock)?;
    let end = EndEntityCert::try_from(der).map_err(|_| CheckError::InvalidCertificate)?;
    let crl_der = crl
        .map(|p| {
            CertificateRevocationListDer::from_pem_slice(
                blocks(p, &[CRL], 1, CheckError::InvalidCrl)?[0],
            )
            .map_err(|_| CheckError::InvalidCrl)
        })
        .transpose()?;
    let list = crl_der
        .as_deref()
        .map(|d| current_crl(d, now))
        .transpose()?;
    let lists: Vec<_> = list.iter().collect();
    let revocation = if list.is_some() {
        Some(
            RevocationOptionsBuilder::new(&lists)
                .map_err(|_| CheckError::InvalidCrl)?
                .with_expiration_policy(webpki::ExpirationPolicy::Enforce)
                .build(),
        )
    } else {
        None
    };
    let (expires_at, issuer_sha256) = select_issuer(
        issuers,
        &end,
        leaf.validity().not_after.timestamp(),
        time,
        revocation,
        purpose,
    )?;
    let remaining_days = ((expires_at - now) / DAY) as u64;
    Ok(CredentialReport {
        leaf_sha256: format!("{:x}", Sha256::digest(der)),
        issuer_sha256,
        expires_at,
        remaining_days,
        crl_checked: list.is_some(),
    })
}
