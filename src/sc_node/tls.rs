//! TLS 1.3 site trust and per-handshake client authentication.
use super::Stage;
use crate::sc_pki;
use rustls::{
    ClientConfig, DigitallySignedStruct, Error, SignatureScheme,
    client::{
        ResolvesClientCert, Resumption,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::{ring, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
    sign::CertifiedKey,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
struct SiteTrust {
    issuers: Arc<[u8]>,
    crl: Option<Arc<[u8]>>,
}
impl ServerCertVerifier for SiteTrust {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _chain: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // Only a configured direct signer can verify the leaf. Peer-supplied
        // intermediates do not participate in path building (Annex AB.7.4).
        let validated = sc_pki::operational_leaf(end, now).and_then(|leaf| {
            sc_pki::verify_leaf(
                end,
                &leaf,
                &self.issuers,
                self.crl.as_deref(),
                now,
                webpki::KeyUsage::server_auth(),
            )
        });
        validated
            .map(|_| ServerCertVerified::assertion())
            .map_err(|reason| {
                Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(
                    Arc::new(reason),
                )))
            })
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("TLS 1.3 required".into()))
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            msg,
            cert,
            sig,
            &ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
#[derive(Debug)]
pub(super) struct NodeAuth {
    identity: Arc<CertifiedKey>,
    requested: AtomicBool,
}
impl ResolvesClientCert for NodeAuth {
    fn has_certs(&self) -> bool {
        true
    }
    fn resolve(&self, _: &[&[u8]], schemes: &[SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        self.identity.key.choose_scheme(schemes)?;
        self.requested.store(true, Ordering::Release);
        Some(self.identity.clone())
    }
}
impl NodeAuth {
    pub(super) fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}
pub(super) fn config(
    identity: Arc<CertifiedKey>,
    issuers: Arc<[u8]>,
    crl: Option<Arc<[u8]>>,
) -> Result<(Arc<ClientConfig>, Arc<NodeAuth>), Stage> {
    let auth = Arc::new(NodeAuth {
        identity,
        requested: AtomicBool::new(false),
    });
    let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Stage::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SiteTrust { issuers, crl }))
        .with_client_cert_resolver(auth.clone());
    config.resumption = Resumption::disabled();
    Ok((Arc::new(config), auth))
}

// Tokio preserves the rustls error inside the I/O error. Surface only our
// closed set of certificate diagnostics, never peer-controlled error text.
pub(super) fn failure(error: std::io::Error) -> Stage {
    if let Some(Error::InvalidCertificate(rustls::CertificateError::Other(other))) =
        error.get_ref().and_then(|e| e.downcast_ref::<Error>())
        && let Some(reason) = other.0.downcast_ref::<sc_pki::CheckError>()
    {
        return Stage::PeerCertificate(*reason);
    }
    Stage::Tls
}
