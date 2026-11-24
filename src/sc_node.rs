//! Bounded one-shot ReadProperty through a configured primary and optional failover SC hub.
mod socket;
mod tls;
use crate::{
    ReadPropertyRequest,
    client::{self, Reading},
    sc_pki, wire,
};
use bacnet_transport::{
    port::TransportPort,
    sc::{ScActiveHub, ScTransport, generate_random48_vmac},
};
use bacnet_types::enums::ConfirmedServiceChoice;
use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::tungstenite::http::Uri;
pub use uuid::Uuid;

/// Staged PEM material. The node and hub CRLs may have different issuers.
pub struct Credentials<'a> {
    pub certificate: &'a [u8],
    pub key: &'a [u8],
    pub issuers: &'a [u8],
    pub crl: Option<&'a [u8]>,
    pub peer_crl: Option<&'a [u8]>,
}
/// Public diagnosis of the phase that did not complete.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Stage {
    Configuration,
    Tcp,
    Tls,
    PeerCertificate(sc_pki::CheckError),
    MutualAuth,
    WebSocket,
    Connect,
    Read,
}
impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Configuration => "invalid hub, UUID or target VMAC",
            Self::Tcp => "TCP connection",
            Self::PeerCertificate(reason) => {
                return write!(f, "TLS peer authentication ({reason})");
            }
            Self::Tls => "TLS peer authentication",
            Self::MutualAuth => "hub did not request the node certificate",
            Self::WebSocket => "hub WebSocket upgrade",
            Self::Connect => "SC Connect-Accept",
            Self::Read => "property read outstanding",
        })
    }
}
impl std::error::Error for Stage {}
#[derive(Debug)]
pub enum Failure {
    Link(Stage),
    Deadline(Stage),
    Application(client::Error),
    Credential(sc_pki::CheckError),
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(s) => write!(f, "{s} failed"),
            Self::Deadline(s) => write!(f, "deadline during {s}"),
            Self::Application(e) => e.fmt(f),
            Self::Credential(e) => e.fmt(f),
        }
    }
}
impl From<Stage> for Failure {
    fn from(s: Stage) -> Self {
        Self::Link(s)
    }
}
#[derive(Debug)]
pub struct ReadError {
    pub primary: Failure,
    pub failover: Option<Failure>,
}
impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "primary: {}", self.primary)?;
        if let Some(other) = &self.failover {
            write!(f, ", failover: {other}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ReadError {}
/// A verified property and the outcome of the preceding primary attempt on failover.
#[derive(Debug)]
pub struct Outcome {
    pub reading: Reading,
    pub primary_failure: Option<Failure>,
}

/// A commissioning node with a caller-assigned persistent UUID. Each call
/// starts at the primary, with at most one fresh read through the backup.
pub struct Node<'a> {
    uuid: Uuid,
    vmac: [u8; 6],
    credentials: Credentials<'a>,
    next_invoke: u8,
}
impl<'a> Node<'a> {
    pub fn new(uuid: Uuid, credentials: Credentials<'a>) -> Result<Self, Stage> {
        if uuid.is_nil() {
            return Err(Stage::Configuration);
        }
        Ok(Self {
            uuid,
            vmac: generate_random48_vmac().map_err(|_| Stage::Configuration)?,
            credentials,
            next_invoke: 1,
        })
    }
    /// Read a local-link property. Each endpoint receives its own five-second budget.
    pub async fn read(
        &mut self,
        primary: &str,
        failover: Option<&str>,
        target: [u8; 6],
        property: &ReadPropertyRequest,
    ) -> Result<Outcome, ReadError> {
        let config = |stage| ReadError {
            primary: Failure::Link(stage),
            failover: None,
        };
        if target == [0; 6] || target == [255; 6] || target == self.vmac {
            return Err(config(Stage::Configuration));
        }
        let primary = socket::endpoint(primary).map_err(config)?;
        let backup = failover.map(socket::endpoint).transpose().map_err(config)?;
        let first = match self
            .attempt(primary, ScActiveHub::Primary, target, property)
            .await
        {
            Ok(reading) => {
                return Ok(Outcome {
                    reading,
                    primary_failure: None,
                });
            }
            Err(e) => e,
        };
        if let Some(uri) = backup
            && matches!(first, Failure::Link(_) | Failure::Deadline(_))
        {
            return match self
                .attempt(uri, ScActiveHub::Failover, target, property)
                .await
            {
                Ok(reading) => Ok(Outcome {
                    reading,
                    primary_failure: Some(first),
                }),
                Err(e) => Err(ReadError {
                    primary: first,
                    failover: Some(e),
                }),
            };
        }
        Err(ReadError {
            primary: first,
            failover: None,
        })
    }
    async fn attempt(
        &mut self,
        uri: Uri,
        role: ScActiveHub,
        target: [u8; 6],
        property: &ReadPropertyRequest,
    ) -> Result<Reading, Failure> {
        let invoke = self.next_invoke;
        self.next_invoke = invoke.wrapping_add(1);
        let deadline = Instant::now() + Duration::from_secs(5);
        let c = &self.credentials;
        if c.peer_crl.is_some_and(|p| p.len() > sc_pki::MAX_PEM_BYTES) {
            return Err(Failure::Credential(sc_pki::CheckError::OversizedInput));
        }
        let (identity, _) =
            sc_pki::node_identity(c.certificate, c.key, c.issuers, c.crl, SystemTime::now())
                .map_err(Failure::Credential)?;
        let (config, auth) = tls::config(
            Arc::new(identity),
            c.issuers.into(),
            c.peer_crl.map(Into::into),
        )?;
        let mut stage = Stage::Tcp;
        let attempt = async {
            let ws = socket::Socket::connect(uri, config, auth, &mut stage).await?;
            stage = Stage::Connect;
            let mut link = ScTransport::new(ws, self.vmac)
                .with_device_uuid(*self.uuid.as_bytes())
                .with_initial_hub(role);
            let mut rx = link.start().await.map_err(|_| Failure::Link(stage))?;
            stage = Stage::Read;
            let mut buf = [0; wire::MAX_FRAME];
            let size = wire::encode_read(invoke, property, &mut buf)
                .map_err(|_| Failure::Link(Stage::Configuration))?;
            // The shared encoder prefixes its NPDU with a four-octet B/IP header.
            link.send_unicast(&buf[4..size], &target)
                .await
                .map_err(|_| Failure::Link(stage))?;
            while let Some(msg) = rx.recv().await {
                if msg.link_layer_group || msg.source_mac.as_slice() != target {
                    continue;
                }
                let Ok(npdu) = bacnet_npdu::Npdu::decode(&msg.npdu) else {
                    continue;
                };
                if npdu.destination.is_some()
                    || npdu.source.is_some()
                    || npdu.message_type.is_some()
                {
                    continue;
                }
                if let Some(result) = client::reply(
                    npdu.payload,
                    invoke,
                    ConfirmedServiceChoice::READ_PROPERTY,
                    &|body| client::single_ack(body, property),
                ) {
                    return Ok((link, result));
                }
            }
            Err(Failure::Link(stage))
        };
        let (mut link, result) = timeout_at(deadline, attempt)
            .await
            .map_err(|_| Failure::Deadline(stage))??;
        // An overdue Disconnect-ACK cannot invalidate a completed transaction.
        let _ = timeout_at(
            deadline.min(Instant::now() + Duration::from_millis(150)),
            link.stop(),
        )
        .await;
        result.map_err(Failure::Application)
    }
}
