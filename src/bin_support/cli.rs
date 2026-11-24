use super::{
    Client, DEFAULT_BIND, Ipv4Addr, ObjectIdentifier, RemoteStation, Result, SocketAddrV4, TIMEOUT,
    WatchOptions,
};
use bacnet_kit::ReadPropertyRequest;
use clap::{Args, CommandFactory, Parser, Subcommand, value_parser};
use std::str::FromStr;

#[derive(Parser)]
#[command(
    version,
    about = "Commission BACnet devices and inspect captured MS/TP frames",
    after_help = "TARGET accepts IPv4:PORT or NETWORK:0xHEX@ROUTER:PORT. The simulator defaults to 127.0.0.1:47808. BACnet/IP reads allow three seconds.\nCargo features: sc-pki enables sc-check; sc-node also enables sc-read."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Operation>,
}

impl Cli {
    pub fn parse_command() -> Option<Operation> {
        match Self::try_parse() {
            Ok(cli) => {
                // clap applies num_args per group when flags interrupt a positional list.
                if let Some(Operation::Batch { references, .. }) = &cli.command
                    && references.len() > bacnet_kit::batch::MAX_BATCH
                {
                    Self::command()
                        .find_subcommand_mut("batch")
                        .expect("declared command")
                        .error(
                            clap::error::ErrorKind::TooManyValues,
                            format!(
                                "batch accepts at most {} property references",
                                bacnet_kit::batch::MAX_BATCH
                            ),
                        )
                        .exit();
                }
                cli.command
            }
            Err(error) => {
                // A clap diagnostic can quote an unexpected argument verbatim.
                if error.use_stderr()
                    && let Some(name) = std::env::args_os()
                        .skip(1)
                        .find(|s| s != "help" && s != "--")
                    && let Some(command @ ("sc-read" | "sc-check" | "mstp-frame")) = name.to_str()
                {
                    let mut root = Self::command();
                    if let Some(cmd) = root.find_subcommand_mut(command) {
                        cmd.set_bin_name(format!("bacnet-kit {command}"));
                        eprintln!(
                            "error: {}\n\n{}\n\nFor details, use 'bacnet-kit {command} --help'.",
                            error.kind(),
                            cmd.render_usage()
                        );
                    } else {
                        let feature = if command == "sc-read" {
                            "sc-node"
                        } else {
                            "sc-pki"
                        };
                        eprintln!(
                            "error: enable {command} with 'cargo build --features {feature}'."
                        );
                    }
                    std::process::exit(error.exit_code());
                }
                error.exit()
            }
        }
    }

    pub fn help() -> Result<()> {
        Self::command().print_help()?;
        Ok(())
    }
}

#[derive(Subcommand)]
pub enum Operation {
    /// Read a simulated temperature on loopback
    Demo,
    /// Run a mixed-result property batch on loopback
    BatchDemo,
    /// Exercise setpoints, relinquishment and readback on a local fixture
    CommandDemo {
        /// Suppress one acknowledgement and verify the effective value
        #[arg(long)]
        lose_ack: bool,
    },
    /// Capture the fixture's capability worksheet as Markdown
    Pics {
        /// Include the loopback-only commandable Analog Value
        #[arg(long)]
        commands: bool,
    },
    /// Serve the reference device over BACnet/IP
    Serve {
        /// Local IPv4:PORT to bind
        #[arg(default_value_t = DEFAULT_BIND)]
        bind: SocketAddrV4,
        /// Set the Analog Input fault flag
        #[arg(long)]
        fault: bool,
        /// Enable supervised commands (loopback bind only)
        #[arg(long)]
        commands: bool,
    },
    /// Verify device identity and list objects
    Inspect {
        /// IPv4:PORT or NETWORK:0xHEX@ROUTER:PORT
        target: Target,
        /// Verify a known Device instance
        #[arg(long, value_parser = instance())]
        device: Option<u32>,
    },
    /// Read an Analog Input's value, unit and status
    Read {
        /// IPv4:PORT or NETWORK:0xHEX@ROUTER:PORT
        target: Target,
        /// Analog Input instance
        #[arg(value_parser = instance())]
        instance: u32,
    },
    /// Read an ordered batch of up to 32 properties
    Batch {
        /// IPv4:PORT or NETWORK:0xHEX@ROUTER:PORT
        target: Target,
        /// TYPE:INSTANCE/PROPERTY[/INDEX]
        #[arg(required = true, num_args = 1..=bacnet_kit::batch::MAX_BATCH, value_name = "REF")]
        references: Vec<PropertyRef>,
        /// Learn this Device's APDU and segmentation limits before reading
        #[arg(long, value_parser = instance())]
        device: Option<u32>,
    },
    /// Watch an Analog Input with periodic verified readback
    Watch {
        /// IPv4:PORT or NETWORK:0xHEX@ROUTER:PORT
        target: Target,
        /// Device instance
        #[arg(value_parser = instance())]
        device: u32,
        /// Analog Input instance
        #[arg(value_parser = instance())]
        input: u32,
        /// Request confirmed notifications
        #[arg(long)]
        confirmed: bool,
        /// Observation window in seconds
        #[arg(long, default_value_t = 15, value_parser = value_parser!(u32).range(1..))]
        seconds: u32,
        /// Maximum age of a verified reading in seconds
        #[arg(long, default_value_t = WatchOptions::default().max_age.as_secs() as u32, value_parser = value_parser!(u32).range(1..))]
        max_age: u32,
        /// Subscription lifetime in seconds
        #[arg(long, default_value_t = WatchOptions::default().lease_seconds, value_parser = value_parser!(u32).range(1..))]
        lease: u32,
    },
    /// Register with a BBMD and listen for forwarded I-Am announcements
    Foreign {
        /// BBMD IPv4:PORT
        bbmd: SocketAddrV4,
        /// Registration lifetime in seconds
        #[arg(long, default_value_t = 60, value_parser = value_parser!(u16).range(1..))]
        ttl: u16,
        /// Observation window in seconds
        #[arg(long, default_value_t = 15, value_parser = value_parser!(u32).range(1..))]
        seconds: u32,
    },
    /// Inspect a single MS/TP capture offline
    MstpFrame(FrameInput),
    /// Check staged Secure Connect credentials offline
    #[cfg(feature = "sc-pki")]
    ScCheck {
        #[command(flatten)]
        files: CredentialFiles,
        /// Minimum remaining lifetime in whole days
        #[arg(long, default_value_t = 0)]
        min_days: u32,
    },
    /// Read one property through a Secure Connect hub
    #[cfg(feature = "sc-node")]
    ScRead(ScRead),
}

fn instance() -> clap::builder::RangedI64ValueParser<u32> {
    value_parser!(u32).range(0..i64::from(ObjectIdentifier::WILDCARD_INSTANCE))
}

#[derive(Args)]
#[group(required = true, multiple = false)]
pub struct FrameInput {
    /// Captured hex octets (quote spaced hex)
    pub hex: Option<String>,
    /// Read one binary record from standard input
    #[arg(long)]
    pub raw: bool,
}

#[cfg(feature = "sc-pki")]
#[derive(Args)]
pub struct CredentialFiles {
    /// Operational node certificate (PEM)
    #[arg(long, value_name = "FILE")]
    pub cert: std::path::PathBuf,
    /// Node private key (PEM)
    #[arg(long, value_name = "FILE")]
    pub key: std::path::PathBuf,
    /// Trusted issuer certificates (PEM)
    #[arg(long, value_name = "FILE")]
    pub issuers: std::path::PathBuf,
    /// Node issuer's revocation list (PEM)
    #[arg(long, value_name = "FILE")]
    pub crl: Option<std::path::PathBuf>,
}

#[cfg(feature = "sc-node")]
#[derive(Args)]
pub struct ScRead {
    /// Primary hub's wss:// URL
    #[arg(value_name = "PRIMARY-WSS")]
    pub primary: String,
    /// Remote station VMAC as twelve hex digits
    #[arg(value_parser = vmac, value_name = "TARGET-VMAC")]
    pub target: [u8; 6],
    /// TYPE:INSTANCE/PROPERTY[/INDEX]
    pub reference: PropertyRef,
    /// Persistent node identity
    #[arg(long)]
    pub uuid: bacnet_kit::sc_node::Uuid,
    /// Backup hub's wss:// URL
    #[arg(long, value_name = "BACKUP-WSS")]
    pub failover: Option<String>,
    #[command(flatten)]
    pub files: CredentialFiles,
    /// Hub issuer's revocation list (PEM)
    #[arg(long, value_name = "FILE")]
    pub peer_crl: Option<std::path::PathBuf>,
}

#[cfg(feature = "sc-node")]
fn vmac(text: &str) -> std::result::Result<[u8; 6], &'static str> {
    if text.len() != 12 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("expected twelve hex digits");
    }
    Ok(std::array::from_fn(|i| {
        u8::from_str_radix(&text[2 * i..2 * i + 2], 16).expect("validated hex")
    }))
}

#[derive(Clone)]
pub struct PropertyRef {
    pub label: String,
    pub request: ReadPropertyRequest,
}

impl FromStr for PropertyRef {
    type Err = &'static str;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self {
            label: s.into(),
            request: property_reference(s).ok_or("expected TYPE:INSTANCE/PROPERTY[/INDEX]")?,
        })
    }
}

fn property_reference(text: &str) -> Option<ReadPropertyRequest> {
    let (kind, rest) = text.split_once(':')?;
    let kind = kind
        .parse()
        .ok()
        .or_else(|| kind.parse().ok().map(super::ObjectType::from_raw))?;
    let mut parts = rest.split('/');
    let instance = parts.next()?.parse().ok()?;
    let property = parts.next()?;
    let p = property
        .parse()
        .ok()
        .or_else(|| property.parse().ok().map(super::Prop::from_raw))?;
    let index = parts.next().map(str::parse).transpose().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(ReadPropertyRequest {
        object_identifier: ObjectIdentifier::new_addressable(kind, instance).ok()?,
        property_identifier: p,
        property_array_index: index,
    })
}

#[derive(Clone)]
pub struct Target {
    endpoint: SocketAddrV4,
    station: Option<RemoteStation>,
}

impl FromStr for Target {
    type Err = String;
    fn from_str(text: &str) -> std::result::Result<Self, String> {
        let (station, endpoint) = match text.split_once('@') {
            Some((station, endpoint)) => (
                Some(
                    station
                        .parse::<RemoteStation>()
                        .map_err(|e| e.to_string())?,
                ),
                endpoint,
            ),
            None => (None, text),
        };
        Ok(Self {
            endpoint: endpoint
                .parse()
                .map_err(|_| "expected an IPv4:port endpoint")?,
            station,
        })
    }
}

impl Target {
    pub fn connect(self) -> Result<Client> {
        Ok(match self.station {
            Some(station) => Client::via(Ipv4Addr::UNSPECIFIED, self.endpoint, station, TIMEOUT)?,
            None => Client::connect(self.endpoint, TIMEOUT)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_options_preserve_reference_order() {
        Cli::command().debug_assert();
        let target = "42:0x07@127.0.0.1:47808";
        let first = "analog-input:1/units";
        let second = "device:123/object-list/0";
        for args in [
            ["--device", "123", target, first, second],
            [target, first, "--device", "123", second],
            [target, first, second, "--device", "123"],
        ] {
            let cli = Cli::try_parse_from(["bacnet-kit", "batch"].into_iter().chain(args)).unwrap();
            let Some(Operation::Batch {
                target,
                references,
                device,
            }) = cli.command
            else {
                panic!("expected a batch")
            };
            assert_eq!(device, Some(123));
            assert_eq!(target.station.unwrap().network(), 42);
            assert_eq!(references.len(), 2);
            assert_eq!(references[0].label, first);
            assert_eq!(references[1].request.property_array_index, Some(0));
        }
    }
}
