use bacnet_kit::{
    Device, ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest,
    commands::{Command, CommandSlots},
    wire::{ReadPropertyAckRef, WireError, encode_read},
};

struct Fixture {
    device: Device,
    slots: CommandSlots,
}
impl Fixture {
    fn new() -> Self {
        Self {
            device: Device::new(123, 0, 21.5).unwrap(),
            slots: CommandSlots::default(),
        }
    }
    fn exchange(&mut self, input: &[u8], allow: impl FnOnce(Command) -> bool) -> Vec<u8> {
        let mut out = [0; 256];
        let n = self
            .device
            .respond_with_commands(input, &mut out, &mut self.slots, allow)
            .unwrap()
            .unwrap_or(0);
        out[..n].to_vec()
    }
    fn read(&mut self, t: ObjectType, id: u32, p: Prop, i: Option<u32>) -> Vec<u8> {
        let mut req = [0; 64];
        let n = encode_read(
            7,
            &ReadPropertyRequest {
                object_identifier: ObjectIdentifier::new(t, id).unwrap(),
                property_identifier: p,
                property_array_index: i,
            },
            &mut req,
        )
        .unwrap();
        let ack = self.exchange(&req[..n], |_| panic!("read asked for write permission"));
        assert_eq!(&ack[6..9], &[0x30, 7, 12]);
        ReadPropertyAckRef::decode(&ack[9..])
            .unwrap()
            .property_value
            .to_vec()
    }
    fn value(&mut self, p: Prop, i: Option<u32>) -> Vec<u8> {
        self.read(ObjectType::ANALOG_VALUE, 1, p, i)
    }
}
fn write(p: u8, value: Option<f32>) -> Vec<u8> {
    let mut buf = [0; 40];
    let n = Command::new(p, value)
        .unwrap()
        .encode(42, &mut buf)
        .unwrap();
    buf[..n].to_vec()
}
fn real(v: f32) -> Vec<u8> {
    let mut b = vec![0x44];
    b.extend(v.to_be_bytes());
    b
}

#[test]
fn lost_ack_readback_and_relinquish() {
    // 002-supervised-commands/SR-004
    let mut f = Fixture::new();
    for (p, v, effective) in [
        (16, Some(19.), 19.),
        (8, Some(22.), 22.),
        (16, Some(18.), 22.),
        (8, None, 18.),
        (16, None, 20.),
    ] {
        let request = write(p, v);
        // Deliberately discard the result that the adapter would send. Recover with two new reads.
        drop(f.exchange(&request, |c| c.priority() == p && c.value() == v));
        assert_eq!(
            f.value(Prop::PRIORITY_ARRAY, Some(p.into())),
            v.map_or(vec![0], real)
        );
        assert_eq!(f.value(Prop::PRESENT_VALUE, None), real(effective));
    }
    assert_eq!(f.value(Prop::PRIORITY_ARRAY, None), vec![0; 16]);
    assert_eq!(f.value(Prop::PRIORITY_ARRAY, Some(0)), [0x21, 16]);
    assert_eq!(f.value(Prop::RELINQUISH_DEFAULT, None), real(20.));
}

#[test]
fn denied_targets_priorities_and_values_do_not_change_slots() {
    // 002-supervised-commands/SR-001
    let mut f = Fixture::new();
    let request = write(8, Some(30.));
    assert_eq!(
        f.exchange(&request, |_| false)[6..],
        [0x50, 42, 15, 0x91, 2, 0x91, 40]
    );
    for priority in [0, 6, 17] {
        let mut invalid = request.clone();
        *invalid.last_mut().unwrap() = priority;
        assert_eq!(
            f.exchange(&invalid, |_| panic!("invalid priority authorized"))[6],
            0x50
        );
    }
    let mut other = request.clone();
    other[14] = 2;
    assert_eq!(
        f.exchange(&other, |_| panic!("wrong object authorized"))[6],
        0x50
    );
    let mut property = request.clone();
    property[16] = 87;
    assert_eq!(
        f.exchange(&property, |_| panic!("wrong property authorized"))[6],
        0x50
    );
    for value in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY] {
        let mut invalid = request.clone();
        invalid[19..23].copy_from_slice(&value.to_be_bytes());
        assert_eq!(
            f.exchange(&invalid, |_| panic!("non-finite value authorized"))[6],
            0x50
        );
    }
    let mut indexed = request.clone();
    indexed.splice(17..17, [0x29, 1]);
    indexed[3] += 2;
    assert_eq!(
        f.exchange(&indexed, |_| panic!("indexed write authorized"))[6..],
        [0x50, 42, 15, 0x91, 2, 0x91, 50]
    );
    let mut group = request;
    group[1] = 11;
    assert!(
        f.exchange(&group, |_| panic!("group request authorized"))
            .is_empty()
    );
    assert_eq!(f.value(Prop::PRIORITY_ARRAY, None), vec![0; 16]);
}

#[test]
fn validates_entire_write_and_reserves_ack_before_commit() {
    // 002-supervised-commands/SR-002
    let mut f = Fixture::new();
    let request = write(8, Some(22.));
    for len in 0..request.len() {
        // The completely omitted optional priority is itself a valid command.
        if len == request.len() - 2 {
            continue;
        }
        let mut short = request[..len].to_vec();
        if len >= 4 {
            short[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        }
        let _ = f.exchange(&short, |_| panic!("partial request authorized"));
    }
    let mut trailing = request.clone();
    trailing.push(0);
    trailing[3] += 1;
    assert_eq!(
        f.exchange(&trailing, |_| panic!("trailing data authorized"))[6],
        0x60
    );
    let mut out = [0; 8];
    assert_eq!(
        f.device
            .respond_with_commands(&request, &mut out, &mut f.slots, |_| true),
        Err(WireError::OutputTooSmall)
    );
    assert_eq!(f.value(Prop::PRIORITY_ARRAY, None), vec![0; 16]);
    let mut omitted = request;
    omitted.truncate(omitted.len() - 2);
    omitted[3] -= 2;
    assert_eq!(
        f.exchange(&omitted, |c| c.priority() == 16)[6..],
        [0x20, 42, 15]
    );
    assert_eq!(f.value(Prop::PRIORITY_ARRAY, Some(16)), real(22.));
}

#[test]
fn advertises_actual_objects_and_services() {
    let mut f = Fixture::new();
    assert_eq!(
        f.read(ObjectType::DEVICE, 123, Prop::OBJECT_LIST, Some(0)),
        [0x21, 3]
    );
    assert_eq!(
        f.read(ObjectType::DEVICE, 123, Prop::OBJECT_LIST, Some(3)),
        [0xc4, 0, 0x80, 0, 1]
    );
    assert_eq!(
        f.read(
            ObjectType::DEVICE,
            123,
            Prop::PROTOCOL_SERVICES_SUPPORTED,
            None
        ),
        [0x85, 8, 6, 0, 11, 0, 0, 0x20, 0, 0]
    );
    assert_eq!(
        f.read(
            ObjectType::DEVICE,
            123,
            Prop::PROTOCOL_OBJECT_TYPES_SUPPORTED,
            None
        ),
        [0x85, 10, 7, 0xa0, 0x80, 0, 0, 0, 0, 0, 0, 0]
    );
}
