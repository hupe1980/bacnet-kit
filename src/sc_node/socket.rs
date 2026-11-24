//! Bounded binary WebSocket used by the upstream SC state machine.
use super::{Stage, tls::NodeAuth};
use bacnet_transport::{sc::WebSocketPort, sc_frame::BACNET_SC_HUB_SUBPROTOCOL};
use bacnet_types::error::Error;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use std::sync::Arc;
use tokio::{net::TcpStream, sync::Mutex};
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        ClientRequestBuilder, Message,
        http::{Uri, header::SEC_WEBSOCKET_PROTOCOL},
        protocol::WebSocketConfig,
    },
};

type Stream = WebSocketStream<TlsStream<TcpStream>>;
pub(super) struct Socket {
    write: Mutex<SplitSink<Stream, Message>>,
    read: Mutex<SplitStream<Stream>>,
}

pub(super) fn endpoint(url: &str) -> Result<Uri, Stage> {
    if url.len() > 2048 || url.contains('#') {
        return Err(Stage::Configuration);
    }
    let uri: Uri = url.parse().map_err(|_| Stage::Configuration)?;
    let authority = uri.authority().ok_or(Stage::Configuration)?;
    // Validate the explicit port before applying the default. An invalid
    // numeric suffix must never silently dial a different port.
    if uri.scheme_str() != Some("wss") || authority.as_str().contains('@') {
        return Err(Stage::Configuration);
    }
    let host = authority.host();
    let suffix = &authority.as_str()[host.len()..];
    if host.is_empty()
        || (!suffix.is_empty()
            && (authority.port_u16().is_none_or(|p| p == 0)
                || !suffix
                    .strip_prefix(':')
                    .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))))
    {
        return Err(Stage::Configuration);
    }
    Ok(uri)
}

impl Socket {
    pub(super) async fn connect(
        uri: Uri,
        config: Arc<rustls::ClientConfig>,
        auth: Arc<NodeAuth>,
        stage: &mut Stage,
    ) -> Result<Self, Stage> {
        let host = uri
            .host()
            .ok_or(Stage::Configuration)?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|_| Stage::Configuration)?;
        *stage = Stage::Tcp;
        let tcp = TcpStream::connect((host, uri.port_u16().unwrap_or(443)))
            .await
            .map_err(|_| *stage)?;
        let _ = tcp.set_nodelay(true);
        *stage = Stage::Tls;
        let tls = TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(super::tls::failure)?;
        if !auth.requested() {
            return Err(Stage::MutualAuth);
        }
        *stage = Stage::WebSocket;
        let request = ClientRequestBuilder::new(uri).with_sub_protocol(BACNET_SC_HUB_SUBPROTOCOL);
        let limits = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(4096)
            .max_write_buffer_size(131098)
            .max_frame_size(Some(5705))
            .max_message_size(Some(5705));
        let (ws, response) = client_async_with_config(request, tls, Some(limits))
            .await
            .map_err(|_| *stage)?;
        if response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|s| s.to_str().ok())
            != Some(BACNET_SC_HUB_SUBPROTOCOL)
        {
            return Err(*stage);
        }
        let (write, read) = ws.split();
        Ok(Self {
            write: Mutex::new(write),
            read: Mutex::new(read),
        })
    }
}
fn closed() -> Error {
    Error::Encoding("SC WebSocket closed or invalid".into())
}
impl WebSocketPort for Socket {
    async fn send(&self, data: &[u8]) -> Result<(), Error> {
        self.write
            .lock()
            .await
            .send(Message::Binary(data.to_vec().into()))
            .await
            .map_err(|_| closed())
    }
    async fn recv(&self) -> Result<Vec<u8>, Error> {
        loop {
            let frame = self
                .read
                .lock()
                .await
                .next()
                .await
                .ok_or_else(closed)?
                .map_err(|_| closed())?;
            match frame {
                Message::Binary(b) => return Ok(b.to_vec()),
                Message::Ping(_) => self
                    .write
                    .lock()
                    .await
                    .flush()
                    .await
                    .map_err(|_| closed())?,
                Message::Pong(_) => (),
                _ => return Err(closed()),
            }
        }
    }
}
