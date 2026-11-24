//! Evidence for the two known workstation configurations.
use super::{Client, ObjectIdentifier, ObjectType, Prop, ReadPropertyRequest, Result};
use bacnet_encoding::primitives::{ApplicationValueRef as Value, decode_application_value_ref};
use bacnet_kit::client::Error;
use std::fmt::Write;

const IMPLICIT: [Prop; 4] = [
    Prop::OBJECT_IDENTIFIER,
    Prop::OBJECT_NAME,
    Prop::OBJECT_TYPE,
    Prop::PROPERTY_LIST,
];

fn listed_properties(
    client: &mut Client,
    object: ObjectIdentifier,
    expected: &[Prop],
) -> Result<Vec<Prop>> {
    let count = client.read(object, Prop::PROPERTY_LIST, Some(0))?;
    if count.scalar()? != Value::Unsigned(expected.len() as u64) {
        return Err(format!("{object}: unexpected Property_List length").into());
    }
    let whole = client.read(object, Prop::PROPERTY_LIST, None)?;
    let mut offset = 0;
    let mut actual = Vec::with_capacity(expected.len());
    for index in 1..=expected.len() {
        let (member, next) = decode_application_value_ref(&whole.value, offset)?;
        let indexed = client.read(object, Prop::PROPERTY_LIST, Some(index as u32))?;
        let Value::Enumerated(id) = member else {
            return Err(format!("{object}: invalid Property_List element {index}").into());
        };
        let prop = Prop::from_raw(id);
        if !expected.contains(&prop) || actual.contains(&prop) || indexed.scalar()? != member {
            return Err(
                format!("{object}: inconsistent Property_List element {index} ({prop})").into(),
            );
        }
        actual.push(prop);
        offset = next;
    }
    if offset != whole.value.len() {
        return Err(format!("{object}: trailing Property_List elements").into());
    }
    let refs = actual
        .iter()
        .chain(&IMPLICIT)
        .map(|&property_identifier| ReadPropertyRequest {
            object_identifier: object,
            property_identifier,
            property_array_index: None,
        })
        .collect::<Vec<_>>();
    for row in client.read_multiple(&refs)? {
        let value = row.outcome.map_err(|e| {
            format!(
                "{object}/{}: class {}, code {}",
                row.reference.property_identifier, e.class, e.code
            )
        })?;
        match row.reference.property_identifier {
            Prop::PROPERTY_LIST if value.value != whole.value => {
                return Err(
                    format!("{object}: RP and RPM returned different property lists").into(),
                );
            }
            Prop::OBJECT_IDENTIFIER if value.scalar()? != Value::ObjectIdentifier(object) => {
                return Err(format!("{object}: inconsistent Object_Identifier").into());
            }
            Prop::OBJECT_TYPE
                if value.scalar()? != Value::Enumerated(object.object_type().to_raw()) =>
            {
                return Err(format!("{object}: inconsistent Object_Type").into());
            }
            _ => {}
        }
    }
    Ok(actual)
}

pub(super) fn worksheet(client: &mut Client, commands: bool) -> Result<String> {
    let inventory = client.inventory_of(123)?;
    let device = inventory.device;
    let input = ObjectIdentifier::new(ObjectType::ANALOG_INPUT, 1)?;
    let value = ObjectIdentifier::new(ObjectType::ANALOG_VALUE, 1)?;
    let objects = [device, input, value];
    let expected = &objects[..if commands { 3 } else { 2 }];
    if inventory.objects.len() != expected.len()
        || !expected.iter().all(|o| inventory.objects.contains(o))
    {
        return Err("Object_List disagrees with the selected fixture configuration".into());
    }

    let caps = client.capabilities(123)?;
    let services: &[usize] = if commands {
        &[12, 14, 15, 34]
    } else {
        &[12, 14, 34]
    };
    let types: &[usize] = if commands { &[0, 2, 8] } else { &[0, 8] };
    if caps.services.width() != 50
        || caps.object_types.width() != 65
        || !caps.services.positions().eq(services.iter().copied())
        || !caps.object_types.positions().eq(types.iter().copied())
    {
        return Err(
            "Device capability bits disagree with the selected fixture configuration".into(),
        );
    }
    for (property, expected) in [
        (Prop::VENDOR_IDENTIFIER, Value::Unsigned(0)),
        (Prop::MAX_APDU_LENGTH_ACCEPTED, Value::Unsigned(1476)),
        (Prop::SEGMENTATION_SUPPORTED, Value::Enumerated(3)),
    ] {
        if client.read(device, property, None)?.scalar()? != expected {
            return Err(format!("{device}/{property}: unexpected fixture value").into());
        }
    }
    let revision = match client.read(device, Prop::PROTOCOL_REVISION, None) {
        Err(Error::Remote { class: 2, code: 32 }) => "unreported (Unknown_Property)".into(),
        Ok(reading) => match reading.scalar()? {
            Value::Unsigned(n) => n.to_string(),
            _ => return Err("invalid Protocol_Revision".into()),
        },
        Err(e) => return Err(e.into()),
    };
    let mode = if commands {
        "supervised commands"
    } else {
        "read-only"
    };
    let mut report = format!(
        "# Reference fixture PICS worksheet

Configuration: {mode}, bacnet-kit {}.

## Observed over BACnet/IP

Device 123, fixture vendor ID 0, maximum APDU 1476 octets, no segmentation. Protocol_Revision: {revision}.

Worksheet baseline: ANSI/ASHRAE 135-2024. Clause 21 vocabulary widths: {} service bits and {} object-type bits.

Executable service positions: {services:?}. Object-type positions: {types:?}.

| Object instance | Verified Property_List members |
| --- | --- |
",
        env!("CARGO_PKG_VERSION"),
        caps.services.width(),
        caps.object_types.width()
    );
    for &object in expected {
        let properties: &[Prop] = if object == device {
            &[
                Prop::OBJECT_LIST,
                Prop::VENDOR_IDENTIFIER,
                Prop::MAX_APDU_LENGTH_ACCEPTED,
                Prop::SEGMENTATION_SUPPORTED,
                Prop::PROTOCOL_SERVICES_SUPPORTED,
                Prop::PROTOCOL_OBJECT_TYPES_SUPPORTED,
            ]
        } else if object == input {
            &[Prop::PRESENT_VALUE, Prop::UNITS, Prop::STATUS_FLAGS]
        } else {
            &[
                Prop::PRESENT_VALUE,
                Prop::UNITS,
                Prop::STATUS_FLAGS,
                Prop::PRIORITY_ARRAY,
                Prop::RELINQUISH_DEFAULT,
            ]
        };
        let names = listed_properties(client, object, properties)?
            .into_iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(report, "| {object} | {names} |")?;
    }
    report.push_str("
Object_Identifier, Object_Name, Object_Type and Property_List were also read explicitly for every object.

## Candidate BIBBs and behavior evidence

| Annex K candidate | Device role | Verification |
| --- | --- | --- |
| DS-RP-B | Executes ReadProperty (15.5) | Live read and `scripts/check-interop.py` (C bacrp) |
| DS-RPM-B | Executes ReadPropertyMultiple (15.7) | Live batch and `scripts/check-interop.py` (C bacrpm) |
| DM-DDB-B | Executes Who-Is and initiates I-Am (Clause 16) | `tests/core.rs` |
");
    if commands {
        report.push_str("| DS-WP-B | Executes WriteProperty (15.9) | `tests/commands.rs` and `scripts/check-interop.py` (C bacwp) |

Only Analog Value 1 Present_Value accepts a finite BACnet REAL or NULL. The workstation policy allows loopback requests at priorities 8 and 16. NULL relinquishes the slot. Command slots are volatile.
");
    } else {
        report.push_str(
            "
The read-only configuration accepts no property writes.
",
        );
    }
    report.push_str("
## Product declaration still required

This capture covers the IPv4 Annex J reference server. The fixture has fixed instances and supports no dynamic creation or deletion. The manufacturer supplies its assigned identity, model and firmware, claimed protocol revision, Annex L profile, required objects and properties including Network Port where applicable, persistence and shipping link configuration. Complete the Clause 22 / Annex A PICS and obtain product-specific test evidence before claiming conformance.
");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 009-capability-evidence/SR-003
    #[test]
    fn mismatched_fixture_fails_without_returning_a_worksheet() {
        for commands in [false, true] {
            let err = crate::with_fixture(commands, false, |peer| {
                worksheet(&mut Client::connect(peer, crate::TIMEOUT)?, !commands)
            })
            .unwrap_err();
            assert!(err.to_string().contains("Object_List"));
        }
    }
}
