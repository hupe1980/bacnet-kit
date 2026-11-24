#![no_std]
#![no_main]

use bacnet_kit::{Device, commands::CommandSlots};
use core::{hint::black_box, panic::PanicInfo};
use cortex_m_rt::entry;
use cortex_m_semihosting::debug;

#[entry]
fn main() -> ! {
    // Directed ReadProperty, invoke ID 42, Analog Input 1, Present_Value.
    let request = [0x81, 10, 0, 17, 1, 4, 0, 5, 42, 12, 12, 0, 0, 0, 1, 25, 85];
    let expected = [
        0x81, 10, 0, 23, 1, 0, 0x30, 42, 12, 12, 0, 0, 0, 1, 25, 85, 0x3e, 0x44, 0x41, 0xac, 0, 0,
        0x3f,
    ];
    let mut reply = [0; 128];
    let device = Device::new(123, 0, black_box(21.5)).unwrap();
    let n = device
        .respond(black_box(&request), &mut reply)
        .unwrap()
        .unwrap();
    assert_eq!(black_box(&reply[..n]), expected);
    // Independent WriteProperty fixture: set Analog Value 1 to 22 at priority 8.
    let write = [
        0x81, 10, 0, 26, 1, 4, 0, 5, 42, 15, 12, 0, 0x80, 0, 1, 25, 85, 0x3e, 0x44, 0x41, 0xb0, 0,
        0, 0x3f, 0x49, 8,
    ];
    let mut slots = CommandSlots::default();
    let n = device
        .respond_with_commands(black_box(&write), &mut reply, &mut slots, |cmd| {
            cmd.priority() == 8
        })
        .unwrap()
        .unwrap();
    assert_eq!(&reply[..n], &[0x81, 10, 0, 9, 1, 0, 0x20, 42, 15]);
    let mut av = request;
    av[12] = 0x80;
    let mut expected_av = expected;
    expected_av[11] = 0x80;
    expected_av[19] = 0xb0;
    let n = device
        .respond_with_commands(black_box(&av), &mut reply, &mut slots, |_| false)
        .unwrap()
        .unwrap();
    assert_eq!(black_box(&reply[..n]), expected_av);
    // Mixed ReadPropertyMultiple: Present_Value followed by absent Description.
    let rpm = [
        0x81, 10, 0, 21, 1, 4, 0, 5, 42, 14, 12, 0, 0, 0, 1, 0x1e, 9, 85, 9, 28, 0x1f,
    ];
    let n = device
        .respond(black_box(&rpm), &mut reply)
        .unwrap()
        .unwrap();
    assert_eq!(
        black_box(&reply[..n]),
        &[
            0x81, 10, 0, 33, 1, 0, 0x30, 42, 14, 12, 0, 0, 0, 1, 0x1e, 0x29, 85, 0x4e, 0x44, 0x41,
            0xac, 0, 0, 0x4f, 0x29, 28, 0x5e, 0x91, 2, 0x91, 32, 0x5f, 0x1f
        ]
    );
    // Decode the independent C extended frame in caller-owned scratch on Cortex-M4.
    let mut capture = *include_bytes!("../../../tests/fixtures/mstp/extended-502.bin");
    let observed = bacnet_kit::mstp::inspect(black_box(&mut capture)).unwrap();
    assert_eq!(
        observed.header.frame_type,
        bacnet_kit::mstp::FrameType::BACNET_EXTENDED_DATA_EXPECTING_REPLY
    );
    assert_eq!(
        (observed.header.source, observed.header.destination),
        (2, 1)
    );
    assert_eq!((observed.data_octets, observed.wire_octets), (502, 517));
    assert!(!observed.padded);
    finish(debug::EXIT_SUCCESS)
}

fn finish(status: debug::ExitStatus) -> ! {
    debug::exit(status);
    loop {
        core::hint::spin_loop()
    }
}

#[panic_handler]
fn panic(_: &PanicInfo<'_>) -> ! {
    finish(debug::EXIT_FAILURE)
}
