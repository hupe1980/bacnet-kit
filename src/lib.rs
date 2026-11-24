//! BACnet commissioning tools for workstations and embedded devices.
//!
//! With default features the blocking UDP client reads values, units and status
//! from local or routed BACnet/IP devices. Enable `sc-node` for a property
//! read through a primary or failover Secure Connect hub.
//!
//! The reference [`Device`] and offline [`mstp`] frame inspector also build
//! without `std` or an allocator. The caller supplies the wire buffers.
//!
//! ```
//! use bacnet_kit::Device;
//!
//! let device = Device::new(123, 0, 21.5)?;
//! let who_is = [0x81, 0x0a, 0, 8, 1, 0, 0x10, 8];
//! let mut response = [0; 128];
//! let size = device.respond(&who_is, &mut response)?.unwrap();
//! assert_eq!(&response[6..8], &[0x10, 0]); // I-Am
//! assert!(size > 8);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
pub mod batch;
#[cfg(feature = "std")]
pub mod client;
pub mod commands;
pub mod device;
pub mod mstp;
#[cfg(feature = "sc-pki")]
pub mod sc_pki;
pub mod wire;

pub use bacnet_services::read_property::ReadPropertyRequest;
pub use bacnet_types::enums::{ObjectType, PropertyIdentifier};
pub use bacnet_types::primitives::ObjectIdentifier;
pub use device::Device;

/// Default UDP port for BACnet/IP.
pub const DEFAULT_UDP_PORT: u16 = 47808;

#[cfg(feature = "sc-node")]
pub mod sc_node;
