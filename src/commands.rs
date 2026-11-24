//! Supervised commands for the fixture's simulated Analog Value 1.
use crate::wire::{self, WireError};
use crate::{ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest};
use bacnet_encoding::{
    apdu::header::ConfirmedRequestHeader,
    constructed::tagged::{
        decode_ctx_object_id, decode_ctx_unsigned, decode_optional_ctx, expect_closing,
        expect_opening,
    },
    primitives::*,
};
use bacnet_types::enums::{ConfirmedServiceChoice as Service, ErrorClass, ErrorCode, RejectReason};

/// One validated slot change. An empty value relinquishes the slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Command {
    priority: u8,
    value: Option<f32>,
}

impl Command {
    /// Create a finite setpoint or a relinquish at an externally writable priority.
    pub fn new(priority: u8, value: Option<f32>) -> Result<Self, ErrorCode> {
        if !(1..=16).contains(&priority) || value.is_some_and(|v| !v.is_finite()) {
            return Err(ErrorCode::VALUE_OUT_OF_RANGE);
        }
        if priority == 6 {
            return Err(ErrorCode::WRITE_ACCESS_DENIED);
        }
        Ok(Self { priority, value })
    }

    pub fn priority(self) -> u8 {
        self.priority
    }
    pub fn value(self) -> Option<f32> {
        self.value
    }

    /// Encode a directed WriteProperty for the simulator's Analog Value 1.
    pub fn encode(self, invoke: u8, out: &mut [u8]) -> Result<usize, WireError> {
        let req = ReadPropertyRequest {
            object_identifier: analog_value(),
            property_identifier: Prop::PRESENT_VALUE,
            property_array_index: None,
        };
        const BODY_CAPACITY: usize = 32;
        let mut body = [0; BODY_CAPACITY];
        let mut sink = &mut body[..];
        req.encode_to_sink(&mut sink)?;
        bacnet_encoding::tags::encode_opening_tag_to_sink(&mut sink, 3)?;
        slot_value(&mut sink, self.value)?;
        bacnet_encoding::tags::encode_closing_tag_to_sink(&mut sink, 3)?;
        encode_ctx_unsigned_to_sink(&mut sink, 4, self.priority.into())?;
        let n = BODY_CAPACITY - sink.len();
        wire::confirmed_frame(invoke, Service::WRITE_PROPERTY, &body[..n], out)
    }
}

pub(crate) fn analog_value() -> ObjectIdentifier {
    ObjectIdentifier::new(ObjectType::ANALOG_VALUE, 1).expect("fixed Analog Value identifier")
}

fn slot_value(
    out: &mut &mut [u8],
    value: Option<f32>,
) -> Result<usize, bacnet_encoding::sink::SinkFull> {
    match value {
        Some(v) => encode_app_real_to_sink(out, v),
        None => encode_app_null_to_sink(out),
    }
}

/// Caller-owned volatile command state. Each new fixture starts at 20 °C.
#[derive(Default)]
pub struct CommandSlots {
    slots: [Option<f32>; 16],
}

impl CommandSlots {
    const RELINQUISH_DEFAULT: f32 = 20.;
    fn effective(&self) -> f32 {
        self.slots
            .iter()
            .flatten()
            .next()
            .copied()
            .unwrap_or(Self::RELINQUISH_DEFAULT)
    }

    pub(crate) fn read(
        &self,
        req: &ReadPropertyRequest,
        mut out: &mut [u8],
    ) -> Result<usize, (ErrorClass, ErrorCode)> {
        let property = |code| (ErrorClass::PROPERTY, code);
        let value = if req.property_identifier == Prop::PRIORITY_ARRAY {
            match req.property_array_index {
                Some(0) => encode_app_unsigned_to_sink(&mut out, 16),
                Some(i @ 1..=16) => slot_value(&mut out, self.slots[(i - 1) as usize]),
                Some(_) => return Err(property(ErrorCode::INVALID_ARRAY_INDEX)),
                None => self
                    .slots
                    .iter()
                    .try_fold(0, |n, v| slot_value(&mut out, *v).map(|m| n + m)),
            }
        } else {
            if req.property_array_index.is_some() {
                return Err(property(ErrorCode::PROPERTY_IS_NOT_AN_ARRAY));
            }
            match req.property_identifier {
                Prop::OBJECT_IDENTIFIER => {
                    encode_app_object_id_to_sink(&mut out, &req.object_identifier)
                }
                Prop::OBJECT_NAME => {
                    encode_app_character_string_to_sink(&mut out, "Simulated setpoint")
                }
                Prop::OBJECT_TYPE => {
                    encode_app_enumerated_to_sink(&mut out, ObjectType::ANALOG_VALUE.to_raw())
                }
                Prop::PRESENT_VALUE => encode_app_real_to_sink(&mut out, self.effective()),
                Prop::RELINQUISH_DEFAULT => {
                    encode_app_real_to_sink(&mut out, Self::RELINQUISH_DEFAULT)
                }
                Prop::UNITS => encode_app_enumerated_to_sink(&mut out, 62),
                Prop::STATUS_FLAGS => encode_app_bit_string_to_sink(&mut out, 4, &[0]),
                _ => return Err(property(ErrorCode::UNKNOWN_PROPERTY)),
            }
        };
        value.map_err(|_| (ErrorClass::RESOURCES, ErrorCode::NO_SPACE_TO_WRITE_PROPERTY))
    }

    pub(crate) fn write(
        &mut self,
        h: ConfirmedRequestHeader,
        body: &[u8],
        out: &mut [u8],
        allow: impl FnOnce(Command) -> bool,
    ) -> Result<usize, WireError> {
        let cmd = match decode_command(body) {
            Ok(cmd) if allow(cmd) => cmd,
            Ok(_) => {
                return wire::service_error(
                    h,
                    ErrorClass::PROPERTY,
                    ErrorCode::WRITE_ACCESS_DENIED,
                    out,
                );
            }
            Err(WriteError::Property(class, code)) => {
                return wire::service_error(h, class, code, out);
            }
            Err(WriteError::Syntax) => {
                return wire::frame(
                    bacnet_encoding::apdu::header::ApduHeader::Reject {
                        invoke_id: h.invoke_id,
                        reason: RejectReason::INVALID_TAG,
                    },
                    &[],
                    out,
                );
            }
        };
        let ack = wire::simple_ack(h.invoke_id, Service::WRITE_PROPERTY);
        out.get_mut(..ack.len())
            .ok_or(WireError::OutputTooSmall)?
            .copy_from_slice(&ack);
        self.slots[usize::from(cmd.priority - 1)] = cmd.value;
        Ok(ack.len())
    }
}

enum WriteError {
    Syntax,
    Property(ErrorClass, ErrorCode),
}
impl From<bacnet_types::error::Error> for WriteError {
    fn from(_: bacnet_types::error::Error) -> Self {
        Self::Syntax
    }
}

fn decode_command(data: &[u8]) -> Result<Command, WriteError> {
    let label = "WriteProperty";
    let (id, pos) = decode_ctx_object_id(data, 0, 0, label)?;
    let (prop, pos) = decode_ctx_unsigned::<u32>(data, pos, 1, label)?;
    let (index, pos) = decode_optional_ctx(data, pos, 2, label, decode_ctx_unsigned::<u32>)?;
    let pos = expect_opening(data, pos, 3, label)?;
    let (value, pos) = decode_application_value_ref(data, pos)?;
    let pos = expect_closing(data, pos, 3, label)?;
    let (priority, end) = decode_optional_ctx(data, pos, 4, label, decode_ctx_unsigned::<u64>)?;
    if end != data.len() {
        return Err(WriteError::Syntax);
    }
    let property = |code| WriteError::Property(ErrorClass::PROPERTY, code);
    if id != analog_value() || Prop::from_raw(prop) != Prop::PRESENT_VALUE {
        return Err(property(ErrorCode::WRITE_ACCESS_DENIED));
    }
    if index.is_some() {
        return Err(property(ErrorCode::PROPERTY_IS_NOT_AN_ARRAY));
    }
    let value = match value {
        ApplicationValueRef::Null => None,
        ApplicationValueRef::Real(v) => Some(v),
        _ => return Err(property(ErrorCode::INVALID_DATA_TYPE)),
    };
    let priority = priority.unwrap_or(16);
    if !(1..=16).contains(&priority) {
        return Err(WriteError::Property(
            ErrorClass::SERVICES,
            ErrorCode::PARAMETER_OUT_OF_RANGE,
        ));
    }
    Command::new(priority as u8, value).map_err(property)
}
