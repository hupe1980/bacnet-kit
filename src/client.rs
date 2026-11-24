//! Blocking BACnet/IP reads over a direct or routed link. Each [`Client`] owns
//! its UDP socket and correlates responses with the outstanding request.
mod capabilities;
pub use capabilities::{Capabilities, CapabilityBits};
mod foreign;
pub use foreign::{Announcement, ForeignEvent, ForeignListener, RegistrationEvent};
mod link;
use link::DirectedSocket;
pub use link::RemoteStation;
mod segments;
mod watch;
use crate::{ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest, wire};
use bacnet_encoding::{
    apdu::header::ApduHeader,
    primitives::{ApplicationValueRef as Value, decode_application_value_ref},
};
use bacnet_types::enums::{AbortReason, ConfirmedServiceChoice as Service, RejectReason};
pub use segments::MAX_SEGMENTS;
use std::{
    fmt, io,
    net::{Ipv4Addr, SocketAddrV4},
    time::{Duration, Instant},
};
pub use watch::{Watch, WatchEvent, WatchOptions, WatchTrigger};

/// Maximum number of entries enumerated by one inventory operation.
pub const MAX_OBJECTS: usize = 128;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Timeout,
    Invalid(&'static str),
    TooManyObjects(u64),
    Remote { class: u32, code: u32 },
    Rejected(RejectReason),
    Aborted(AbortReason),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Timeout => f.write_str("peer did not complete the read before the deadline"),
            Self::Invalid(msg) => f.write_str(msg),
            Self::TooManyObjects(n) => {
                write!(f, "peer lists {n} objects (inventory limit {MAX_OBJECTS})")
            }
            Self::Remote { class, code } => {
                write!(f, "remote BACnet error (class {class}, code {code})")
            }
            Self::Rejected(r) => write!(f, "peer rejected the request ({r})"),
            Self::Aborted(r) => write!(f, "peer aborted the request ({r})"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::Timeout,
            _ => Self::Io(e),
        }
    }
}

/// A fully correlated reply with owned, application-tagged value octets.
#[derive(Debug)]
pub struct Reading {
    pub object: ObjectIdentifier,
    pub value: Vec<u8>,
}
impl Reading {
    /// Decode a scalar and require full consumption. Arrays retain their raw value octets.
    pub fn scalar(&self) -> Result<Value<'_>, Error> {
        let (v, n) = decode_application_value_ref(&self.value, 0)
            .map_err(|_| Error::Invalid("invalid property value"))?;
        if n != self.value.len() {
            return Err(Error::Invalid("expected exactly one property value"));
        }
        Ok(v)
    }
}

/// Four flags in BACnet wire order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusFlags {
    pub in_alarm: bool,
    pub fault: bool,
    pub overridden: bool,
    pub out_of_service: bool,
}
impl StatusFlags {
    /// Interpret the four high bits of a BACnet Status_Flags data octet.
    pub fn from_bits(b: u8) -> Self {
        Self {
            in_alarm: b & 128 != 0,
            fault: b & 64 != 0,
            overridden: b & 32 != 0,
            out_of_service: b & 16 != 0,
        }
    }
}
/// Point attributes are independent reads. Observation time refers to receipt of Present_Value.
#[derive(Debug, Clone)]
pub struct Sample {
    pub value: f32,
    pub unit: u32,
    pub flags: StatusFlags,
    pub observed_at: Instant,
}
impl Sample {
    pub fn is_stale(&self, now: Instant, max_age: Duration) -> bool {
        now.saturating_duration_since(self.observed_at) >= max_age
    }
}
#[derive(Debug)]
pub struct Inventory {
    pub device: ObjectIdentifier,
    pub objects: Vec<ObjectIdentifier>,
}

/// One requested occurrence, including its own protocol error when the access failed.
#[derive(Debug)]
pub struct BatchResult {
    pub reference: ReadPropertyRequest,
    pub outcome: Result<Reading, PropertyError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyError {
    pub class: u32,
    pub code: u32,
}

/// Effective transmit limits after accounting for peer capacity and the local read profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerLimits {
    /// Maximum request APDU size in octets, capped at 1,476 on this link.
    pub max_apdu: u16,
    /// Zero for an unsegmented receiver, otherwise at most [`MAX_SEGMENTS`].
    pub max_request_segments: u8,
}

pub struct Client {
    socket: DirectedSocket,
    timeout: Duration,
    next: u16,
    limits: PeerLimits,
}
impl Client {
    /// Connect an ephemeral local socket to one commissioned IPv4 endpoint.
    pub fn connect(peer: SocketAddrV4, timeout: Duration) -> Result<Self, Error> {
        Self::bind(Ipv4Addr::UNSPECIFIED, peer, timeout)
    }
    /// Choose the outgoing interface explicitly on a multihomed host.
    pub fn bind(interface: Ipv4Addr, peer: SocketAddrV4, timeout: Duration) -> Result<Self, Error> {
        Self::open(interface, peer, None, timeout)
    }
    /// Read a commissioned station behind a configured IPv4 router.
    ///
    /// ```no_run
    /// use bacnet_kit::client::Client;
    /// use std::{net::Ipv4Addr, time::Duration};
    /// let router = "127.0.0.1:47808".parse()?;
    /// let mut reader = Client::via(Ipv4Addr::UNSPECIFIED, router, "200:0x2a".parse()?, Duration::from_secs(3))?;
    /// let inventory = reader.inventory_of(77)?;
    /// let sample = reader.sample(1)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn via(
        interface: Ipv4Addr,
        router: SocketAddrV4,
        station: RemoteStation,
        timeout: Duration,
    ) -> Result<Self, Error> {
        Self::open(interface, router, Some(station), timeout)
    }
    fn open(
        interface: Ipv4Addr,
        peer: SocketAddrV4,
        station: Option<RemoteStation>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        if timeout.is_zero() || Instant::now().checked_add(timeout).is_none() {
            return Err(Error::Invalid("invalid read timeout"));
        }
        Ok(Self {
            socket: DirectedSocket::open(interface, peer, station)?,
            timeout,
            next: 0,
            limits: PeerLimits {
                max_apdu: 1476,
                max_request_segments: 0,
            },
        })
    }
    fn deadline(&self) -> Result<Instant, Error> {
        Instant::now()
            .checked_add(self.timeout)
            .ok_or(Error::Invalid("deadline out of range"))
    }
    fn read_until(
        &mut self,
        object: ObjectIdentifier,
        property: Prop,
        index: Option<u32>,
        deadline: Instant,
    ) -> Result<Reading, Error> {
        let req = ReadPropertyRequest {
            object_identifier: object,
            property_identifier: property,
            property_array_index: index,
        };
        self.exchange(
            deadline,
            Service::READ_PROPERTY,
            |invoke, out| wire::encode_read(invoke, &req, out),
            |body| single_ack(body, &req),
        )
    }

    fn exchange<T>(
        &mut self,
        deadline: Instant,
        service: Service,
        encode: impl FnOnce(u8, &mut [u8]) -> Result<usize, wire::WireError>,
        decode: impl Fn(&[u8]) -> Option<T>,
    ) -> Result<T, Error> {
        if self.next > 255 {
            self.socket = self.socket.reopen()?;
            self.next = 0;
        }
        let invoke = self.next as u8;
        self.next += 1;
        let mut packet = [0; wire::MAX_FRAME + 1];
        let n = encode(invoke, &mut packet)
            .map_err(|_| Error::Invalid("cannot encode property request"))?;
        segments::exchange(
            &self.socket,
            deadline,
            self.limits,
            &mut packet[..n],
            invoke,
            service,
            decode,
        )
    }

    /// Learn the connected Device's receive limits for subsequent reads on this client.
    /// All probes share one deadline. A failed refresh leaves the previous limits in place.
    pub fn negotiate(&mut self, device_instance: u32) -> Result<PeerLimits, Error> {
        let deadline = self.deadline()?;
        let device = ObjectIdentifier::new_addressable(ObjectType::DEVICE, device_instance)
            .map_err(|_| Error::Invalid("expected a concrete Device instance"))?;
        let maximum = self.read_until(device, Prop::MAX_APDU_LENGTH_ACCEPTED, None, deadline)?;
        let Value::Unsigned(max_apdu) = maximum.scalar()? else {
            return Err(Error::Invalid("expected unsigned Max_APDU_Length_Accepted"));
        };
        let max_apdu = u16::try_from(max_apdu)
            .ok()
            .filter(|n| *n >= 50)
            .ok_or(Error::Invalid("invalid Max_APDU_Length_Accepted"))?
            .min(1476);
        let direction = self.read_until(device, Prop::SEGMENTATION_SUPPORTED, None, deadline)?;
        let max_request_segments = match direction.scalar()? {
            Value::Enumerated(0 | 2) => {
                match self.read_until(device, Prop::MAX_SEGMENTS_ACCEPTED, None, deadline) {
                    Ok(count) => match count.scalar()? {
                        Value::Unsigned(n) if (2..=u32::MAX.into()).contains(&n) => {
                            n.min(MAX_SEGMENTS.into()) as u8
                        }
                        _ => return Err(Error::Invalid("invalid Max_Segments_Accepted")),
                    },
                    // A receiving peer may lack this property. Clause 5.4.4.1 permits
                    // transmission with an unknown count, within the local segment budget.
                    Err(Error::Remote { class: 2, code: 32 }) => MAX_SEGMENTS,
                    Err(error) => return Err(error),
                }
            }
            Value::Enumerated(1 | 3) => 0,
            _ => return Err(Error::Invalid("invalid Segmentation_Supported")),
        };
        self.limits = PeerLimits {
            max_apdu,
            max_request_segments,
        };
        Ok(self.limits)
    }
    /// Read a single property or selected array element.
    pub fn read(
        &mut self,
        object: ObjectIdentifier,
        property: Prop,
        index: Option<u32>,
    ) -> Result<Reading, Error> {
        self.read_until(object, property, index, self.deadline()?)
    }
    /// Read 1–32 explicit references in one transaction. Duplicate references retain their order.
    /// A per-property access error leaves the other results available.
    ///
    /// ```no_run
    /// use bacnet_kit::{client::Client, ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest};
    /// use std::time::Duration;
    /// let mut client = Client::connect("127.0.0.1:47808".parse()?, Duration::from_secs(3))?;
    /// let input = ObjectIdentifier::new(ObjectType::ANALOG_INPUT, 1)?;
    /// let refs = [Prop::PRESENT_VALUE, Prop::DESCRIPTION].map(|property_identifier| ReadPropertyRequest {
    ///     object_identifier: input, property_identifier, property_array_index: None,
    /// });
    /// for row in client.read_multiple(&refs)? {
    ///     match row.outcome {
    ///         Ok(value) => println!("{:?}", value.scalar()?),
    ///         Err(error) => eprintln!("{}: class {}, code {}", row.reference.property_identifier, error.class, error.code),
    ///     }
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_multiple(
        &mut self,
        refs: &[ReadPropertyRequest],
    ) -> Result<Vec<BatchResult>, Error> {
        if refs.is_empty() || refs.len() > crate::batch::MAX_BATCH {
            return Err(Error::Invalid("a batch needs 1–32 property references"));
        }
        if refs.iter().any(|r| {
            r.object_identifier.instance_number() == ObjectIdentifier::WILDCARD_INSTANCE
                || crate::batch::selector(r.property_identifier)
        }) {
            return Err(Error::Invalid(
                "batch reads require concrete objects and explicit properties",
            ));
        }
        self.exchange(
            self.deadline()?,
            Service::READ_PROPERTY_MULTIPLE,
            |invoke, out| encode_batch(invoke, refs, out),
            |body| batch_ack(body, refs),
        )
    }

    /// Resolve a device identity and enumerate its Object_List under one deadline.
    pub fn inventory(&mut self) -> Result<Inventory, Error> {
        let wildcard =
            ObjectIdentifier::new(ObjectType::DEVICE, ObjectIdentifier::WILDCARD_INSTANCE)
                .expect("valid wildcard");
        self.inventory_from(wildcard)
    }

    /// Verify a known Device instance and enumerate its Object_List under one deadline.
    pub fn inventory_of(&mut self, instance: u32) -> Result<Inventory, Error> {
        let device = ObjectIdentifier::new_addressable(ObjectType::DEVICE, instance)
            .map_err(|_| Error::Invalid("expected a concrete Device instance"))?;
        self.inventory_from(device)
    }

    fn inventory_from(&mut self, target: ObjectIdentifier) -> Result<Inventory, Error> {
        let deadline = self.deadline()?;
        let identity = self.read_until(target, Prop::OBJECT_IDENTIFIER, None, deadline)?;
        let device = match identity.scalar()? {
            Value::ObjectIdentifier(id) if id == identity.object => id,
            _ => return Err(Error::Invalid("inconsistent Device identity")),
        };
        let count = self.read_until(device, Prop::OBJECT_LIST, Some(0), deadline)?;
        let count = match count.scalar()? {
            Value::Unsigned(n) if n > MAX_OBJECTS as u64 => return Err(Error::TooManyObjects(n)),
            Value::Unsigned(n) if n > 0 => n as u32,
            _ => return Err(Error::Invalid("invalid Object_List length")),
        };
        let mut objects = Vec::with_capacity(count as usize);
        for i in 1..=count {
            let item = self.read_until(device, Prop::OBJECT_LIST, Some(i), deadline)?;
            match item.scalar()? {
                Value::ObjectIdentifier(id)
                    if id.instance_number() != ObjectIdentifier::WILDCARD_INSTANCE =>
                {
                    objects.push(id)
                }
                _ => return Err(Error::Invalid("invalid Object_List member")),
            };
        }
        Ok(Inventory { device, objects })
    }
    /// Collect the value, units and four status flags for one Analog Input.
    pub fn sample(&mut self, instance: u32) -> Result<Sample, Error> {
        let id = ObjectIdentifier::new_addressable(ObjectType::ANALOG_INPUT, instance)
            .map_err(|_| Error::Invalid("invalid Analog Input instance"))?;
        self.sample_until(id, self.deadline()?)
    }

    fn sample_until(&mut self, id: ObjectIdentifier, deadline: Instant) -> Result<Sample, Error> {
        let reading = self.read_until(id, Prop::PRESENT_VALUE, None, deadline)?;
        let observed_at = Instant::now();
        let Value::Real(value) = reading.scalar()? else {
            return Err(Error::Invalid("expected a REAL Present_Value"));
        };
        let units = self.read_until(id, Prop::UNITS, None, deadline)?;
        let Value::Enumerated(unit) = units.scalar()? else {
            return Err(Error::Invalid("expected enumerated Units"));
        };
        let status = self.read_until(id, Prop::STATUS_FLAGS, None, deadline)?;
        let Value::BitString {
            unused_bits: 4,
            data: [b],
        } = status.scalar()?
        else {
            return Err(Error::Invalid("expected four Status_Flags"));
        };
        Ok(Sample {
            value,
            observed_at,
            unit,
            flags: StatusFlags::from_bits(*b),
        })
    }
}
fn remaining(deadline: Instant) -> Result<Duration, Error> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(Error::Timeout)
}

pub(crate) fn reply<T>(
    apdu: &[u8],
    invoke: u8,
    service: Service,
    decode: &impl Fn(&[u8]) -> Option<T>,
) -> Option<Result<T, Error>> {
    let (h, body) = ApduHeader::decode(apdu).ok()?;
    match h {
        ApduHeader::ComplexAck {
            invoke_id,
            service_choice,
        } if invoke_id == invoke && service_choice == service => decode(body).map(Ok),
        ApduHeader::Error {
            invoke_id,
            service_choice,
        } if invoke_id == invoke && service_choice == service => {
            let (Value::Enumerated(class), offset) = decode_application_value_ref(body, 0).ok()?
            else {
                return None;
            };
            let (Value::Enumerated(code), end) = decode_application_value_ref(body, offset).ok()?
            else {
                return None;
            };
            (end == body.len()).then_some(Err(Error::Remote { class, code }))
        }
        ApduHeader::Reject { invoke_id, reason } if invoke_id == invoke && body.is_empty() => {
            Some(Err(Error::Rejected(reason)))
        }
        ApduHeader::Abort {
            server: true,
            invoke_id,
            reason,
        } if invoke_id == invoke && body.is_empty() => Some(Err(Error::Aborted(reason))),
        _ => None,
    }
}

pub(crate) fn single_ack(body: &[u8], r: &ReadPropertyRequest) -> Option<Reading> {
    let ack = wire::ReadPropertyAckRef::decode(body).ok()?;
    let alias = r.object_identifier.object_type() == ObjectType::DEVICE
        && r.object_identifier.instance_number() == ObjectIdentifier::WILDCARD_INSTANCE
        && ack.object_identifier.object_type() == ObjectType::DEVICE;
    if ack.object_identifier.instance_number() == ObjectIdentifier::WILDCARD_INSTANCE
        || (!alias && r.object_identifier != ack.object_identifier)
        || r.property_identifier != ack.property_identifier
        || r.property_array_index != ack.property_array_index
    {
        return None;
    }
    Some(Reading {
        object: ack.object_identifier,
        value: ack.property_value.to_vec(),
    })
}

fn groups(refs: &[ReadPropertyRequest]) -> impl Iterator<Item = &[ReadPropertyRequest]> {
    refs.chunk_by(|a, b| a.object_identifier == b.object_identifier)
}

fn encode_batch(
    invoke: u8,
    refs: &[ReadPropertyRequest],
    out: &mut [u8],
) -> Result<usize, wire::WireError> {
    use bacnet_encoding::{
        primitives::encode_ctx_object_id_to_sink,
        tags::{encode_closing_tag_to_sink, encode_opening_tag_to_sink},
    };
    // Object (5), opening/closing (2), property (5), optional index (5).
    const CAP: usize = 17 * crate::batch::MAX_BATCH;
    let mut body = [0; CAP];
    let mut tail = &mut body[..];
    for group in groups(refs) {
        encode_ctx_object_id_to_sink(&mut tail, 0, &group[0].object_identifier)?;
        encode_opening_tag_to_sink(&mut tail, 1)?;
        for r in group {
            crate::batch::put_reference(
                &mut tail,
                0,
                r.property_identifier,
                r.property_array_index,
            )?;
        }
        encode_closing_tag_to_sink(&mut tail, 1)?;
    }
    let len = CAP - tail.len();
    wire::confirmed_frame(invoke, Service::READ_PROPERTY_MULTIPLE, &body[..len], out)
}

fn batch_ack(body: &[u8], refs: &[ReadPropertyRequest]) -> Option<Vec<BatchResult>> {
    use bacnet_encoding::{
        constructed::{
            PropertyValueBoundary, extract_property_value, tagged::decode_app_enumerated,
        },
        tags::decode_tag,
    };
    let mut cursor = crate::batch::Cursor::new(body);
    let mut rows = Vec::with_capacity(refs.len());
    for group in groups(refs) {
        let object = cursor.object().ok()?;
        if object != group[0].object_identifier {
            return None;
        }
        for r in group {
            let (p, i) = cursor.reference(2).ok()?;
            if (p, i) != (r.property_identifier, r.property_array_index) {
                return None;
            }
            let (tag, start) = decode_tag(body, cursor.pos).ok()?;
            let outcome = if tag.is_opening_tag(4) {
                let (raw, end) = extract_property_value(
                    body,
                    start,
                    4,
                    p,
                    &[
                        PropertyValueBoundary::Context(2),
                        PropertyValueBoundary::Closing(1),
                    ],
                )
                .ok()?;
                cursor.pos = end;
                Ok(Reading {
                    object,
                    value: raw.to_vec(),
                })
            } else if tag.is_opening_tag(5) {
                let (class, end) =
                    decode_app_enumerated::<u32>(body, start, "RPM error class").ok()?;
                let (code, end) = decode_app_enumerated::<u32>(body, end, "RPM error code").ok()?;
                cursor.pos = end;
                cursor.close(5).ok()?;
                Err(PropertyError { class, code })
            } else {
                return None;
            };
            rows.push(BatchResult {
                reference: r.clone(),
                outcome,
            });
        }
        cursor.close(1).ok()?;
    }
    (cursor.pos == body.len()).then_some(rows)
}
