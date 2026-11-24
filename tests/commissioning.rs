#![cfg(feature = "std")]
use bacnet_kit::{
    Device, ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest,
    client::{Client, Error, MAX_OBJECTS},
    wire::{MAX_FRAME, encode_read},
};
use std::{
    net::{SocketAddrV4, UdpSocket},
    thread,
    time::{Duration, Instant},
};
fn socket() -> (UdpSocket, SocketAddrV4) {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let std::net::SocketAddr::V4(addr) = s.local_addr().unwrap() else {
        unreachable!()
    };
    (s, addr)
}
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
fn serve(s: UdpSocket, n: usize, device: Device) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0; MAX_FRAME + 1];
        let mut out = [0; MAX_FRAME];
        for _ in 0..n {
            let (n, peer) = s.recv_from(&mut buf).unwrap();
            let len = device.respond(&buf[..n], &mut out).unwrap().unwrap();
            s.send_to(&out[..len], peer).unwrap();
        }
    })
}
#[test]
fn simultaneous_clients_inventory_sample_and_remote_error() {
    let (s, addr) = socket();
    let mut device = fixture();
    device.set_fault(true);
    let worker = serve(s, 8, device);
    let inventory = thread::spawn(move || {
        Client::connect(addr, Duration::from_secs(2))
            .unwrap()
            .inventory()
            .unwrap()
    });
    let mut client = Client::connect(addr, Duration::from_secs(2)).unwrap();
    let sample = client.sample(1).unwrap();
    assert_eq!((sample.value, sample.unit), (21.5, 62));
    assert!(sample.flags.fault);
    let timestamp = sample.observed_at;
    assert!(matches!(
        client.read(
            object(ObjectType::ANALOG_INPUT, 99),
            Prop::PRESENT_VALUE,
            None
        ),
        Err(Error::Remote { class: 1, code: 31 })
    ));
    assert_eq!(timestamp, sample.observed_at);
    assert!(sample.is_stale(timestamp + Duration::from_secs(3), Duration::from_secs(3)));
    assert!(!sample.is_stale(timestamp, Duration::from_secs(3)));
    let list = inventory.join().unwrap();
    assert_eq!(list.device, object(ObjectType::DEVICE, 123));
    assert_eq!(
        list.objects,
        [list.device, object(ObjectType::ANALOG_INPUT, 1)]
    );
    worker.join().unwrap();
}
#[test]
fn accepts_only_matching_peer_and_property() {
    // 001-commissioning/SR-001
    let (s, addr) = socket();
    let worker = thread::spawn(move || {
        let mut input = [0; 256];
        let (n, peer) = s.recv_from(&mut input).unwrap();
        let mut out = [0; 256];
        let device = fixture();
        let right = device.respond(&input[..n], &mut out).unwrap().unwrap();
        let good = out[..right].to_vec();
        out[right - 2] ^= 1;
        let (other, _) = socket();
        other.send_to(&out[..right], peer).unwrap();
        out[7] = 99;
        s.send_to(&out[..right], peer).unwrap();
        let wrong = request(object(ObjectType::ANALOG_INPUT, 1), Prop::UNITS, None, 0);
        let len = device.respond(&wrong, &mut out).unwrap().unwrap();
        s.send_to(&out[..len], peer).unwrap();
        // The altered replies carry a different value, making accidental acceptance observable.
        let mut wrong_service = good.clone();
        wrong_service[8] = 14;
        wrong_service[right - 2] ^= 1;
        s.send_to(&wrong_service, peer).unwrap();
        let mut wrong_object = good.clone();
        wrong_object[13] = 2;
        wrong_object[right - 2] ^= 1;
        s.send_to(&wrong_object, peer).unwrap();
        let indexed = request(
            object(ObjectType::ANALOG_INPUT, 1),
            Prop::PRESENT_VALUE,
            Some(1),
            0,
        );
        // Only ACKs carry an echoed index. Insert the optional context tag before the opening value tag.
        let mut wrong_index = good.clone();
        wrong_index.splice(16..16, indexed[17..].iter().copied());
        wrong_index[3] = wrong_index.len() as u8;
        wrong_index[right] ^= 1;
        s.send_to(&wrong_index, peer).unwrap();
        let mut oversized = good.clone();
        oversized.resize(MAX_FRAME + 1, 0);
        s.send_to(&oversized, peer).unwrap();
        s.send_to(&good, peer).unwrap();
    });
    let reading = Client::connect(addr, Duration::from_secs(1))
        .unwrap()
        .read(
            object(ObjectType::ANALOG_INPUT, 1),
            Prop::PRESENT_VALUE,
            None,
        )
        .unwrap();
    assert_eq!(
        reading.scalar().unwrap(),
        bacnet_encoding::primitives::ApplicationValueRef::Real(21.5)
    );
    worker.join().unwrap();
}
#[test]
fn inventory_limits_and_timeout() {
    // 001-commissioning/SR-002
    let (s, addr) = socket();
    let worker = thread::spawn(move || {
        let mut input = [0; 256];
        let mut out = [0; 256];
        for i in 0..2 {
            let (n, peer) = s.recv_from(&mut input).unwrap();
            let len = fixture().respond(&input[..n], &mut out).unwrap().unwrap();
            if i == 1 {
                out[len - 2] = (MAX_OBJECTS + 1) as u8;
            }
            s.send_to(&out[..len], peer).unwrap();
        }
    });
    assert!(matches!(
        Client::connect(addr, Duration::from_secs(1))
            .unwrap()
            .inventory(),
        Err(Error::TooManyObjects(129))
    ));
    worker.join().unwrap();
    let (_silent, addr) = socket();
    let now = Instant::now();
    assert!(matches!(
        Client::connect(addr, Duration::from_millis(50))
            .unwrap()
            .inventory(),
        Err(Error::Timeout)
    ));
    assert!(now.elapsed() < Duration::from_secs(2));
}

#[test]
fn distinguishes_remote_terminal_outcomes() {
    for (pdu, reason) in [(0x60, 9), (0x71, 4)] {
        let (s, addr) = socket();
        let worker = thread::spawn(move || {
            let mut input = [0; 256];
            let (_, peer) = s.recv_from(&mut input).unwrap();
            s.send_to(&[0x81, 10, 0, 9, 1, 0, pdu, input[8], reason], peer)
                .unwrap();
        });
        let mut client = Client::connect(addr, Duration::from_secs(1)).unwrap();
        let result = client.read(object(ObjectType::DEVICE, 123), Prop::OBJECT_NAME, None);
        assert!(match result {
            Err(Error::Rejected(r)) => pdu == 0x60 && r.to_raw() == reason,
            Err(Error::Aborted(r)) => pdu == 0x71 && r.to_raw() == reason,
            _ => false,
        });
        worker.join().unwrap();
    }
}

#[test]
fn sample_shares_one_deadline_across_properties() {
    // 001-commissioning/SR-002
    let (s, addr) = socket();
    let worker = thread::spawn(move || {
        let mut input = [0; 256];
        let mut out = [0; 256];
        let mut requests = 0;
        s.set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        while let Ok((n, peer)) = s.recv_from(&mut input) {
            thread::sleep(Duration::from_millis(80));
            let size = fixture().respond(&input[..n], &mut out).unwrap().unwrap();
            s.send_to(&out[..size], peer).unwrap();
            requests += 1;
            if requests == 3 {
                break;
            }
        }
    });
    let mut client = Client::connect(addr, Duration::from_millis(185)).unwrap();
    assert!(matches!(client.sample(1), Err(Error::Timeout)));
    worker.join().unwrap();
}

#[test]
fn supported_type_can_have_no_installed_instance() {
    // 009-capability-evidence/SR-002
    let (server, addr) = socket();
    let worker = thread::spawn(move || {
        let device = fixture();
        let mut buf = [0; MAX_FRAME];
        let mut out = [0; MAX_FRAME];
        // Two capability reads followed by the ordinary two-object inventory.
        for _ in 0..6 {
            let (n, peer) = server.recv_from(&mut buf).unwrap();
            let len = device.respond(&buf[..n], &mut out).unwrap().unwrap();
            let ack = bacnet_kit::wire::ReadPropertyAckRef::decode(&out[9..len]).unwrap();
            if ack.property_identifier == Prop::PROTOCOL_OBJECT_TYPES_SUPPORTED {
                // Extend the typed value to 66 positions and advertise type 65.
                let mut bits = ack.property_value.to_vec();
                bits[2] = 6;
                *bits.last_mut().unwrap() = 0x40;
                let extended = bacnet_kit::wire::ReadPropertyAckRef {
                    property_value: &bits,
                    ..ack
                };
                let mut body = [0; 64];
                let size = extended.encode_to_sink(&mut &mut body[..]).unwrap();
                assert_eq!(size, len - 9);
                out[9..len].copy_from_slice(&body[..size]);
            }
            server.send_to(&out[..len], peer).unwrap();
        }
    });
    let mut client = Client::connect(addr, Duration::from_secs(2)).unwrap();
    let caps = client.capabilities(123).unwrap();
    assert_eq!(caps.object_types.width(), 66);
    assert_eq!(
        caps.object_types.positions().collect::<Vec<_>>(),
        [0, 8, 65]
    );
    let inventory = client.inventory_of(123).unwrap();
    assert!(
        inventory
            .objects
            .iter()
            .all(|id| id.object_type().to_raw() != 65)
    );
    worker.join().unwrap();
}
