//! A bounded simulator with a read-only default and optional caller-owned command state.
use crate::commands::{Command, CommandSlots, analog_value};
use crate::wire::{ReadPropertyAckRef, WireError, frame, local_payload};
use crate::{ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest};
use bacnet_encoding::{
    apdu::header::{ApduHeader, ConfirmedRequestHeader},
    primitives::*,
};
use bacnet_services::who_is::{IAmRequest, WhoIsRequest};
use bacnet_types::enums::{
    AbortReason, ConfirmedServiceChoice as Service, ErrorClass, ErrorCode, RejectReason,
    Segmentation, UnconfirmedServiceChoice as Unconfirmed,
};

// All array reads share BACnet's whole / count / one-based element semantics.
fn read_array<T>(
    mut values: impl Iterator<Item = T>,
    index: Option<u32>,
    mut sink: &mut [u8],
    mut encode: impl FnMut(&mut &mut [u8], T) -> Result<usize, bacnet_encoding::sink::SinkFull>,
) -> Result<usize, (ErrorClass, ErrorCode)> {
    match index {
        None => values.try_fold(0, |n, v| encode(&mut sink, v).map(|m| n + m)),
        Some(0) => encode_app_unsigned_to_sink(&mut sink, values.count() as u64),
        Some(i) => {
            let v = values
                .nth((i - 1) as usize)
                .ok_or((ErrorClass::PROPERTY, ErrorCode::INVALID_ARRAY_INDEX))?;
            encode(&mut sink, v)
        }
    }
    .map_err(|_| (ErrorClass::RESOURCES, ErrorCode::NO_SPACE_TO_WRITE_PROPERTY))
}

/// A simulated Device and a single Analog Input. Supply an application-owned vendor ID.
pub struct Device {
    id: ObjectIdentifier,
    input: ObjectIdentifier,
    vendor: u16,
    temperature: f32,
    fault: bool,
}
impl Device {
    /// Analog Input 1 reports degrees Celsius.
    pub fn new(
        instance: u32,
        vendor: u16,
        temperature: f32,
    ) -> Result<Self, bacnet_types::error::Error> {
        Ok(Self {
            id: ObjectIdentifier::new_addressable(ObjectType::DEVICE, instance)?,
            input: ObjectIdentifier::new_addressable(ObjectType::ANALOG_INPUT, 1)?,
            vendor,
            temperature,
            fault: false,
        })
    }
    /// Change the simulated fault indication while retaining the measured value.
    pub fn set_fault(&mut self, fault: bool) {
        self.fault = fault;
    }
    /// Inspect a local datagram. Send a returned response to its original UDP source.
    /// `Ok(None)` means there is nothing to send. A recognized confirmed request
    /// can produce a BACnet Error, Reject or Abort reply.
    pub fn respond(&self, bytes: &[u8], out: &mut [u8]) -> Result<Option<usize>, WireError> {
        self.dispatch(bytes, out, None, |_| false)
    }

    /// Serve an additional simulated Analog Value using caller-owned slots. The
    /// policy receives each validated change and may capture the UDP source.
    pub fn respond_with_commands(
        &self,
        bytes: &[u8],
        out: &mut [u8],
        slots: &mut CommandSlots,
        allow: impl FnOnce(Command) -> bool,
    ) -> Result<Option<usize>, WireError> {
        self.dispatch(bytes, out, Some(slots), allow)
    }

    fn dispatch(
        &self,
        bytes: &[u8],
        out: &mut [u8],
        slots: Option<&mut CommandSlots>,
        allow: impl FnOnce(Command) -> bool,
    ) -> Result<Option<usize>, WireError> {
        let Ok((apdu, group)) = local_payload(bytes) else {
            return Ok(None);
        };
        if !group && apdu.len() >= 6 && apdu[0] >> 4 == 0 && apdu[0] & 8 != 0 {
            return frame(
                ApduHeader::Abort {
                    server: true,
                    invoke_id: apdu[2],
                    reason: AbortReason::SEGMENTATION_NOT_SUPPORTED,
                },
                &[],
                out,
            )
            .map(Some);
        }
        let Ok((header, body)) = ApduHeader::decode(apdu) else {
            return Ok(None);
        };
        match header {
            ApduHeader::UnconfirmedRequest(Unconfirmed::WHO_IS) => {
                let Ok(who) = WhoIsRequest::decode(body) else {
                    return Ok(None);
                };
                if who
                    .range
                    .is_some_and(|r| !r.contains(self.id.instance_number()))
                {
                    return Ok(None);
                }
                let iam = IAmRequest {
                    object_identifier: self.id,
                    max_apdu_length: 1476,
                    segmentation_supported: Segmentation::NONE,
                    vendor_id: self.vendor,
                };
                let mut scratch = [0; 32];
                let n = iam
                    .encode_to_sink(&mut &mut scratch[..])
                    .map_err(|_| WireError::OutputTooSmall)?;
                frame(
                    ApduHeader::UnconfirmedRequest(Unconfirmed::I_AM),
                    &scratch[..n],
                    out,
                )
                .map(Some)
            }
            ApduHeader::ConfirmedRequest(h) if !group => {
                if h.service_choice == Service::WRITE_PROPERTY
                    && let Some(slots) = slots
                {
                    return slots.write(h, body, out, allow).map(Some);
                }
                self.confirmed(h, body, out, slots.as_deref()).map(Some)
            }
            _ => Ok(None),
        }
    }
    fn confirmed(
        &self,
        h: ConfirmedRequestHeader,
        body: &[u8],
        out: &mut [u8],
        slots: Option<&CommandSlots>,
    ) -> Result<usize, WireError> {
        let reject = |reason| ApduHeader::Reject {
            invoke_id: h.invoke_id,
            reason,
        };
        if h.service_choice == Service::READ_PROPERTY_MULTIPLE {
            return self.batch_reply(h, body, out, slots);
        }
        if h.service_choice != Service::READ_PROPERTY {
            return frame(reject(RejectReason::UNRECOGNIZED_SERVICE), &[], out);
        }
        let Ok(mut req) = ReadPropertyRequest::decode(body) else {
            return frame(reject(RejectReason::INVALID_TAG), &[], out);
        };
        req.object_identifier = self.resolve_read_object(req.object_identifier);
        let mut value = [0; 96];
        let count = match self.value(&req, &mut value, slots) {
            Ok(n) => n,
            Err((class, code)) => {
                return crate::wire::service_error(h, class, code, out);
            }
        };
        let ack = ReadPropertyAckRef {
            object_identifier: req.object_identifier,
            property_identifier: req.property_identifier,
            property_array_index: req.property_array_index,
            property_value: &value[..count],
        };
        let mut encoded = [0; 120];
        let n = ack
            .encode_to_sink(&mut &mut encoded[..])
            .map_err(|_| WireError::OutputTooSmall)?;
        if n + 3 > usize::from(h.max_apdu.octets()) {
            return frame(
                ApduHeader::Abort {
                    server: true,
                    invoke_id: h.invoke_id,
                    reason: AbortReason::SEGMENTATION_NOT_SUPPORTED,
                },
                &[],
                out,
            );
        }
        frame(
            ApduHeader::ComplexAck {
                invoke_id: h.invoke_id,
                service_choice: h.service_choice,
            },
            &encoded[..n],
            out,
        )
    }
    pub(crate) fn resolve_read_object(&self, object: ObjectIdentifier) -> ObjectIdentifier {
        if object.object_type() == ObjectType::DEVICE
            && object.instance_number() == ObjectIdentifier::WILDCARD_INSTANCE
        {
            self.id
        } else {
            object
        }
    }

    pub(crate) fn batch_properties(
        &self,
        id: ObjectIdentifier,
        slots: Option<&CommandSlots>,
    ) -> Option<(&'static [Prop], &'static [Prop])> {
        const DEVICE: &[Prop] = &[
            Prop::OBJECT_IDENTIFIER,
            Prop::OBJECT_NAME,
            Prop::OBJECT_TYPE,
            Prop::OBJECT_LIST,
            Prop::VENDOR_IDENTIFIER,
            Prop::MAX_APDU_LENGTH_ACCEPTED,
            Prop::SEGMENTATION_SUPPORTED,
            Prop::PROTOCOL_SERVICES_SUPPORTED,
            Prop::PROTOCOL_OBJECT_TYPES_SUPPORTED,
        ];
        const POINT: &[Prop] = &[
            Prop::OBJECT_IDENTIFIER,
            Prop::OBJECT_NAME,
            Prop::OBJECT_TYPE,
            Prop::PRESENT_VALUE,
            Prop::UNITS,
            Prop::STATUS_FLAGS,
        ];
        if id == self.id {
            Some((DEVICE, &[]))
        } else if id == self.input {
            Some((POINT, &[]))
        } else if slots.is_some() && id == analog_value() {
            Some((POINT, &[Prop::PRIORITY_ARRAY, Prop::RELINQUISH_DEFAULT]))
        } else {
            None
        }
    }

    pub(crate) fn value(
        &self,
        r: &ReadPropertyRequest,
        mut sink: &mut [u8],
        slots: Option<&CommandSlots>,
    ) -> Result<usize, (ErrorClass, ErrorCode)> {
        if r.property_identifier == Prop::PROPERTY_LIST {
            // The RPM catalogue already excludes Property_List from ALL / REQUIRED.
            let (required, optional) = self
                .batch_properties(r.object_identifier, slots)
                .ok_or((ErrorClass::OBJECT, ErrorCode::UNKNOWN_OBJECT))?;
            let members = required.iter().chain(optional).copied().filter(|p| {
                !matches!(
                    *p,
                    Prop::OBJECT_IDENTIFIER | Prop::OBJECT_TYPE | Prop::OBJECT_NAME
                )
            });
            return read_array(members, r.property_array_index, sink, |out, p| {
                encode_app_enumerated_to_sink(out, p.to_raw())
            });
        }
        if let Some(slots) = slots
            && r.object_identifier == analog_value()
        {
            return slots.read(r, sink);
        }
        let device = r.object_identifier == self.id;
        if !device && r.object_identifier != self.input {
            return Err((ErrorClass::OBJECT, ErrorCode::UNKNOWN_OBJECT));
        }
        let prop_error = |e| (ErrorClass::PROPERTY, e);
        let p = r.property_identifier;
        if device && p == Prop::OBJECT_LIST {
            let objects = [self.id, self.input, analog_value()];
            let objects = &objects[..if slots.is_some() { 3 } else { 2 }];
            return read_array(objects.iter(), r.property_array_index, sink, |out, id| {
                encode_app_object_id_to_sink(out, id)
            });
        }
        let encoded = match (device, p) {
            (_, Prop::OBJECT_IDENTIFIER) => {
                encode_app_object_id_to_sink(&mut sink, &r.object_identifier)
            }
            (_, Prop::OBJECT_TYPE) => {
                encode_app_enumerated_to_sink(&mut sink, r.object_identifier.object_type().to_raw())
            }
            (_, Prop::OBJECT_NAME) => encode_app_character_string_to_sink(
                &mut sink,
                if device {
                    "Commissioning fixture"
                } else {
                    "Supply temperature"
                },
            ),
            (true, Prop::VENDOR_IDENTIFIER) => {
                encode_app_unsigned_to_sink(&mut sink, self.vendor.into())
            }
            (true, Prop::MAX_APDU_LENGTH_ACCEPTED) => encode_app_unsigned_to_sink(&mut sink, 1476),
            (true, Prop::SEGMENTATION_SUPPORTED) => {
                encode_app_enumerated_to_sink(&mut sink, Segmentation::NONE.to_raw().into())
            }
            // Clause 21 widths for the worksheet's 135-2024 vocabulary.
            // RP (12), RPM (14), optional WP (15), Who-Is (34).
            (true, Prop::PROTOCOL_SERVICES_SUPPORTED) => encode_app_bit_string_to_sink(
                &mut sink,
                6,
                &[0, if slots.is_some() { 11 } else { 10 }, 0, 0, 0x20, 0, 0],
            ),
            (true, Prop::PROTOCOL_OBJECT_TYPES_SUPPORTED) => encode_app_bit_string_to_sink(
                &mut sink,
                7,
                &[
                    if slots.is_some() { 0xa0 } else { 0x80 },
                    0x80,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ],
            ),
            (false, Prop::PRESENT_VALUE) => encode_app_real_to_sink(&mut sink, self.temperature),
            (false, Prop::UNITS) => encode_app_enumerated_to_sink(&mut sink, 62),
            (false, Prop::STATUS_FLAGS) => {
                encode_app_bit_string_to_sink(&mut sink, 4, &[if self.fault { 0x40 } else { 0 }])
            }
            _ => return Err(prop_error(ErrorCode::UNKNOWN_PROPERTY)),
        };
        if r.property_array_index.is_some() {
            return Err(prop_error(ErrorCode::PROPERTY_IS_NOT_AN_ARRAY));
        }
        encoded.map_err(|_| (ErrorClass::RESOURCES, ErrorCode::NO_SPACE_TO_WRITE_PROPERTY))
    }
}
