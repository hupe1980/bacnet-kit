use bacnet_kit::{
    Device,
    wire::{MAX_FRAME, WireError},
};

fn group(instance: u8, properties: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x0c, 0, 0, 0, instance, 0x1e];
    bytes.extend_from_slice(properties);
    bytes.push(0x1f);
    bytes
}
fn request(body: &[u8], apdu_size: u8) -> Vec<u8> {
    let mut bytes = vec![0x81, 10, 0, 0, 1, 4, 0, apdu_size, 42, 14];
    bytes.extend_from_slice(body);
    let size = (bytes.len() as u16).to_be_bytes();
    bytes[2..4].copy_from_slice(&size);
    bytes
}
fn reply(body: &[u8], apdu_size: u8) -> Vec<u8> {
    let mut out = [0; MAX_FRAME];
    let n = Device::new(123, 0, 21.5)
        .unwrap()
        .respond(&request(body, apdu_size), &mut out)
        .unwrap()
        .unwrap();
    assert_eq!(usize::from(u16::from_be_bytes([out[2], out[3]])), n);
    out[6..n].to_vec()
}

#[test]
fn mixed_ack_preserves_identifiers_order_and_duplicate_occurrences() {
    let actual = reply(&group(1, &[9, 85, 9, 28, 9, 85]), 5);
    let value = [0x29, 85, 0x4e, 0x44, 0x41, 0xac, 0, 0, 0x4f];
    let error = [0x29, 28, 0x5e, 0x91, 2, 0x91, 32, 0x5f];
    assert_eq!(
        actual,
        [
            &[0x30, 42, 14, 0x0c, 0, 0, 0, 1, 0x1e][..],
            &value,
            &error,
            &value,
            &[0x1f]
        ]
        .concat()
    );
}

#[test]
fn selectors_and_invalid_index_return_distinct_wire_results() {
    assert_eq!(
        reply(&group(1, &[9, 80]), 5),
        [0x30, 42, 14, 0x0c, 0, 0, 0, 1, 0x1e, 0x1f]
    );
    let all = reply(&group(1, &[9, 8]), 5);
    assert_eq!(all, reply(&group(1, &[9, 105]), 5));
    assert_eq!(
        all,
        reply(&group(1, &[9, 75, 9, 77, 9, 79, 9, 85, 9, 117, 9, 111]), 5)
    );
    let indexed = reply(&group(1, &[9, 8, 0x19, 0]), 5);
    assert_eq!(
        &indexed[9..],
        &[0x29, 8, 0x39, 0, 0x5e, 0x91, 2, 0x91, 50, 0x5f, 0x1f]
    );
    assert_eq!(
        &reply(&group(99, &[9, 8]), 5)[9..],
        &[0x29, 8, 0x5e, 0x91, 1, 0x91, 31, 0x5f, 0x1f]
    );
}

#[test]
fn wildcard_device_reads_match_the_concrete_device() {
    // Device 4194303 aliases the local Device throughout the read access specification.
    let mut wildcard = group(0, &[9, 75, 9, 76, 0x19, 0, 9, 8]);
    wildcard[1..5].copy_from_slice(&0x023f_ffffu32.to_be_bytes());
    let mut concrete = wildcard.clone();
    concrete[1..5].copy_from_slice(&0x0200_007bu32.to_be_bytes());
    assert_eq!(reply(&wildcard, 5), reply(&concrete, 5));
}

#[test]
fn batch_budgets_fail_without_partial_ack() {
    // 003-batch-reads/SR-002
    assert_eq!(reply(&group(1, &[9, 85].repeat(32)), 5)[0], 0x30);
    assert_eq!(reply(&group(1, &[9, 85].repeat(33)), 5), [0x71, 42, 9]);
    assert_eq!(reply(&group(1, &[9, 8].repeat(6)), 5), [0x71, 42, 9]);
    assert_eq!(reply(&group(1, &[9, 80].repeat(33)), 5), [0x71, 42, 9]);
    assert_eq!(reply(&group(1, &[9, 85].repeat(8)), 0), [0x71, 42, 4]);
    for bad in [vec![], group(1, &[]), group(1, &[9, 85, 0x2e])] {
        assert_eq!(reply(&bad, 5), [0x60, 42, 4]);
    }
    let device = Device::new(123, 0, 21.5).unwrap();
    assert_eq!(
        device.respond(&request(&group(1, &[9, 85]), 5), &mut [0; 10]),
        Err(WireError::OutputTooSmall)
    );
    let mut broadcast = request(&group(1, &[9, 85]), 5);
    broadcast[1] = 11;
    assert_eq!(device.respond(&broadcast, &mut [0; 64]).unwrap(), None);
}

#[cfg(feature = "std")]
mod host {
    use super::*;
    use bacnet_encoding::primitives::ApplicationValueRef as Value;
    use bacnet_kit::{
        ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest,
        client::{Client, Error, PropertyError},
    };
    use std::{
        net::{SocketAddr, UdpSocket},
        thread,
        time::Duration,
    };

    fn reference(property: Prop, index: Option<u32>) -> ReadPropertyRequest {
        ReadPropertyRequest {
            object_identifier: ObjectIdentifier::new(ObjectType::ANALOG_INPUT, 1).unwrap(),
            property_identifier: property,
            property_array_index: index,
        }
    }
    fn peers() -> (UdpSocket, Client) {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let SocketAddr::V4(addr) = peer.local_addr().unwrap() else {
            unreachable!()
        };
        (peer, Client::connect(addr, Duration::from_secs(2)).unwrap())
    }
    #[test]
    fn validates_complete_batch_before_returning_any_rows() {
        // 003-batch-reads/SR-001
        let (server, mut client) = peers();
        let worker = thread::spawn(move || {
            let mut buf = [0; MAX_FRAME];
            let (n, peer) = server.recv_from(&mut buf).unwrap();
            let mut expected = request(&group(1, &[9, 85, 9, 28, 9, 85]), 5);
            expected[6] = 2; // segmented replies accepted
            expected[7] = 0x45; // up to 16 segments, 1476 octets each
            expected[8] = 0;
            assert_eq!(&buf[..n], &expected);
            let mut ack = [0; 256];
            let n = Device::new(123, 0, 21.5)
                .unwrap()
                .respond(&buf[..n], &mut ack)
                .unwrap()
                .unwrap();
            // Give the first value in every mismatched reply a distinguishable reading.
            let mut wrong = ack[..n].to_vec();
            wrong[19..23].copy_from_slice(&99f32.to_be_bytes());
            for (pos, v) in [(7, 1), (8, 12), (13, 2), (16, 77), (25, 0x39), (n - 9, 77)] {
                let mut bytes = wrong.clone();
                bytes[pos] = v;
                server.send_to(&bytes, peer).unwrap();
            }
            let mut indexed = wrong.clone();
            indexed.splice(17..17, [0x39, 0]);
            let mut extra = wrong.clone();
            extra.splice(n - 1..n - 1, wrong[15..24].iter().copied());
            let mut missing = wrong.clone();
            missing.drain(32..41);
            for mut bytes in [
                indexed,
                extra,
                missing,
                wrong[..n - 1].to_vec(),
                [wrong.as_slice(), &[0]].concat(),
            ] {
                let len = (bytes.len() as u16).to_be_bytes();
                bytes[2..4].copy_from_slice(&len);
                server.send_to(&bytes, peer).unwrap();
            }
            server.send_to(&ack[..n], peer).unwrap();
        });
        let refs = [
            reference(Prop::PRESENT_VALUE, None),
            reference(Prop::DESCRIPTION, None),
            reference(Prop::PRESENT_VALUE, None),
        ];
        let rows = client.read_multiple(&refs).unwrap();
        assert_eq!(rows.len(), 3);
        for (r, result) in refs.iter().zip(&rows) {
            assert_eq!(r, &result.reference);
        }
        assert_eq!(
            rows[0].outcome.as_ref().unwrap().scalar().unwrap(),
            Value::Real(21.5)
        );
        assert_eq!(
            rows[1].outcome.as_ref().unwrap_err(),
            &PropertyError { class: 2, code: 32 }
        );
        assert_eq!(
            rows[2].outcome.as_ref().unwrap().scalar().unwrap(),
            Value::Real(21.5)
        );
        worker.join().unwrap();
    }
    #[test]
    fn refuses_unbounded_or_ambiguous_client_inputs_locally() {
        let (_peer, mut client) = peers();
        let r = reference(Prop::PRESENT_VALUE, None);
        let mut wildcard = r.clone();
        wildcard.object_identifier =
            ObjectIdentifier::new(ObjectType::DEVICE, ObjectIdentifier::WILDCARD_INSTANCE).unwrap();
        for refs in [
            vec![],
            vec![r; 33],
            vec![reference(Prop::ALL, None)],
            vec![wildcard],
        ] {
            assert!(matches!(
                client.read_multiple(&refs),
                Err(Error::Invalid(_))
            ));
        }
    }
}

#[test]
fn explicit_property_list_in_a_mixed_batch_preserves_array_semantics() {
    // 009-capability-evidence/SR-001
    // Analog Input 1 exposes Present_Value, Units and Status_Flags beyond the implicit four.
    let property_list = [0x0a, 0x01, 0x73];
    let mut properties = property_list.to_vec();
    for index in [0, 1, 3, 4] {
        properties.extend(property_list);
        properties.extend([0x19, index]);
    }
    let apdu = reply(&group(1, &properties), 5);
    let property = &[0x2a, 0x01, 0x73][..];
    assert_eq!(
        &apdu[9..],
        [
            property,
            &[0x4e, 0x91, 85, 0x91, 117, 0x91, 111, 0x4f],
            property,
            &[0x39, 0, 0x4e, 0x21, 3, 0x4f],
            property,
            &[0x39, 1, 0x4e, 0x91, 85, 0x4f],
            property,
            &[0x39, 3, 0x4e, 0x91, 111, 0x4f],
            property,
            &[0x39, 4, 0x5e, 0x91, 2, 0x91, 42, 0x5f, 0x1f]
        ]
        .concat()
    );
}
