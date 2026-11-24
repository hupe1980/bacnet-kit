//! Inspect a single captured BACnet MS/TP frame without IO or allocation.
pub use bacnet_mstp_codec::{FrameError, FrameHeader, FrameType};
use bacnet_mstp_codec::{
    MAX_EXTENDED_FRAME_LENGTH, MAX_EXTENDED_MPDU_DATA, MAX_STANDARD_MPDU_DATA,
    decode_frame_in_place,
};
use core::fmt;

/// Largest supported extended frame including one optional FF pad octet.
pub const MAX_RECORD: usize = MAX_EXTENDED_FRAME_LENGTH + 1;

/// Metadata from one integrity-checked record. Addresses are the octets observed on the link.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Observation {
    pub header: FrameHeader,
    /// Application-independent data length after link decoding.
    pub data_octets: usize,
    /// Complete captured length, including the optional pad.
    pub wire_octets: usize,
    pub padded: bool,
}

/// Reason the capture could not yield one supported observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InspectError {
    TooLarge,
    Wire(FrameError),
    Unsupported(FrameType),
    InvalidDataLength { kind: FrameType, octets: usize },
    TrailingBytes,
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(
                f,
                "one MS/TP record may contain at most {MAX_RECORD} octets"
            ),
            Self::Wire(FrameError::InvalidPreamble { .. }) => {
                f.write_str("expected the MS/TP preamble 55 FF")
            }
            Self::Wire(err) => err.fmt(f),
            Self::Unsupported(kind) => write!(f, "unsupported MS/TP frame type {}", kind.to_raw()),
            Self::InvalidDataLength { kind, octets } => {
                write!(f, "{kind} cannot carry {octets} data octets")
            }
            Self::TrailingBytes => {
                f.write_str("expected one frame and at most one trailing FF pad")
            }
        }
    }
}
impl core::error::Error for InspectError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Wire(err) => Some(err),
            _ => None,
        }
    }
}

/// Inspect a complete record starting at its preamble. Standard types 0–7 and
/// extended BACnet data types 32/33 are supported. Extended decoding may
/// overwrite caller scratch even if a later record-level check fails.
///
/// ```
/// use bacnet_kit::mstp::{FrameType, inspect};
/// let mut token = [0x55, 0xff, 0, 1, 2, 0, 0, 0x40];
/// let observed = inspect(&mut token)?;
/// assert_eq!(observed.header.frame_type, FrameType::TOKEN);
/// assert_eq!((observed.header.source, observed.header.destination), (2, 1));
/// # Ok::<(), bacnet_kit::mstp::InspectError>(())
/// ```
pub fn inspect(record: &mut [u8]) -> Result<Observation, InspectError> {
    let wire_octets = record.len();
    if wire_octets > MAX_RECORD {
        return Err(InspectError::TooLarge);
    }
    let (frame, used) = decode_frame_in_place(record).map_err(|e| match e {
        FrameError::UnsupportedCobsFrame { raw_type } => {
            InspectError::Unsupported(FrameType::from_raw(raw_type))
        }
        other => InspectError::Wire(other),
    })?;
    let header = frame.header;
    let kind = header.frame_type;
    let data_octets = frame.data.len();
    if !kind.is_defined() {
        return Err(InspectError::Unsupported(kind));
    }
    if (!kind.allows_data() && data_octets != 0)
        || (kind.is_extended_data()
            && !(MAX_STANDARD_MPDU_DATA + 1..=MAX_EXTENDED_MPDU_DATA).contains(&data_octets))
    {
        return Err(InspectError::InvalidDataLength {
            kind,
            octets: data_octets,
        });
    }
    let padded = match &record[used..] {
        [] => false,
        [0xff] => true,
        _ => return Err(InspectError::TrailingBytes),
    };
    Ok(Observation {
        header,
        data_octets,
        wire_octets,
        padded,
    })
}
