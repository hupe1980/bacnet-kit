use bacnet_kit::{
    DEFAULT_UDP_PORT, Device, ObjectIdentifier, ObjectType, PropertyIdentifier as Prop,
    ReadPropertyRequest,
    client::{
        Client, ForeignEvent, ForeignListener, Reading, RegistrationEvent, RemoteStation, Sample,
        WatchEvent, WatchOptions, WatchTrigger,
    },
    commands::{Command, CommandSlots},
    wire::MAX_FRAME,
};
use std::{
    error::Error,
    io::{self, Write},
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

#[path = "bin_support/cli.rs"]
mod cli;
use cli::{Cli, Operation, PropertyRef};

#[path = "bin_support/mstp.rs"]
mod mstp;
#[path = "bin_support/pics.rs"]
mod pics;
#[cfg(feature = "sc-pki")]
#[path = "bin_support/sc_check.rs"]
mod sc_check;
#[cfg(feature = "sc-node")]
#[path = "bin_support/sc_read.rs"]
mod sc_read;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_BIND: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, DEFAULT_UDP_PORT);
fn main() -> ExitCode {
    let result = match Cli::parse_command() {
        Some(command) => run(command),
        None => Cli::help(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bacnet-kit: {e}");
            ExitCode::FAILURE
        }
    }
}

fn status_text(flags: bacnet_kit::client::StatusFlags) -> String {
    let names = [
        (flags.in_alarm, "in-alarm"),
        (flags.fault, "fault"),
        (flags.overridden, "overridden"),
        (flags.out_of_service, "out-of-service"),
    ];
    let active: Vec<_> = names
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| *name)
        .collect();
    if active.is_empty() {
        "normal".into()
    } else {
        active.join(", ")
    }
}
fn unit_text(unit: u32) -> String {
    match unit {
        62 => "°C".into(),
        64 => "°F".into(),
        n => bacnet_types::enums::EngineeringUnits::from_raw(n)
            .name()
            .map(|name| name.to_ascii_lowercase().replace('_', " "))
            .unwrap_or_else(|| format!("unit {n}")),
    }
}
fn output(instance: u32, s: Sample) {
    let unit = unit_text(s.unit);
    println!(
        "Analog Input {instance}   {} {unit}\nStatus           {}",
        s.value,
        status_text(s.flags)
    );
}
fn serve_one(
    socket: &UdpSocket,
    device: &Device,
    slots: Option<&mut CommandSlots>,
    buf: &mut [u8],
    out: &mut [u8],
    deliver: impl FnOnce(&[u8]) -> bool,
) -> io::Result<()> {
    let (n, addr) = socket.recv_from(buf)?;
    let response = match slots {
        Some(slots) => device.respond_with_commands(&buf[..n], out, slots, |c| {
            addr.ip().is_loopback() && matches!(c.priority(), 8 | 16)
        }),
        None => device.respond(&buf[..n], out),
    };
    if let Ok(Some(len)) = response
        && deliver(&out[..len])
    {
        socket.send_to(&out[..len], addr)?;
    }
    Ok(())
}
#[derive(Clone, Copy)]
enum Demo {
    Temperature,
    Pics { commands: bool },
    Batch,
    Commands { lose_ack: bool },
}

fn write_ack(frame: &[u8], invoke: u8) -> bool {
    frame == [0x81, 10, 0, 9, 1, 0, 0x20, invoke, 15]
}

fn demo(mode: Demo) -> Result<()> {
    let commands = matches!(mode, Demo::Commands { .. } | Demo::Pics { commands: true });
    let lose_ack = matches!(mode, Demo::Commands { lose_ack: true });
    with_fixture(commands, lose_ack, |peer| match mode {
        Demo::Pics { commands } => {
            let report = pics::worksheet(&mut Client::connect(peer, TIMEOUT)?, commands)?;
            io::stdout().lock().write_all(report.as_bytes())?;
            Ok(())
        }
        Demo::Batch => batch(
            Client::connect(peer, TIMEOUT)?,
            &[
                "analog-input:1/present-value".parse()?,
                "analog-input:1/description".parse()?,
                "device:123/object-list/0".parse()?,
            ],
            None,
        )
        .and_then(|failed| {
            if failed == 1 {
                Ok(())
            } else {
                Err("unexpected fixture results".into())
            }
        }),
        Demo::Commands { lose_ack } => command_trace(peer, lose_ack),
        Demo::Temperature => Client::connect(peer, TIMEOUT)
            .and_then(|mut c| c.sample(1))
            .map(|s| output(1, s))
            .map_err(Into::into),
    })
}

fn with_fixture<T>(
    commands: bool,
    lose_ack: bool,
    run: impl FnOnce(SocketAddrV4) -> Result<T>,
) -> Result<T> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let std::net::SocketAddr::V4(peer) = socket.local_addr()? else {
        unreachable!()
    };
    socket.set_read_timeout(Some(Duration::from_millis(50)))?;
    let device = Device::new(123, 0, 21.5)?;
    let (done, stop) = std::sync::mpsc::channel::<()>();
    let worker = thread::spawn(move || -> io::Result<()> {
        let mut buf = [0; MAX_FRAME + 1];
        let mut out = [0; MAX_FRAME];
        let mut slots = commands.then(CommandSlots::default);
        let mut suppress = lose_ack;
        while matches!(stop.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)) {
            match serve_one(
                &socket,
                &device,
                slots.as_mut(),
                &mut buf,
                &mut out,
                |reply| {
                    if suppress && write_ack(reply, 2) {
                        suppress = false;
                        return false;
                    }
                    true
                },
            ) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) => {}
                r => r?,
            }
        }
        Ok(())
    });
    let result = run(peer);
    drop(done);
    worker.join().map_err(|_| "simulator thread failed")??;
    result
}
fn run(command: Operation) -> Result<()> {
    match command {
        #[cfg(feature = "sc-node")]
        Operation::ScRead(args) => sc_read::run(args),
        #[cfg(feature = "sc-pki")]
        Operation::ScCheck { files, min_days } => sc_check::run(files, min_days),
        Operation::MstpFrame(input) => mstp::run(input),
        Operation::Pics { commands } => demo(Demo::Pics { commands }),
        Operation::Demo => demo(Demo::Temperature),
        Operation::BatchDemo => demo(Demo::Batch),
        Operation::Batch {
            target,
            references,
            device,
        } => {
            let failed = batch(target.connect()?, &references, device)?;
            if failed > 0 {
                return Err(
                    format!("{failed} of {} property reads failed", references.len()).into(),
                );
            }
            Ok(())
        }
        Operation::CommandDemo { lose_ack } => demo(Demo::Commands { lose_ack }),
        Operation::Watch {
            target,
            device,
            input,
            confirmed,
            seconds,
            max_age,
            lease,
        } => watch(
            target.connect()?,
            device,
            input,
            WatchOptions {
                confirmed,
                max_age: Duration::from_secs(max_age.into()),
                lease_seconds: lease,
            },
            seconds,
        ),
        Operation::Foreign { bbmd, ttl, seconds } => foreign(bbmd, ttl, seconds),
        Operation::Inspect { target, device } => {
            let mut client = target.connect()?;
            let data = match device {
                Some(instance) => client.inventory_of(instance)?,
                None => client.inventory()?,
            };
            println!(
                "Device {}   ·   {} objects",
                data.device.instance_number(),
                data.objects.len()
            );
            for id in data.objects {
                println!("  {} {}", id.object_type(), id.instance_number())
            }
            Ok(())
        }
        Operation::Read { target, instance } => {
            output(instance, target.connect()?.sample(instance)?);
            Ok(())
        }
        Operation::Serve {
            bind: addr,
            fault,
            commands,
        } => {
            if commands && !addr.ip().is_loopback() {
                return Err("--commands requires a loopback IPv4 address".into());
            }
            let socket = UdpSocket::bind(addr)?;
            let mut slots = commands.then(CommandSlots::default);
            let mut device = Device::new(123, 0, 21.5)?;
            device.set_fault(fault);
            println!("Serving Device 123 on {}", socket.local_addr()?);
            if commands {
                println!("Analog Value 1 enabled · accepted priorities 8 and 16");
            }
            io::stdout().flush()?;
            let mut buf = [0; MAX_FRAME + 1];
            let mut out = [0; MAX_FRAME];
            loop {
                if let Err(e) =
                    serve_one(&socket, &device, slots.as_mut(), &mut buf, &mut out, |_| {
                        true
                    })
                    && e.kind() != io::ErrorKind::Interrupted
                {
                    return Err(e.into());
                }
            }
        }
    }
}

// Keep readback on a separate socket from the supervised write.
fn command_trace(peer: SocketAddrV4, lose_ack: bool) -> Result<()> {
    use bacnet_encoding::primitives::ApplicationValueRef as Value;
    let writer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    writer.connect(peer)?;

    let mut reader = Client::connect(peer, TIMEOUT)?;
    let point = ObjectIdentifier::new(ObjectType::ANALOG_VALUE, 1)?;
    println!("Analog Value 1 · simulated setpoint\nPriority  Action       Slot      Effective");
    for (invoke, (priority, value)) in [
        (16, Some(19.)),
        (8, Some(22.)),
        (16, Some(18.)),
        (8, None),
        (16, None),
    ]
    .into_iter()
    .enumerate()
    {
        let command = Command::new(priority, value)
            .map_err(|code| format!("invalid fixture command ({code})"))?;
        let mut buf = [0; MAX_FRAME + 1];
        let n = command.encode(invoke as u8, &mut buf)?;
        let expected_loss = lose_ack && invoke == 2;
        writer.set_read_timeout(Some(if expected_loss {
            Duration::from_millis(250)
        } else {
            TIMEOUT
        }))?;
        writer.send(&buf[..n])?;
        let acknowledged = match writer.recv(&mut buf) {
            Ok(n) if write_ack(&buf[..n], invoke as u8) => true,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                false
            }
            Ok(_) => return Err("unexpected response to fixture command".into()),
            Err(e) => return Err(e.into()),
        };
        let slot = reader.read(point, Prop::PRIORITY_ARRAY, Some(priority.into()))?;
        let display = match slot.scalar()? {
            Value::Null => "NULL".to_owned(),
            Value::Real(v) => format!("{v} °C"),
            _ => return Err("invalid priority slot readback".into()),
        };
        let reading = reader.read(point, Prop::PRESENT_VALUE, None)?;
        let Value::Real(effective) = reading.scalar()? else {
            return Err("invalid effective value readback".into());
        };
        let action = if value.is_some() { "Set" } else { "Relinquish" };
        println!("{priority:>8}  {action:<11}  {display:<8}  {effective} °C");
        if !acknowledged {
            println!("          ACK missing · slot and effective value read back without retry");
            if !expected_loss {
                return Err("write outcome unknown after acknowledgement timeout".into());
            }
        }
    }
    Ok(())
}

fn foreign(bbmd: SocketAddrV4, ttl: u16, seconds: u32) -> Result<()> {
    let mut listener = ForeignListener::bind(Ipv4Addr::UNSPECIFIED, bbmd, ttl)?;
    println!("BBMD          {bbmd}  ·  {ttl} s TTL");
    let end = Instant::now() + Duration::from_secs(seconds.into());
    while let Some(event) = listener.next_until(end)? {
        match event {
            ForeignEvent::Registration(state) => match state {
                RegistrationEvent::Registered { .. } => println!("Registered"),
                RegistrationEvent::Renewed { .. } => println!("Renewed"),
                RegistrationEvent::Lapsed => println!("Expired"),
                RegistrationEvent::Unanswered => println!("Registration unanswered"),
                RegistrationEvent::Refused { code } => {
                    println!("Registration refused  {:#06x}", code.to_raw())
                }
                _ => {}
            },
            ForeignEvent::Announcement(a) => {
                print!(
                    "Device {}  ·  origin {}",
                    a.device.object_identifier.instance_number(),
                    a.origin
                );
                if let Some(station) = a.station {
                    print!("  ·  station {station}");
                }
                println!();
            }
        }
    }
    if !listener.registered() {
        return Err("observation ended without a live BBMD registration".into());
    }
    Ok(())
}

fn watch(
    client: Client,
    device: u32,
    input: u32,
    options: WatchOptions,
    seconds: u32,
) -> Result<()> {
    let mut watch = client.watch(device, input, options)?;
    let end = Instant::now() + Duration::from_secs(seconds.into());
    let show = |cause: &str, s: &Sample, gap: bool| {
        println!(
            "{cause:<13} {} {}  {}{}",
            s.value,
            unit_text(s.unit),
            status_text(s.flags),
            if gap { "  · history gap" } else { "" }
        );
    };
    show("Baseline", watch.latest(), false);
    let mut last_read_failed = false;
    let result = (|| -> Result<()> {
        while let Some(event) = watch.next_until(end)? {
            match event {
                WatchEvent::Subscribed => {
                    println!("Subscribed    {} s lease", options.lease_seconds)
                }
                WatchEvent::Observation {
                    trigger,
                    sample,
                    gap,
                } => {
                    let cause = match trigger {
                        WatchTrigger::Verification => "Verification",
                        WatchTrigger::Notification => "Notification",
                    };
                    show(cause, &sample, gap);
                    last_read_failed = false;
                }
                WatchEvent::SubscriptionFailed(error) => {
                    eprintln!("COV unavailable · {error} · verification continues")
                }
                WatchEvent::VerificationFailed(error) => {
                    last_read_failed = true;
                    eprintln!(
                        "Readback failed · {error} · last good observation is {:.1} s old",
                        watch.latest().observed_at.elapsed().as_secs_f64()
                    );
                }
            }
        }
        if last_read_failed || watch.latest().is_stale(Instant::now(), options.max_age) {
            return Err("watch ended without a fresh verified reading".into());
        }
        Ok(())
    })();
    let cancelled = watch.cancel();
    result?;
    cancelled?;
    Ok(())
}

fn batch_value(reading: &Reading, p: Prop) -> String {
    use bacnet_encoding::primitives::ApplicationValueRef as V;
    let raw = || format!("tagged bytes {:02x?}", reading.value);
    match reading.scalar() {
        Ok(V::Null) => "NULL".into(),
        Ok(V::Real(v)) => v.to_string(),
        Ok(V::Double(v)) => v.to_string(),
        Ok(V::Unsigned(v)) => v.to_string(),
        Ok(V::Signed(v)) => v.to_string(),
        Ok(V::Boolean(v)) => v.to_string(),
        Ok(V::BitString {
            unused_bits: 4,
            data: [bits],
        }) if p == Prop::STATUS_FLAGS => {
            status_text(bacnet_kit::client::StatusFlags::from_bits(*bits))
        }
        Ok(V::Enumerated(v)) if p == Prop::UNITS => {
            bacnet_types::enums::EngineeringUnits::from_raw(v).to_string()
        }
        Ok(V::Enumerated(v)) => v.to_string(),
        Ok(V::CharacterString(s)) => s.as_str().map(|v| format!("{v:?}")).unwrap_or_else(raw),
        Ok(V::ObjectIdentifier(v)) => format!("{}:{}", v.object_type(), v.instance_number()),
        _ => raw(),
    }
}

fn batch(mut client: Client, references: &[PropertyRef], device: Option<u32>) -> Result<usize> {
    let refs: Vec<_> = references.iter().map(|r| r.request.clone()).collect();
    if let Some(instance) = device {
        client.negotiate(instance)?;
    }
    let rows = client.read_multiple(&refs)?;
    let mut failed = 0;
    for (reference, row) in references.iter().zip(rows) {
        let name = &reference.label;
        match row.outcome {
            Ok(reading) => println!(
                "{name}  {}",
                batch_value(&reading, row.reference.property_identifier)
            ),
            Err(error) => {
                failed += 1;
                let reason = u16::try_from(error.code)
                    .ok()
                    .and_then(|v| bacnet_types::enums::ErrorCode::from_raw(v).name())
                    .unwrap_or("PROPERTY_ERROR");
                println!(
                    "{name}  {reason} (class {}, code {})",
                    error.class, error.code
                );
            }
        }
    }
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_strings_escape_terminal_controls() {
        // 003-batch-reads/SR-003
        let object = ObjectIdentifier::new(ObjectType::DEVICE, 123).unwrap();
        let value = vec![0x75, 5, 0, b'a', 27, b'\n', b'b'];
        let shown = batch_value(&Reading { object, value }, Prop::OBJECT_NAME);
        assert_eq!(shown, r#""a\u{1b}\nb""#);
    }
    #[test]
    fn default_fixture_binds_loopback() {
        // 001-commissioning/SR-004
        assert_eq!(
            DEFAULT_BIND,
            "127.0.0.1:47808".parse::<SocketAddrV4>().unwrap()
        );
        let socket = UdpSocket::bind((*DEFAULT_BIND.ip(), 0)).unwrap();
        assert!(socket.local_addr().unwrap().ip().is_loopback());
    }
}
