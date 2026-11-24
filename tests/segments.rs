//! UDP transactions with original byte fixtures for loss, refusal and correlation.
#![cfg(feature = "std")]
use bacnet_kit::{
    ObjectIdentifier, ObjectType, PropertyIdentifier as Prop, ReadPropertyRequest,
    client::{Client, Error, MAX_SEGMENTS},
};
use std::{
    net::{SocketAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};

fn setup(timeout: Duration) -> (UdpSocket, Client) {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let SocketAddr::V4(addr) = peer.local_addr().unwrap() else {
        unreachable!()
    };
    (peer, Client::connect(addr, timeout).unwrap())
}

fn recv(peer: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut data = [0; 1500];
    let (n, addr) = peer.recv_from(&mut data).unwrap();
    assert_eq!(u16::from_be_bytes([data[2], data[3]]) as usize, n);
    (data[6..n].to_vec(), addr)
}
fn send(peer: &UdpSocket, addr: SocketAddr, apdu: &[u8]) {
    let len = (6 + apdu.len()) as u16;
    let [hi, lo] = len.to_be_bytes();
    peer.send_to(&[&[0x81, 10, hi, lo, 1, 0][..], apdu].concat(), addr)
        .unwrap();
}
fn segment(seq: u8, more: bool, data: &[u8]) -> Vec<u8> {
    [&[if more { 0x3c } else { 0x38 }, 0, seq, 4, 12][..], data].concat()
}
fn point() -> ObjectIdentifier {
    ObjectIdentifier::new(ObjectType::ANALOG_INPUT, 1).unwrap()
}
const READ_BODY: &[u8] = &[12, 0, 0, 0, 1, 25, 85, 0x3e, 0x44, 0x41, 0xac, 0, 0, 0x3f];

#[test]
fn correlated_reassembly_handles_gap_and_duplicate() {
    // 004-segmented-reads/SR-001
    let (peer, mut client) = setup(Duration::from_secs(2));
    let worker = thread::spawn(move || {
        let (_, addr) = recv(&peer);
        let first = segment(0, true, &READ_BODY[..6]);
        for (offset, wrong) in [(1, 4), (4, 14)] {
            let mut noise = first.clone();
            noise[offset] = wrong;
            send(&peer, addr, &noise);
        }
        let mut foreign = first.clone();
        foreign[5] = 0xff;
        send(&UdpSocket::bind("127.0.0.1:0").unwrap(), addr, &foreign);
        send(&peer, addr, &first);
        assert_eq!(recv(&peer).0, [0x40, 0, 0, 1]);
        send(&peer, addr, &first);
        assert_eq!(recv(&peer).0, [0x40, 0, 0, 1]);
        send(&peer, addr, &segment(2, false, &READ_BODY[9..]));
        assert_eq!(recv(&peer).0, [0x42, 0, 0, 1]);
        let mut premature = [&[0x30, 0, 12][..], READ_BODY].concat();
        premature[12..16].copy_from_slice(&99f32.to_be_bytes());
        send(&peer, addr, &premature);
        send(&peer, addr, &segment(1, false, &READ_BODY[6..]));
        assert_eq!(recv(&peer).0, [0x40, 0, 1, 1]);
    });
    let value = client.read(point(), Prop::PRESENT_VALUE, None).unwrap();
    assert_eq!(value.value, [0x44, 0x41, 0xac, 0, 0]);
    worker.join().unwrap();
}

#[test]
fn refuses_invalid_window_and_mismatched_final_reference() {
    for bad_window in [true, false] {
        let (peer, mut client) = setup(Duration::from_secs(2));
        let worker = thread::spawn(move || {
            let (_, addr) = recv(&peer);
            let mut ack = segment(0, false, READ_BODY);
            if bad_window {
                ack[3] = 128;
            } else {
                ack[11] = 77;
            } // wrong property
            send(&peer, addr, &ack);
            assert_eq!(recv(&peer).0, [0x70, 0, if bad_window { 7 } else { 2 }]);
        });
        assert!(matches!(
            client.read(point(), Prop::PRESENT_VALUE, None),
            Err(Error::Invalid(_))
        ));
        worker.join().unwrap();
    }
}

#[test]
fn reassembly_has_a_fixed_limit_and_releases_state() {
    // 004-segmented-reads/SR-002
    let (peer, mut client) = setup(Duration::from_secs(2));
    let worker = thread::spawn(move || {
        let (_, addr) = recv(&peer);
        for seq in 0..MAX_SEGMENTS {
            send(&peer, addr, &segment(seq, true, &[1; 1471]));
            let control = recv(&peer).0;
            if seq + 1 == MAX_SEGMENTS {
                assert_eq!(control, [0x70, 0, 1]);
            } else {
                assert_eq!(control, [0x40, 0, seq, 1]);
            }
        }
        let (req, addr) = recv(&peer);
        assert_eq!(req[2], 1);
        send(&peer, addr, &[&[0x30, 1, 12][..], READ_BODY].concat());
    });
    assert!(matches!(
        client.read(point(), Prop::PRESENT_VALUE, None),
        Err(Error::Invalid(_))
    ));
    assert!(client.read(point(), Prop::PRESENT_VALUE, None).is_ok());
    worker.join().unwrap();
}

#[test]
fn duplicate_segments_do_not_extend_the_read_deadline() {
    let (peer, mut client) = setup(Duration::from_millis(100));
    let worker = thread::spawn(move || {
        let (_, addr) = recv(&peer);
        for _ in 0..100 {
            send(&peer, addr, &segment(0, true, &READ_BODY[..6]));
            thread::sleep(Duration::from_millis(10));
        }
    });
    let before = Instant::now();
    assert!(matches!(
        client.read(point(), Prop::PRESENT_VALUE, None),
        Err(Error::Timeout)
    ));
    assert!(before.elapsed() < Duration::from_millis(650));
    worker.join().unwrap();
}

fn probes(peer: &UdpSocket, direction: u8, count: u8) {
    let mut props = vec![(62, [0x21, 50]), (107, [0x91, direction])];
    if direction == 2 {
        props.push((167, [0x21, count]));
    }
    for (prop, value) in props {
        let (req, addr) = recv(peer);
        assert_eq!(&req[9..], &[25, prop]);
        send(
            peer,
            addr,
            &[&[0x30, req[2], 12][..], &req[4..], &[0x3e], &value, &[0x3f]].concat(),
        );
    }
}
fn batch_refs() -> Vec<ReadPropertyRequest> {
    vec![
        ReadPropertyRequest {
            object_identifier: point(),
            property_identifier: Prop::PRESENT_VALUE,
            property_array_index: None
        };
        24
    ]
}

#[test]
fn retransmits_only_the_outstanding_segment_then_accepts_early_result() {
    // 004-segmented-reads/SR-003
    let (peer, mut client) = setup(Duration::from_secs(3));
    let worker = thread::spawn(move || {
        probes(&peer, 2, 2);
        let (first, addr) = recv(&peer);
        assert_eq!(first.len(), 50);
        assert_eq!(&first[..6], &[0x0e, 0x45, 3, 0, 1, 14]);
        assert_eq!(recv(&peer).0, first); // lost first acknowledgement
        send(&peer, addr, &[0x41, 3, 0, 1]);
        let (last, _) = recv(&peer);
        assert_eq!(&last[..6], &[0x0a, 0x45, 3, 1, 1, 14]);
        send(&peer, addr, &[0x43, 3, 0, 1]); // last received in order is zero
        assert_eq!(recv(&peer).0, last);
        // A completed read can arrive before its final request SegmentACK.
        let mut ack = vec![0x30, 3, 14, 12, 0, 0, 0, 1, 0x1e];
        for _ in 0..24 {
            ack.extend([0x29, 85, 0x4e, 0x44, 0x41, 0xac, 0, 0, 0x4f]);
        }
        ack.push(0x1f);
        send(&peer, addr, &ack);
    });
    assert_eq!(client.negotiate(77).unwrap().max_request_segments, 2);
    let rows = client.read_multiple(&batch_refs()).unwrap();
    assert_eq!(rows.len(), 24);
    assert!(
        rows.iter()
            .all(|r| r.outcome.as_ref().unwrap().value == [0x44, 0x41, 0xac, 0, 0])
    );
    worker.join().unwrap();
}

#[test]
fn refuses_oversized_request_to_transmit_only_peer_before_sending() {
    let (peer, mut client) = setup(Duration::from_secs(2));
    let worker = thread::spawn(move || {
        probes(&peer, 1, 0);
        peer.set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        assert!(peer.recv_from(&mut [0; 128]).is_err());
    });
    assert_eq!(client.negotiate(77).unwrap().max_request_segments, 0);
    assert!(matches!(
        client.read_multiple(&batch_refs()),
        Err(Error::Invalid(_))
    ));
    worker.join().unwrap();
}

#[test]
fn negative_acknowledgements_exhaust_the_retry_budget() {
    // 004-segmented-reads/SR-003
    let (peer, mut client) = setup(Duration::from_secs(3));
    let worker = thread::spawn(move || {
        probes(&peer, 2, 2);
        let (first, addr) = recv(&peer);
        for _ in 0..2 {
            send(&peer, addr, &[0x43, 4, 255, 1]);
            assert_eq!(recv(&peer).0, first);
        }
        send(&peer, addr, &[0x43, 4, 255, 1]);
        assert_eq!(recv(&peer).0, [0x70, 4, 10]);
    });
    client.negotiate(77).unwrap();
    // Same peer could accept the short two-segment request, but this one would need more.
    let mut large = batch_refs();
    for r in large.iter_mut().step_by(2) {
        r.object_identifier = ObjectIdentifier::new(ObjectType::DEVICE, 77).unwrap();
    }
    assert!(matches!(
        client.read_multiple(&large),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        client.read_multiple(&batch_refs()),
        Err(Error::Timeout)
    ));
    worker.join().unwrap();
}
