#![cfg(feature = "std")]
use bacnet_kit::{
    ObjectIdentifier, ObjectType, PropertyIdentifier as Prop,
    client::{Client, ForeignEvent, ForeignListener, RegistrationEvent, RemoteStation},
};
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    thread,
    time::{Duration, Instant},
};

fn peer() -> (UdpSocket, SocketAddrV4) {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let SocketAddr::V4(addr) = s.local_addr().unwrap() else {
        unreachable!()
    };
    (s, addr)
}
fn read_frame(socket: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut buf = [0; 1510];
    let (n, src) = socket.recv_from(&mut buf).unwrap();
    (buf[..n].to_vec(), src)
}
fn frame(function: u8, data: &[u8]) -> Vec<u8> {
    let [hi, lo] = ((data.len() + 4) as u16).to_be_bytes();
    [&[0x81, function, hi, lo][..], data].concat()
}

#[test]
fn only_the_commissioned_station_completes_a_routed_read() {
    // 006-routed-commissioning/SR-001
    let (router, hop) = peer();
    let worker = thread::spawn(move || {
        let (request, src) = read_frame(&router);
        assert_eq!(&request[4..11], &[1, 0x24, 0, 200, 1, 42, 255]);
        let invoke = request[13];
        let bad = [0x60, invoke, 9];
        for header in [
            &[1, 0][..],
            &[1, 8, 0, 201, 1, 42],
            &[1, 8, 0, 200, 1, 43],
            &[1, 0x88, 0, 200, 1, 42, 0],
        ] {
            router
                .send_to(&frame(10, &[header, &bad].concat()), src)
                .unwrap();
        }
        let source = &[1, 8, 0, 200, 1, 42][..];
        router
            .send_to(&frame(11, &[source, &bad].concat()), src)
            .unwrap();
        peer()
            .0
            .send_to(&frame(10, &[source, &bad].concat()), src)
            .unwrap();
        let body = [12, 0, 0, 0, 1, 25, 85, 0x3e, 0x44, 0x41, 0xac, 0, 0, 0x3f];
        for (seq, part) in [&body[..6], &body[6..]].into_iter().enumerate() {
            let segment = [if seq == 0 { 0x3c } else { 0x38 }, invoke, seq as u8, 1, 12];
            router
                .send_to(&frame(10, &[source, &segment, part].concat()), src)
                .unwrap();
            let (ack, from) = read_frame(&router);
            assert_eq!(from, src);
            assert_eq!(
                &ack[4..],
                &[1, 0x20, 0, 200, 1, 42, 255, 0x40, invoke, seq as u8, 1]
            );
        }
    });
    let mut client = Client::via(
        Ipv4Addr::LOCALHOST,
        hop,
        "200:42".parse().unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();
    let point = ObjectIdentifier::new(ObjectType::ANALOG_INPUT, 1).unwrap();
    assert_eq!(
        client.read(point, Prop::PRESENT_VALUE, None).unwrap().value,
        [0x44, 0x41, 0xac, 0, 0]
    );
    worker.join().unwrap();
}

fn announcement(id: u8) -> Vec<u8> {
    frame(
        4,
        &[
            127, 0, 0, 9, 0xba, 0xc0, 1, 8, 0, 200, 1, 42, 0x10, 0, 0xc4, 0x02, 0, 0, id, 0x22,
            0x05, 0xc4, 0x91, 3, 0x21, 9,
        ],
    )
}
#[test]
fn foreign_delivery_requires_a_live_lease_and_preserves_provenance() {
    // 006-routed-commissioning/SR-002
    let (bbmd, hop) = peer();
    let worker = thread::spawn(move || {
        let (request, src) = read_frame(&bbmd);
        assert_eq!(&request, &[0x81, 5, 0, 6, 0, 1]);
        let ack = frame(0, &[0, 0]);
        bbmd.send_to(&announcement(1), src).unwrap();
        peer().0.send_to(&ack, src).unwrap();
        bbmd.send_to(&ack, src).unwrap();
        peer().0.send_to(&announcement(2), src).unwrap();
        let mut bad = announcement(3);
        bad[19] = 0; // Analog Input in an I-Am.
        bbmd.send_to(&bad, src).unwrap();
        bbmd.send_to(&announcement(77), src).unwrap();
        let (_, src) = read_frame(&bbmd);
        bbmd.send_to(&frame(0, &[0, 0x30]), src).unwrap();
        let (_, src) = read_frame(&bbmd);
        // Let the first proven lease expire before any successful renewal.
        thread::sleep(Duration::from_millis(120));
        bbmd.send_to(&announcement(4), src).unwrap();
        bbmd.send_to(&ack, src).unwrap();
        bbmd.send_to(&announcement(78), src).unwrap();
    });
    let mut fd = ForeignListener::bind(Ipv4Addr::LOCALHOST, hop, 1).unwrap();
    let end = Instant::now() + Duration::from_secs(4);
    let mut ids = Vec::new();
    let mut expired = false;
    while let Some(event) = fd.next_until(end).unwrap() {
        match event {
            ForeignEvent::Registration(RegistrationEvent::Lapsed) => expired = true,
            ForeignEvent::Announcement(a) => {
                assert_eq!(a.bbmd, hop);
                assert_eq!(a.origin, "127.0.0.9:47808".parse().unwrap());
                assert_eq!(a.station.unwrap().to_string(), "200:0x2a");
                ids.push(a.device.object_identifier.instance_number());
                if ids.len() == 2 {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(expired);
    assert_eq!(ids, [77, 78]);
    worker.join().unwrap();
}
#[test]
fn unrelated_traffic_respects_the_listener_deadline() {
    // 006-routed-commissioning/SR-003
    let (bbmd, hop) = peer();
    let (done, stop) = std::sync::mpsc::channel::<()>();
    let worker = thread::spawn(move || {
        let (_, src) = read_frame(&bbmd);
        let limit = Instant::now() + Duration::from_secs(3);
        while Instant::now() < limit && stop.try_recv().is_err() {
            bbmd.send_to(&announcement(77), src).unwrap();
            thread::sleep(Duration::from_millis(2));
        }
    });
    let mut fd = ForeignListener::bind(Ipv4Addr::LOCALHOST, hop, 30).unwrap();
    let start = Instant::now();
    assert!(
        fd.next_until(start + Duration::from_millis(80))
            .unwrap()
            .is_none()
    );
    done.send(()).unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    worker.join().unwrap();
}

#[test]
fn route_syntax_bounds_the_network_and_preserves_leading_zero_octets() {
    let station: RemoteStation = "65534:0x0002".parse().unwrap();
    assert_eq!(station.network(), 65534);
    assert_eq!(station.mac(), [0, 2]);
    assert_eq!(station.to_string(), "65534:0x0002");
    for bad in [
        "0:1",
        "65535:1",
        "2:256",
        "2:*",
        "2:0x",
        "2:0x1",
        "2:0x+1",
        "2:0x0102030405060708",
    ] {
        assert!(bad.parse::<RemoteStation>().is_err(), "{bad}");
    }
}
