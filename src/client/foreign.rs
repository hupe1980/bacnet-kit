//! Passive I-Am observation through a commissioned IPv4 broadcast manager.
use super::{Error, RemoteStation, link};
use bacnet_bip_core::codec::BvlcFunction;
pub use bacnet_bip_core::node::RegistrationEvent;
use bacnet_bip_core::{
    Instant as Tick, LocalSubnet,
    node::{
        Action, Addressed, Datagram, Foreign, Inbound, Link, LocalFailure, Mode, Node, Permissive,
    },
};
use bacnet_npdu::Npdu;
use bacnet_services::who_is::IAmRequest;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    time::{Duration, Instant},
};

/// One forwarded announcement with the origin as conveyed by the configured BBMD.
#[derive(Debug)]
pub struct Announcement {
    pub bbmd: SocketAddrV4,
    pub origin: SocketAddrV4,
    pub station: Option<RemoteStation>,
    pub device: IAmRequest,
}
#[derive(Debug)]
pub enum ForeignEvent {
    Registration(RegistrationEvent),
    Announcement(Announcement),
}

/// A finite foreign-device lease. Drive registration and receive announcements with
/// [Self::next_until]. On drop, the BBMD ages out the registration.
///
/// ```no_run
/// use bacnet_kit::client::{ForeignEvent, ForeignListener};
/// use std::{net::Ipv4Addr, time::{Duration, Instant}};
/// let bbmd = "127.0.0.1:47808".parse()?;
/// let mut listener = ForeignListener::bind(Ipv4Addr::UNSPECIFIED, bbmd, 60)?;
/// let end = Instant::now() + Duration::from_secs(15);
/// while let Some(event) = listener.next_until(end)? {
///     if let ForeignEvent::Announcement(a) = event {
///         println!("{} from {} through {}", a.device.object_identifier, a.origin, a.bbmd);
///     }
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct ForeignListener {
    socket: UdpSocket,
    bbmd: SocketAddrV4,
    node: Node<1>,
    epoch: Instant,
}
impl ForeignListener {
    /// Open a dedicated connected socket. The first call to next_until starts registration.
    pub fn bind(interface: Ipv4Addr, bbmd: SocketAddrV4, ttl: u16) -> Result<Self, Error> {
        if ttl == 0 {
            return Err(Error::Invalid("foreign-device TTL must be 1–65535 seconds"));
        }
        let socket = link::connected(interface, bbmd)?;
        let SocketAddr::V4(own) = socket.local_addr()? else {
            unreachable!("bound IPv4 socket")
        };
        let subnet = LocalSubnet::new(SocketAddrV4::new(Ipv4Addr::BROADCAST, own.port()));
        let link = Link::new(own, subnet, Mode::Foreign(Foreign::new(bbmd, ttl)))
            .map_err(|_| Error::Invalid("invalid BBMD registration settings"))?;
        let node = Node::new(link, Tick::ZERO).map_err(|_| Error::Invalid("invalid BBMD link"))?;
        Ok(Self {
            socket,
            bbmd,
            node,
            epoch: Instant::now(),
        })
    }
    fn tick(&self) -> Tick {
        Tick::from_nanos(
            self.epoch
                .elapsed()
                .as_nanos()
                .try_into()
                .unwrap_or(u64::MAX),
        )
    }
    /// Whether an acknowledged registration is still live in the local time window.
    pub fn registered(&self) -> bool {
        self.node
            .registration(self.tick())
            .is_some_and(|s| s.registered)
    }

    /// Return the next lease change or admitted I-Am, bounded by the caller's deadline.
    pub fn next_until(&mut self, end: Instant) -> Result<Option<ForeignEvent>, Error> {
        const CAP: usize = 10 + bacnet_npdu::MAX_NPDU;
        let mut buf = [0; CAP + 1];
        while Instant::now() < end {
            self.node.on_deadline(self.tick());
            loop {
                match self.node.poll_action() {
                    Action::Idle => break,
                    Action::Registration(event) => {
                        return Ok(Some(ForeignEvent::Registration(event)));
                    }
                    action @ Action::Register { exchange, .. } => {
                        let mut request = [0; 6];
                        let n = action
                            .message()
                            .unwrap()
                            .encode_into(&mut request)
                            .map_err(|_| Error::Invalid("cannot encode registration"))?;
                        let wait = end.saturating_duration_since(Instant::now());
                        if wait.is_zero() {
                            self.node.exchange_failed(
                                exchange,
                                LocalFailure::Cancelled,
                                self.tick(),
                            );
                            return Ok(None);
                        }
                        let sent = self
                            .socket
                            .set_write_timeout(Some(wait))
                            .and_then(|()| self.socket.send(&request[..n]));
                        match sent {
                            Ok(_) => {
                                self.node.exchange_sent(exchange, self.tick());
                            }
                            Err(_) => self.node.exchange_failed(
                                exchange,
                                LocalFailure::SendFailed,
                                self.tick(),
                            ),
                        }
                    }
                    Action::ExchangeEnded { .. } | Action::DiscardHeld | Action::ReleaseHeld => {}
                }
            }
            let wake = self
                .node
                .next_deadline()
                .and_then(|due| self.epoch.checked_add(Duration::from_nanos(due.as_nanos())))
                .map_or(end, |due| end.min(due));
            let n = match link::recv(&self.socket, &mut buf, wake) {
                Ok(Some(n)) if n <= CAP && Instant::now() < end => n,
                Ok(_) => continue,
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::ConnectionRefused => continue,
                Err(e) => return Err(e),
            };
            let datagram = Datagram::new(
                &buf[..n],
                self.bbmd,
                Some(Addressed {
                    node: true,
                    broadcast: false,
                }),
            );
            let now = self.tick();
            if let Inbound::Deliver(d) = self.node.receive(&datagram, now, &mut Permissive)
                && d.group
                && buf[1] == BvlcFunction::FORWARDED_NPDU.to_raw()
                && self.registered()
                && let Some(event) = self.announcement(d.source, d.npdu)
            {
                return Ok(Some(ForeignEvent::Announcement(event)));
            }
        }
        Ok(None)
    }

    fn announcement(&self, origin: SocketAddrV4, bytes: &[u8]) -> Option<Announcement> {
        if !link::unicast(origin) {
            return None;
        }
        let npdu = Npdu::decode(bytes).ok()?;
        if npdu.message_type.is_some()
            || npdu.expecting_reply
            || npdu
                .destination
                .is_some_and(|d| d.network != bacnet_npdu::GLOBAL_NETWORK || !d.mac.is_empty())
        {
            return None;
        }
        let station = match npdu.source {
            Some(s) => Some(RemoteStation::new(s.network, s.mac).ok()?),
            None => None,
        };
        let device = IAmRequest::decode(npdu.payload.strip_prefix(&[0x10, 0])?).ok()?;
        if device.object_identifier.object_type() != crate::ObjectType::DEVICE
            || device.object_identifier.instance_number()
                == crate::ObjectIdentifier::WILDCARD_INSTANCE
            || device.max_apdu_length < 50
            || device.segmentation_supported.to_raw() > 3
        {
            return None;
        }
        Some(Announcement {
            bbmd: self.bbmd,
            origin,
            station,
            device,
        })
    }
}
