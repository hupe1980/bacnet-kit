//! Directed delivery to a local device or one station behind a known router.
use super::{Error, remaining, wire};
use bacnet_bip_core::codec::{self, Bvll};
use bacnet_npdu::{Address, Mac, Npdu};
use std::{
    fmt, io,
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    str::FromStr,
    time::Instant,
};

pub(super) const MAX_DATAGRAM: usize = 4 + bacnet_npdu::MAX_NPDU;

/// A remote BACnet network number and a link-layer station address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RemoteStation {
    network: u16,
    mac: Mac,
}
impl RemoteStation {
    /// Construct a unicast station. Up to seven address octets cover standard BACnet links.
    pub fn new(network: u16, mac: &[u8]) -> Result<Self, Error> {
        if !(1..=65534).contains(&network) || !(1..=7).contains(&mac.len()) {
            return Err(Error::Invalid(
                "expected network 1–65534 and 1–7 station octets",
            ));
        }
        Ok(Self {
            network,
            mac: Mac::from_slice(mac).expect("bounded station address"),
        })
    }
    pub fn network(&self) -> u16 {
        self.network
    }
    pub fn mac(&self) -> &[u8] {
        self.mac.as_slice()
    }
    fn address(&self) -> Address<'_> {
        Address {
            network: self.network,
            mac: self.mac(),
        }
    }
}
impl fmt::Display for RemoteStation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:0x", self.network)?;
        for b in self.mac() {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
impl FromStr for RemoteStation {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let invalid = Error::Invalid("expected NETWORK:0xHEX or NETWORK:DECIMAL (one octet)");
        let (net, station) = s.split_once(':').ok_or(invalid)?;
        let net = net
            .parse()
            .map_err(|_| Error::Invalid("invalid BACnet network number"))?;
        if let Some(hex) = station.strip_prefix("0x") {
            if !hex.is_ascii() || hex.len() > 14 || !hex.len().is_multiple_of(2) {
                return Err(Error::Invalid("station hex needs 1–7 complete octets"));
            }
            let mut mac = [0; 7];
            for (i, b) in mac.iter_mut().take(hex.len() / 2).enumerate() {
                let pair = &hex[i * 2..i * 2 + 2];
                if !pair.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(Error::Invalid("invalid station hex octet"));
                }
                *b = u8::from_str_radix(pair, 16).expect("validated hex octet");
            }
            Self::new(net, &mac[..hex.len() / 2])
        } else {
            let b = station
                .parse()
                .map_err(|_| Error::Invalid("expected a decimal station 0–255 or 0xHEX"))?;
            Self::new(net, &[b])
        }
    }
}

pub(super) fn unicast(peer: SocketAddrV4) -> bool {
    peer.port() != 0
        && !peer.ip().is_unspecified()
        && !peer.ip().is_broadcast()
        && !peer.ip().is_multicast()
}

pub(super) fn connected(interface: Ipv4Addr, peer: SocketAddrV4) -> Result<UdpSocket, Error> {
    if !unicast(peer) {
        return Err(Error::Invalid(
            "a unicast IPv4 peer and nonzero UDP port are required",
        ));
    }
    let socket = UdpSocket::bind((interface, 0))?;
    socket.connect(peer)?;
    Ok(socket)
}

pub(super) fn recv(
    socket: &UdpSocket,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<Option<usize>, Error> {
    let Ok(wait) = remaining(deadline) else {
        return Ok(None);
    };
    socket.set_read_timeout(Some(wait))?;
    match socket.recv(buf) {
        Ok(n) => Ok(Some(n)),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

pub(super) struct DirectedSocket {
    udp: UdpSocket,
    interface: Ipv4Addr,
    hop: SocketAddrV4,
    station: Option<RemoteStation>,
}
impl DirectedSocket {
    pub fn open(
        interface: Ipv4Addr,
        hop: SocketAddrV4,
        station: Option<RemoteStation>,
    ) -> Result<Self, Error> {
        Ok(Self {
            udp: connected(interface, hop)?,
            interface,
            hop,
            station,
        })
    }
    pub fn reopen(&self) -> Result<Self, Error> {
        Self::open(self.interface, self.hop, self.station)
    }
    pub fn send(&self, deadline: Instant, local_frame: &[u8]) -> Result<(), Error> {
        self.udp.set_write_timeout(Some(remaining(deadline)?))?;
        if let Some(station) = self.station {
            let mut npdu = Npdu::apdu(&local_frame[6..]);
            npdu.expecting_reply = local_frame[5] & 4 != 0;
            npdu.destination = Some(station.address());
            let mut buf = [0; MAX_DATAGRAM];
            let len = 4 + npdu
                .encode_into(&mut buf[4..])
                .map_err(|_| Error::Invalid("routed NPDU exceeds link capacity"))?;
            let [hi, lo] = (len as u16).to_be_bytes();
            buf[..4].copy_from_slice(&[0x81, 10, hi, lo]);
            self.udp.send(&buf[..len])?;
        } else {
            self.udp.send(local_frame)?;
        }
        Ok(())
    }

    pub fn receive<'a>(
        &self,
        buf: &'a mut [u8; MAX_DATAGRAM + 1],
        deadline: Instant,
    ) -> Result<Option<&'a [u8]>, Error> {
        let Some(n) = recv(&self.udp, buf, deadline)? else {
            return Ok(None);
        };
        if n > MAX_DATAGRAM {
            return Ok(None);
        }
        let Some(station) = self.station else {
            return Ok(match wire::local_payload(&buf[..n]) {
                Ok((apdu, false)) => Some(apdu),
                _ => None,
            });
        };
        let Ok(Bvll::OriginalUnicastNpdu(raw)) = codec::decode(&buf[..n]) else {
            return Ok(None);
        };
        let Ok(npdu) = Npdu::decode(raw) else {
            return Ok(None);
        };
        Ok((npdu.message_type.is_none()
            && npdu.destination.is_none()
            && npdu.source == Some(station.address())
            && npdu.payload.len() <= wire::MAX_FRAME - 6)
            .then_some(npdu.payload))
    }
}
