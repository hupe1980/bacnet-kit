#![cfg(feature = "std")]
use bacnet_kit::{
    Device, ObjectIdentifier, ObjectType, PropertyIdentifier as Prop,
    client::{Client, WatchEvent, WatchOptions, WatchTrigger},
    wire::MAX_FRAME,
};
use bacnet_services::{
    common::BACnetPropertyValue,
    cov::{COVNotificationRequest, SubscribeCOVRequest},
};
use bytes::BytesMut;
use std::{
    net::{SocketAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};

fn peers() -> (UdpSocket, Client) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let SocketAddr::V4(addr) = socket.local_addr().unwrap() else {
        unreachable!()
    };
    (
        socket,
        Client::connect(addr, Duration::from_secs(1)).unwrap(),
    )
}
fn recv(peer: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut data = [0; MAX_FRAME + 1];
    let (n, addr) = peer.recv_from(&mut data).unwrap();
    (data[..n].to_vec(), addr)
}
fn send(peer: &UdpSocket, addr: SocketAddr, apdu: &[u8]) {
    let [hi, lo] = ((apdu.len() + 6) as u16).to_be_bytes();
    peer.send_to(&[&[0x81, 10, hi, lo, 1, 0], apdu].concat(), addr)
        .unwrap();
}
fn read(peer: &UdpSocket, device: &Device, data: &[u8], addr: SocketAddr) {
    assert_eq!(data[9], 12);
    let mut out = [0; MAX_FRAME];
    let n = device.respond(data, &mut out).unwrap().unwrap();
    peer.send_to(&out[..n], addr).unwrap();
}
fn sample(peer: &UdpSocket, device: &Device) {
    for _ in 0..4 {
        let (data, addr) = recv(peer);
        read(peer, device, &data, addr);
    }
}
fn control(peer: &UdpSocket) -> (SubscribeCOVRequest, u8, SocketAddr) {
    let (data, addr) = recv(peer);
    assert_eq!(data[9], 5);
    (
        SubscribeCOVRequest::decode(&data[10..]).unwrap(),
        data[8],
        addr,
    )
}
fn cancel(peer: &UdpSocket, subscription: &SubscribeCOVRequest, addr: SocketAddr) {
    let (req, id, source) = control(peer);
    assert!(req.is_cancellation());
    assert_eq!(
        req.subscriber_process_identifier,
        subscription.subscriber_process_identifier
    );
    assert_eq!(
        req.monitored_object_identifier,
        subscription.monitored_object_identifier
    );
    assert_eq!(source, addr);
    send(peer, addr, &[0x20, id, 5]);
}
fn note(req: &SubscribeCOVRequest) -> COVNotificationRequest {
    let property = |id, value| BACnetPropertyValue {
        property_identifier: id,
        property_array_index: None,
        value,
        priority: None,
    };
    COVNotificationRequest {
        subscriber_process_identifier: req.subscriber_process_identifier,
        initiating_device_identifier: ObjectIdentifier::new(ObjectType::DEVICE, 123).unwrap(),
        monitored_object_identifier: req.monitored_object_identifier,
        time_remaining: 1,
        list_of_values: vec![
            property(Prop::PRESENT_VALUE, vec![0x44, 0x42, 0xc6, 0, 0]),
            property(Prop::STATUS_FLAGS, vec![0x82, 4, 0]),
        ],
    }
}
fn notify(peer: &UdpSocket, addr: SocketAddr, confirmed: bool, n: &COVNotificationRequest) {
    let mut b = BytesMut::from(if confirmed {
        &[0, 5, 42, 1][..]
    } else {
        &[0x10, 2]
    });
    n.encode(&mut b);
    send(peer, addr, &b);
}
fn end() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn only_matching_notifications_trigger_verified_readback() {
    // 005-cov-watch/SR-001
    let (peer, client) = peers();
    let (ready, wait) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let device = Device::new(123, 0, 21.5).unwrap();
        sample(&peer, &device);
        let (req, id, addr) = control(&peer);
        assert_eq!(req.issue_confirmed_notifications, Some(true));
        let good = note(&req);
        send(&peer, addr, &[0x20, id.wrapping_add(1), 5]);
        send(&peer, addr, &[0x20, id, 12]);
        let foreign = UdpSocket::bind("127.0.0.1:0").unwrap();
        send(&foreign, addr, &[0x20, id, 5]);
        notify(&foreign, addr, true, &good);
        notify(&peer, addr, false, &good);
        for field in 0..3 {
            let mut bad = good.clone();
            match field {
                0 => {
                    bad.subscriber_process_identifier =
                        bad.subscriber_process_identifier.wrapping_add(1)
                }
                1 => {
                    bad.initiating_device_identifier =
                        ObjectIdentifier::new(ObjectType::DEVICE, 124).unwrap()
                }
                _ => {
                    bad.monitored_object_identifier =
                        ObjectIdentifier::new(ObjectType::ANALOG_INPUT, 2).unwrap()
                }
            }
            notify(&peer, addr, true, &bad);
            assert_eq!(&recv(&peer).0[6..], &[0x50, 42, 1, 0x91, 5, 0x91, 79]);
        }
        send(&peer, addr, &[0x0c, 5, 42, 0, 1, 1, 0]);
        assert_eq!(&recv(&peer).0[6..], &[0x71, 42, 4]);
        let mut bad = good.clone();
        bad.list_of_values.pop();
        notify(&peer, addr, true, &bad);
        assert_eq!(recv(&peer).0[6], 0x50);
        send(&peer, addr, &[0, 5, 42, 1, 0]);
        assert_eq!(recv(&peer).0[6], 0x60);
        send(&peer, addr, &[0x20, id, 5]);
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        notify(&peer, addr, true, &good);
        assert_eq!(&recv(&peer).0[6..], &[0x20, 42, 1]);
        sample(&peer, &device);
        cancel(&peer, &req, addr);
    });
    let mut w = client
        .watch(
            123,
            1,
            WatchOptions {
                confirmed: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(
        w.next_until(end()).unwrap(),
        Some(WatchEvent::Subscribed)
    ));
    assert!(
        w.next_until(Instant::now() + Duration::from_millis(30))
            .unwrap()
            .is_none()
    );
    ready.send(()).unwrap();
    let Some(WatchEvent::Observation {
        trigger: WatchTrigger::Notification,
        sample,
        ..
    }) = w.next_until(end()).unwrap()
    else {
        panic!("missing notification")
    };
    assert_eq!(sample.value, 21.5); // Payload claimed 99. Only a readback can supply a sample.
    w.cancel().unwrap();
    worker.join().unwrap();
}

#[test]
fn failed_verification_preserves_time_and_recovery_marks_gap() {
    // 005-cov-watch/SR-003
    let (peer, client) = peers();
    let worker = thread::spawn(move || {
        let mut device = Device::new(123, 0, 21.5).unwrap();
        sample(&peer, &device);
        let (req, id, addr) = control(&peer);
        send(&peer, addr, &[0x20, id, 5]);
        for _ in 0..2 {
            let (q, src) = recv(&peer);
            read(&peer, &device, &q, src);
        }
        let (units, src) = recv(&peer);
        assert_eq!(units.last(), Some(&117));
        send(&peer, src, &[0x50, units[8], 12, 0x91, 2, 0x91, 32]);
        device.set_fault(true);
        sample(&peer, &device);
        cancel(&peer, &req, addr);
    });
    let age = Duration::from_millis(240);
    let mut w = client
        .watch(
            123,
            1,
            WatchOptions {
                max_age: age,
                ..Default::default()
            },
        )
        .unwrap();
    let baseline = w.latest().observed_at;
    assert!(matches!(
        w.next_until(end()).unwrap(),
        Some(WatchEvent::Subscribed)
    ));
    assert!(matches!(
        w.next_until(end()).unwrap(),
        Some(WatchEvent::VerificationFailed(_))
    ));
    assert_eq!(w.latest().observed_at, baseline);
    let Some(WatchEvent::Observation {
        sample,
        gap: true,
        trigger: WatchTrigger::Verification,
    }) = w.next_until(end()).unwrap()
    else {
        panic!("missing recovery")
    };
    assert!(sample.flags.fault && sample.observed_at > baseline);
    w.cancel().unwrap();
    worker.join().unwrap();
}

#[test]
fn burst_coalesces_and_renewal_still_runs() {
    // 005-cov-watch/SR-002
    let (peer, client) = peers();
    let worker = thread::spawn(move || {
        let device = Device::new(123, 0, 21.5).unwrap();
        sample(&peer, &device);
        let (req, id, addr) = control(&peer);
        send(&peer, addr, &[0x20, id, 5]);
        let sender = peer.try_clone().unwrap();
        let writer = thread::spawn(move || {
            let n = note(&req);
            for _ in 0..100 {
                notify(&sender, addr, false, &n);
                thread::sleep(Duration::from_millis(5));
            }
        });
        let (mut reads, mut renewals) = (0, 0);
        loop {
            let (data, src) = recv(&peer);
            if data[9] == 12 {
                reads += 1;
                read(&peer, &device, &data, src);
            } else {
                let req = SubscribeCOVRequest::decode(&data[10..]).unwrap();
                send(&peer, src, &[0x20, data[8], 5]);
                if req.is_cancellation() {
                    break;
                }
                renewals += 1;
            }
        }
        writer.join().unwrap();
        (reads, renewals)
    });
    let mut watch = client
        .watch(
            123,
            1,
            WatchOptions {
                lease_seconds: 1,
                ..Default::default()
            },
        )
        .unwrap();
    let until = Instant::now() + Duration::from_millis(800);
    let mut received = 0;
    while let Some(e) = watch.next_until(until).unwrap() {
        if matches!(
            e,
            WatchEvent::Observation {
                trigger: WatchTrigger::Notification,
                ..
            }
        ) {
            received += 1;
        }
    }
    watch.cancel().unwrap();
    let (reads, renewals) = worker.join().unwrap();
    assert_eq!(renewals, 1);
    assert!((1..=8).contains(&received));
    assert_eq!(reads, 4 * received);
}

#[test]
fn lost_subscription_ack_still_allows_explicit_cancellation() {
    let (peer, client) = peers();
    let worker = thread::spawn(move || {
        sample(&peer, &Device::new(123, 0, 21.5).unwrap());
        let (req, _, addr) = control(&peer);
        cancel(&peer, &req, addr);
    });
    let mut watch = client
        .watch(
            123,
            1,
            WatchOptions {
                lease_seconds: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(
        watch.next_until(end()).unwrap(),
        Some(WatchEvent::SubscriptionFailed(
            bacnet_kit::client::Error::Timeout
        ))
    ));
    watch.cancel().unwrap();
    worker.join().unwrap();
}
