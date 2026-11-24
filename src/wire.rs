//! Borrowed unsegmented ReadProperty messages on a local IPv4 link.
use crate::ReadPropertyRequest;
use bacnet_bip_core::codec::Bvll;
use bacnet_encoding::apdu::header::{ApduHeader, ConfirmedRequestHeader, MaxApdu, MaxSegments};
use bacnet_npdu::Npdu;
pub use bacnet_services::read_property::ReadPropertyAckRef;
use bacnet_types::enums::ConfirmedServiceChoice;

/// Maximum complete IPv4 datagram payload used by this local, unsegmented profile.
pub const MAX_FRAME: usize = 1482;
/// Wire buffers are supplied by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    InvalidFrame,
    WrongLink,
    OutputTooSmall,
}
impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::InvalidFrame => "invalid BACnet frame",
            Self::WrongLink => "unsupported BACnet link or route",
            Self::OutputTooSmall => "output buffer too small",
        })
    }
}
impl core::error::Error for WireError {}
impl From<bacnet_encoding::sink::SinkFull> for WireError {
    fn from(_: bacnet_encoding::sink::SinkFull) -> Self {
        Self::OutputTooSmall
    }
}

pub(crate) fn local_payload(bytes: &[u8]) -> Result<(&[u8], bool), WireError> {
    if bytes.len() > MAX_FRAME {
        return Err(WireError::InvalidFrame);
    }
    let (npdu, group) =
        match bacnet_bip_core::codec::decode(bytes).map_err(|_| WireError::InvalidFrame)? {
            Bvll::OriginalUnicastNpdu(p) => (p, false),
            Bvll::OriginalBroadcastNpdu(p) => (p, true),
            _ => return Err(WireError::WrongLink),
        };
    let packet = Npdu::decode(npdu).map_err(|_| WireError::InvalidFrame)?;
    if packet.message_type.is_some() || packet.source.is_some() {
        return Err(WireError::WrongLink);
    }
    let global = packet
        .destination
        .is_some_and(|d| d.network == bacnet_npdu::GLOBAL_NETWORK && d.mac.is_empty());
    if packet.destination.is_some() && !global {
        return Err(WireError::WrongLink);
    }
    Ok((packet.payload, group || global))
}

// Local Original-Unicast-NPDU has a four-byte BVLC and two-byte NPCI.
// Reserve the prefix and encode directly into the caller's buffer.
pub(crate) fn frame(header: ApduHeader, body: &[u8], out: &mut [u8]) -> Result<usize, WireError> {
    let mut tail = out.get_mut(6..).ok_or(WireError::OutputTooSmall)?;
    let n = header
        .encode_with_parameters_to_sink(&mut tail, body)
        .map_err(|_| WireError::OutputTooSmall)?
        + 6;
    if n > MAX_FRAME {
        return Err(WireError::OutputTooSmall);
    }
    let [hi, lo] = (n as u16).to_be_bytes();
    let control = if matches!(header, ApduHeader::ConfirmedRequest(_)) {
        4
    } else {
        0
    };
    out[..6].copy_from_slice(&[0x81, 0x0a, hi, lo, 1, control]);
    Ok(n)
}

// The upstream header codec omits the three-octet SimpleACK.
pub(crate) fn simple_ack(invoke: u8, service: ConfirmedServiceChoice) -> [u8; 9] {
    [0x81, 10, 0, 9, 1, 0, 0x20, invoke, service.to_raw()]
}

/// Encode one directed read. An absent index reads the whole property, zero reads array length.
pub fn encode_read(
    invoke_id: u8,
    request: &ReadPropertyRequest,
    out: &mut [u8],
) -> Result<usize, WireError> {
    let mut body = [0; 16];
    let n = request
        .encode_to_sink(&mut &mut body[..])
        .map_err(|_| WireError::OutputTooSmall)?;
    confirmed_frame(
        invoke_id,
        ConfirmedServiceChoice::READ_PROPERTY,
        &body[..n],
        out,
    )
}

pub(crate) fn confirmed_frame(
    invoke_id: u8,
    service_choice: ConfirmedServiceChoice,
    body: &[u8],
    out: &mut [u8],
) -> Result<usize, WireError> {
    let header = ApduHeader::ConfirmedRequest(ConfirmedRequestHeader {
        invoke_id,
        segmented_response_accepted: false,
        max_segments: MaxSegments::UNSPECIFIED,
        max_apdu: MaxApdu::OCTETS_1476,
        service_choice,
    });
    frame(header, body, out)
}

pub(crate) fn service_error(
    h: ConfirmedRequestHeader,
    class: bacnet_types::enums::ErrorClass,
    code: bacnet_types::enums::ErrorCode,
    out: &mut [u8],
) -> Result<usize, WireError> {
    use bacnet_encoding::primitives::encode_app_enumerated_to_sink;
    let mut error = [0; 12];
    let mut sink = &mut error[..];
    let a = encode_app_enumerated_to_sink(&mut sink, class.to_raw().into())
        .map_err(|_| WireError::OutputTooSmall)?;
    let b = encode_app_enumerated_to_sink(&mut sink, code.to_raw().into())
        .map_err(|_| WireError::OutputTooSmall)?;
    frame(
        ApduHeader::Error {
            invoke_id: h.invoke_id,
            service_choice: h.service_choice,
        },
        &error[..a + b],
        out,
    )
}
