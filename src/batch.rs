//! Bounded ReadPropertyMultiple for a local, unsegmented device.
use crate::{
    Device, ObjectIdentifier, PropertyIdentifier as Prop, ReadPropertyRequest,
    commands::CommandSlots,
    wire::{self, WireError},
};
use bacnet_encoding::{
    apdu::header::{ApduHeader, ConfirmedRequestHeader},
    constructed::tagged::{
        decode_ctx_object_id, decode_ctx_unsigned, decode_optional_ctx, expect_closing,
        expect_opening,
    },
    primitives::{
        encode_app_enumerated_to_sink, encode_ctx_object_id_to_sink, encode_ctx_unsigned_to_sink,
    },
    sink::{EncodeSink, SinkFull},
    tags::{decode_tag, encode_closing_tag_to_sink, encode_opening_tag_to_sink},
};
use bacnet_types::{
    enums::{AbortReason, ErrorClass, ErrorCode, RejectReason},
    error::Error,
};

/// Maximum explicit references per transaction and maximum work after selector expansion.
pub const MAX_BATCH: usize = 32;

pub(crate) fn selector(p: Prop) -> bool {
    matches!(p, Prop::ALL | Prop::OPTIONAL | Prop::REQUIRED)
}

pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pub(crate) pos: usize,
}
impl<'a> Cursor<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub(crate) fn object(&mut self) -> Result<ObjectIdentifier, Error> {
        let (id, end) = decode_ctx_object_id(self.data, self.pos, 0, "RPM object")?;
        self.pos = expect_opening(self.data, end, 1, "RPM property list")?;
        Ok(id)
    }
    pub(crate) fn reference(&mut self, tag: u8) -> Result<(Prop, Option<u32>), Error> {
        let (p, end) = decode_ctx_unsigned::<u32>(self.data, self.pos, tag, "RPM property")?;
        let (i, end) = decode_optional_ctx(
            self.data,
            end,
            tag + 1,
            "RPM index",
            decode_ctx_unsigned::<u32>,
        )?;
        self.pos = end;
        Ok((Prop::from_raw(p), i))
    }
    pub(crate) fn close(&mut self, tag: u8) -> Result<(), Error> {
        self.pos = expect_closing(self.data, self.pos, tag, "RPM list")?;
        Ok(())
    }
    fn at_end_of_group(&self) -> Result<bool, Error> {
        Ok(decode_tag(self.data, self.pos)?.0.is_closing_tag(1))
    }
}

pub(crate) fn put_reference(
    tail: &mut &mut [u8],
    tag: u8,
    p: Prop,
    i: Option<u32>,
) -> Result<(), SinkFull> {
    encode_ctx_unsigned_to_sink(tail, tag, p.to_raw().into())?;
    if let Some(i) = i {
        encode_ctx_unsigned_to_sink(tail, tag + 1, i.into())?;
    }
    Ok(())
}

#[derive(Debug)]
enum Refusal {
    Syntax,
    WorkLimit,
    Full,
}
impl From<Error> for Refusal {
    fn from(_: Error) -> Self {
        Self::Syntax
    }
}
impl From<SinkFull> for Refusal {
    fn from(_: SinkFull) -> Self {
        Self::Full
    }
}

impl Device {
    pub(crate) fn batch_reply(
        &self,
        h: ConfirmedRequestHeader,
        input: &[u8],
        out: &mut [u8],
        slots: Option<&CommandSlots>,
    ) -> Result<usize, WireError> {
        let header = ApduHeader::ComplexAck {
            invoke_id: h.invoke_id,
            service_choice: h.service_choice,
        };
        let prefix = wire::frame(header, &[], out)?;
        let max_body = usize::from(h.max_apdu.octets()) - 3;
        let available = (out.len() - prefix).min(max_body);
        let mut tail = &mut out[prefix..prefix + available];
        let result = self.batch_body(input, &mut tail, slots);
        let n = prefix + available - tail.len();
        let response = match result {
            Ok(()) => {
                out[2..4].copy_from_slice(&(n as u16).to_be_bytes());
                return Ok(n);
            }
            Err(Refusal::Syntax) => ApduHeader::Reject {
                invoke_id: h.invoke_id,
                reason: RejectReason::INVALID_TAG,
            },
            Err(Refusal::WorkLimit) => ApduHeader::Abort {
                server: true,
                invoke_id: h.invoke_id,
                reason: AbortReason::OUT_OF_RESOURCES,
            },
            Err(Refusal::Full) if available < max_body => return Err(WireError::OutputTooSmall),
            Err(Refusal::Full) => ApduHeader::Abort {
                server: true,
                invoke_id: h.invoke_id,
                reason: AbortReason::SEGMENTATION_NOT_SUPPORTED,
            },
        };
        wire::frame(response, &[], out)
    }

    fn batch_body(
        &self,
        input: &[u8],
        tail: &mut &mut [u8],
        slots: Option<&CommandSlots>,
    ) -> Result<(), Refusal> {
        if input.is_empty() {
            return Err(Refusal::Syntax);
        }
        let mut cursor = Cursor::new(input);
        let mut remaining = MAX_BATCH;
        while cursor.pos < input.len() {
            let id = self.resolve_read_object(cursor.object()?);
            encode_ctx_object_id_to_sink(tail, 0, &id)?;
            encode_opening_tag_to_sink(tail, 1)?;
            // A read access specification must contain at least one property.
            loop {
                let (p, i) = cursor.reference(0)?;
                if selector(p)
                    && i.is_none()
                    && let Some((required, optional)) = self.batch_properties(id, slots)
                {
                    let required = if p == Prop::OPTIONAL {
                        &[][..]
                    } else {
                        required
                    };
                    let optional = if p == Prop::REQUIRED {
                        &[][..]
                    } else {
                        optional
                    };
                    remaining = remaining
                        .checked_sub((required.len() + optional.len()).max(1))
                        .ok_or(Refusal::WorkLimit)?;
                    for &field in required.iter().chain(optional) {
                        self.batch_element(tail, id, field, None, slots)?;
                    }
                } else {
                    remaining = remaining.checked_sub(1).ok_or(Refusal::WorkLimit)?;
                    self.batch_element(tail, id, p, i, slots)?;
                }
                if cursor.at_end_of_group()? {
                    break;
                }
            }
            cursor.close(1)?;
            encode_closing_tag_to_sink(tail, 1)?;
        }
        Ok(())
    }

    fn batch_element(
        &self,
        tail: &mut &mut [u8],
        id: ObjectIdentifier,
        p: Prop,
        i: Option<u32>,
        slots: Option<&CommandSlots>,
    ) -> Result<(), SinkFull> {
        put_reference(tail, 2, p, i)?;
        let reference = ReadPropertyRequest {
            object_identifier: id,
            property_identifier: p,
            property_array_index: i,
        };
        let mut buf = [0; 96];
        let value = if selector(p) && i.is_some() && self.batch_properties(id, slots).is_some() {
            Err((ErrorClass::PROPERTY, ErrorCode::PROPERTY_IS_NOT_AN_ARRAY))
        } else {
            self.value(&reference, &mut buf, slots)
        };
        match value {
            Ok(n) => {
                encode_opening_tag_to_sink(tail, 4)?;
                tail.write(&buf[..n])?;
                encode_closing_tag_to_sink(tail, 4)?;
            }
            Err((class, code)) => {
                encode_opening_tag_to_sink(tail, 5)?;
                encode_app_enumerated_to_sink(tail, class.to_raw().into())?;
                encode_app_enumerated_to_sink(tail, code.to_raw().into())?;
                encode_closing_tag_to_sink(tail, 5)?;
            }
        }
        Ok(())
    }
}
