use bacnet_kit::{
    Device, ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest,
    wire::{MAX_FRAME, encode_read},
};
fn fixture() -> Device {
    Device::new(123, 0, 21.5).unwrap()
}
fn object(t: ObjectType, id: u32) -> ObjectIdentifier {
    ObjectIdentifier::new(t, id).unwrap()
}
fn request(id: ObjectIdentifier, prop: Prop, index: Option<u32>, invoke: u8) -> Vec<u8> {
    let mut out = [0; 64];
    let n = encode_read(
        invoke,
        &ReadPropertyRequest {
            object_identifier: id,
            property_identifier: prop,
            property_array_index: index,
        },
        &mut out,
    )
    .unwrap();
    out[..n].to_vec()
}
#[test]
fn known_bytes_and_array_selections() {
    let device = fixture();
    let id = object(ObjectType::DEVICE, 123);
    let full = request(id, Prop::OBJECT_LIST, None, 42);
    assert_eq!(
        full,
        [
            0x81, 10, 0, 17, 1, 4, 0, 5, 42, 12, 12, 2, 0, 0, 123, 25, 76
        ]
    );
    let mut out = [0; MAX_FRAME];
    assert_eq!(device.respond(&[0; MAX_FRAME + 1], &mut out).unwrap(), None);
    for (i, want) in [
        (None, &[0xc4, 2, 0, 0, 123, 0xc4, 0, 0, 0, 1][..]),
        (Some(0), &[0x21, 2]),
        (Some(1), &[0xc4, 2, 0, 0, 123]),
        (Some(2), &[0xc4, 0, 0, 0, 1]),
    ] {
        let n = device
            .respond(&request(id, Prop::OBJECT_LIST, i, 42), &mut out)
            .unwrap()
            .unwrap();
        let ack = bacnet_kit::wire::ReadPropertyAckRef::decode(&out[9..n]).unwrap();
        assert_eq!(ack.property_array_index, i);
        assert_eq!(ack.property_value, want);
    }
    assert!(device.respond(&full, &mut [0; 4]).is_err());
    for end in 0..full.len() {
        assert_eq!(device.respond(&full[..end], &mut out).unwrap(), None);
    }
}
#[test]
fn read_only_dispatch_and_scoped_discovery() {
    // 001-commissioning/SR-003
    let device = fixture();
    let mut out = [0; 100];
    let mut req = request(
        object(ObjectType::ANALOG_INPUT, 1),
        Prop::PRESENT_VALUE,
        None,
        42,
    );
    req[9] = 15;
    let n = device.respond(&req, &mut out).unwrap().unwrap();
    assert_eq!(&out[6..n], &[0x60, 42, 9]);
    req[9] = 12;
    let n = device.respond(&req, &mut out).unwrap().unwrap();
    let ack = bacnet_kit::wire::ReadPropertyAckRef::decode(&out[9..n]).unwrap();
    assert_eq!(ack.property_value, &[0x44, 0x41, 0xac, 0, 0]);
    req[1] = 11;
    assert_eq!(device.respond(&req, &mut out).unwrap(), None);
    let who = [0x81, 11, 0, 12, 1, 0, 0x10, 8, 9, 122, 25, 124];
    let n = device.respond(&who, &mut out).unwrap().unwrap();
    let iam = bacnet_services::who_is::IAmRequest::decode(&out[8..n]).unwrap();
    assert_eq!(iam.object_identifier.instance_number(), 123);
    let mut outside = who;
    outside[9] = 124;
    assert_eq!(device.respond(&outside, &mut out).unwrap(), None);
}

#[test]
fn capability_properties_describe_the_fixture() {
    let device = fixture();
    let mut out = [0; MAX_FRAME];
    let id = object(ObjectType::DEVICE, 123);
    for (prop, expected) in [
        // 50 service bits: ReadProperty (12), RPM (14) and Who-Is (34) execute.
        (
            Prop::PROTOCOL_SERVICES_SUPPORTED,
            &[0x85, 8, 6, 0, 10, 0, 0, 0x20, 0, 0][..],
        ),
        // 65 object type bits: Analog Input (0) and Device (8) exist.
        (
            Prop::PROTOCOL_OBJECT_TYPES_SUPPORTED,
            &[0x85, 10, 7, 0x80, 0x80, 0, 0, 0, 0, 0, 0, 0],
        ),
    ] {
        let n = device
            .respond(&request(id, prop, None, 1), &mut out)
            .unwrap()
            .unwrap();
        let ack = bacnet_kit::wire::ReadPropertyAckRef::decode(&out[9..n]).unwrap();
        assert_eq!(ack.property_value, expected);
    }
}
