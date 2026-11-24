//! Read the width on the wire, including positions unknown to this build.
use super::{Client, Error, ObjectIdentifier, ObjectType, Prop, Reading, Value};

/// An MSB-first capability bit string as transmitted by the peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityBits {
    data: Vec<u8>,
    width: usize,
}
impl CapabilityBits {
    /// Maximum accepted wire width. Covers the complete BACnet object-type number space.
    pub const MAX_BITS: usize = 1024;

    fn decode(reading: &Reading) -> Result<Self, Error> {
        let Value::BitString { unused_bits, data } = reading.scalar()? else {
            return Err(Error::Invalid("expected a capability bit string"));
        };
        if data.len() > Self::MAX_BITS / 8
            || unused_bits > 7
            || (data.is_empty() && unused_bits != 0)
            || data
                .last()
                .is_some_and(|b| b & ((1u8 << unused_bits) - 1) != 0)
        {
            return Err(Error::Invalid(
                "invalid capability bit string width or padding",
            ));
        }
        Ok(Self {
            width: data.len() * 8 - usize::from(unused_bits),
            data: data.to_vec(),
        })
    }

    /// Significant positions, including clear trailing bits.
    pub fn width(&self) -> usize {
        self.width
    }

    /// All set positions in wire order, preserving unknown extensions.
    pub fn positions(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.width).filter(|&bit| self.data[bit / 8] & (0x80 >> (bit % 8)) != 0)
    }
}

/// Executable services and possible object types of one concrete Device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub services: CapabilityBits,
    pub object_types: CapabilityBits,
}
impl Client {
    /// Read both Device capability properties under one transaction time budget.
    pub fn capabilities(&mut self, instance: u32) -> Result<Capabilities, Error> {
        let device = ObjectIdentifier::new_addressable(ObjectType::DEVICE, instance)
            .map_err(|_| Error::Invalid("expected a concrete Device instance"))?;
        let deadline = self.deadline()?;
        let mut bits = |p| CapabilityBits::decode(&self.read_until(device, p, None, deadline)?);
        Ok(Capabilities {
            services: bits(Prop::PROTOCOL_SERVICES_SUPPORTED)?,
            object_types: bits(Prop::PROTOCOL_OBJECT_TYPES_SUPPORTED)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(bytes: &[u8]) -> Result<CapabilityBits, Error> {
        CapabilityBits::decode(&Reading {
            object: ObjectIdentifier::new(ObjectType::DEVICE, 123).unwrap(),
            value: bytes.to_vec(),
        })
    }
    // 009-capability-evidence/SR-002
    #[test]
    fn preserves_short_and_extended_widths_and_rejects_malformed_data() {
        let short = parse(&[0x82, 7, 0x80]).unwrap();
        assert_eq!(short.width(), 1);
        assert_eq!(short.positions().collect::<Vec<_>>(), [0]);
        let mut long = vec![0x85, 129, 0];
        long.resize(131, 0);
        *long.last_mut().unwrap() = 1;
        let long = parse(&long).unwrap();
        assert_eq!(long.width(), 1024);
        assert_eq!(long.positions().collect::<Vec<_>>(), [1023]);
        let mut oversized = vec![0x85, 130, 0];
        oversized.resize(132, 0);
        for value in [
            &oversized[..],
            &[0x82, 8, 0],
            &[0x82, 7, 1],
            &[0x82, 7],
            &[0x21, 0],
            &[0x81, 0, 0],
        ] {
            assert!(parse(value).is_err());
        }
    }
}
