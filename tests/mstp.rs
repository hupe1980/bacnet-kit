use bacnet_kit::mstp::{FrameError, FrameType, InspectError, MAX_RECORD, inspect};

const TOKEN: [u8; 8] = [0x55, 0xff, 0, 1, 2, 0, 0, 0x40];
const STANDARD: &[u8] = include_bytes!("fixtures/mstp/standard-501.bin");
const EXTENDED_MIN: &[u8] = include_bytes!("fixtures/mstp/extended-502.bin");
const EXTENDED_MAX: &[u8] = include_bytes!("fixtures/mstp/extended-1497.bin");

// 010-mstp-frame-inspection/SR-003
#[test]
fn captured_token_and_independent_c_frames_agree() {
    for (raw, kind, data) in [
        (&TOKEN[..], 0, 0),
        (STANDARD, 6, 501),
        (EXTENDED_MIN, 32, 502),
        (EXTENDED_MAX, 33, 1497),
    ] {
        for padded in [false, true] {
            let mut record = raw.to_vec();
            if padded {
                record.push(0xff);
            }
            let o = inspect(&mut record).unwrap();
            assert_eq!(o.header.frame_type.to_raw(), kind);
            assert_eq!((o.header.source, o.header.destination), (2, 1));
            assert_eq!(
                (o.data_octets, o.wire_octets, o.padded),
                (data, record.len(), padded)
            );
        }
    }
    assert_eq!(EXTENDED_MAX.len() + 1, MAX_RECORD);
}

// 010-mstp-frame-inspection/SR-001
#[test]
fn enforces_complete_records_and_separate_integrity_checks() {
    for raw in [&TOKEN[..], STANDARD, EXTENDED_MIN, EXTENDED_MAX] {
        for end in 0..raw.len() {
            assert!(inspect(&mut raw[..end].to_vec()).is_err());
        }
        let mut header = raw.to_vec();
        header[7] ^= 1;
        assert_eq!(
            inspect(&mut header),
            Err(InspectError::Wire(FrameError::HeaderCrcMismatch))
        );
        if raw.len() > 8 {
            let mut data = raw.to_vec();
            *data.last_mut().unwrap() ^= 1;
            assert!(matches!(
                inspect(&mut data),
                Err(InspectError::Wire(FrameError::DataCrcMismatch { .. }))
            ));
        }
    }
    for tail in [&[0][..], &[0xff, 0xff], &TOKEN] {
        let mut record = TOKEN.to_vec();
        record.extend(tail);
        assert_eq!(inspect(&mut record), Err(InspectError::TrailingBytes));
    }
    assert_eq!(
        inspect(&mut vec![0; MAX_RECORD + 1]),
        Err(InspectError::TooLarge)
    );
}

fn set_header(record: &mut [u8], index: usize, value: u8) {
    record[index] = value;
    record[7] = bacnet_mstp_codec::crc::crc8(&record[2..7]);
}

#[test]
fn distinguishes_supported_frames_from_valid_but_out_of_scope_records() {
    for kind in 0..=7 {
        let mut empty = TOKEN;
        set_header(&mut empty, 2, kind);
        assert_eq!(
            inspect(&mut empty).unwrap().header.frame_type.to_raw(),
            kind
        );
    }
    for kind in [0, 1, 2, 7] {
        let mut record = STANDARD.to_vec();
        set_header(&mut record, 2, kind);
        assert!(matches!(
            inspect(&mut record),
            Err(InspectError::InvalidDataLength { octets: 501, .. })
        ));
    }
    for (bytes, kind) in [(&TOKEN[..], 8), (EXTENDED_MIN, 34), (STANDARD, 128)] {
        let mut record = bytes.to_vec();
        set_header(&mut record, 2, kind);
        assert_eq!(
            inspect(&mut record),
            Err(InspectError::Unsupported(FrameType::from_raw(kind)))
        );
    }
    let mut record = TOKEN;
    set_header(&mut record, 4, 255);
    assert_eq!(
        inspect(&mut record),
        Err(InspectError::Wire(FrameError::BroadcastSource))
    );
    // The upstream envelope decoder accepts short COBS frames. A BACnet extended data frame starts at 502.
    let mut wire = [0; 64];
    let short = bacnet_mstp_codec::Frame::new(
        FrameType::BACNET_EXTENDED_DATA_EXPECTING_REPLY,
        1,
        2,
        b"short",
    );
    let n = short.encode_into(&mut wire).unwrap();
    assert!(matches!(
        inspect(&mut wire[..n]),
        Err(InspectError::InvalidDataLength { octets: 5, .. })
    ));
}
