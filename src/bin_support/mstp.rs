use super::Result;
use bacnet_kit::mstp::{InspectError, MAX_RECORD, inspect};
use std::io::{self, Read, Write};

fn parse_hex(text: &str) -> Result<Vec<u8>> {
    if text.len() > 4 * MAX_RECORD {
        return Err("hex input exceeds the record text budget".into());
    }
    let mut bytes = Vec::new();
    for group in text.split_ascii_whitespace() {
        let (pairs, remainder) = group.as_bytes().as_chunks::<2>();
        if !remainder.is_empty() {
            return Err("expected two hex digits per octet".into());
        }
        for pair in pairs {
            if bytes.len() == MAX_RECORD {
                return Err(InspectError::TooLarge.into());
            }
            let digit = |b: u8| char::from(b).to_digit(16).ok_or("invalid hex digit");
            bytes.push(((digit(pair[0])? << 4) | digit(pair[1])?) as u8);
        }
    }
    Ok(bytes)
}

pub(super) fn run(input: super::cli::FrameInput) -> Result<()> {
    let mut record = if input.raw {
        let mut bytes = Vec::with_capacity(MAX_RECORD + 1);
        io::stdin()
            .lock()
            .take((MAX_RECORD + 1) as u64)
            .read_to_end(&mut bytes)?;
        bytes
    } else {
        parse_hex(
            input
                .hex
                .as_deref()
                .expect("clap requires exactly one frame source"),
        )?
    };
    let frame = inspect(&mut record)?;
    let kind = frame.header.frame_type;
    let mut out = io::stdout().lock();
    writeln!(out, "Frame        {kind} ({})", kind.to_raw())?;
    writeln!(out, "Source       {}", frame.header.source)?;
    if frame.header.destination == 255 {
        writeln!(out, "Destination  255 (broadcast)")?;
    } else {
        writeln!(out, "Destination  {}", frame.header.destination)?;
    }
    writeln!(out, "Data octets  {}", frame.data_octets)?;
    writeln!(out, "Wire octets  {}", frame.wire_octets)?;
    writeln!(
        out,
        "Pad          {}",
        if frame.padded { "FF" } else { "none" }
    )?;
    Ok(())
}
