//! Finite object subscriptions with independent verification reads.
use super::link::{DirectedSocket, MAX_DATAGRAM};
use super::{
    Client, Error, ObjectIdentifier, ObjectType, Prop, Sample, Service, Value, remaining, reply,
    wire,
};
use bacnet_encoding::{
    apdu::header::{ApduHeader, MaxApdu},
    primitives::decode_application_value_ref,
};
use bacnet_services::cov::{COVNotificationRequest, SubscribeCOVRequest};
use bacnet_types::enums::{AbortReason, ErrorClass, ErrorCode, UnconfirmedServiceChoice};
use bytes::BytesMut;
use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

static PROCESS: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug)]
pub struct WatchOptions {
    /// Request confirmed notifications. The subscriber acknowledges these on its lease socket.
    pub confirmed: bool,
    /// Positive lease lifetime in seconds, renewed halfway through its term.
    pub lease_seconds: u32,
    /// Maximum age of a good observation while the peer responds and the watch is driven.
    pub max_age: Duration,
}
impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            confirmed: false,
            lease_seconds: 60,
            max_age: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchTrigger {
    Notification,
    Verification,
}

#[derive(Debug)]
pub enum WatchEvent {
    /// A matching SubscribeCOV acknowledgement. Renewals produce the same event.
    Subscribed,
    /// A directed readback, including the quality flags. A gap marks incomplete change history.
    Observation {
        trigger: WatchTrigger,
        sample: Sample,
        gap: bool,
    },
    /// Verification continues on its own schedule after a lease failure.
    SubscriptionFailed(Error),
    /// The last successful sample retains its original observation time.
    VerificationFailed(Error),
}

struct Pending {
    invoke: u8,
    sent: Instant,
}

/// One driven Analog Input subscription. Call next_until regularly to service it.
pub struct Watch {
    reader: Client,
    socket: DirectedSocket,
    device: ObjectIdentifier,
    input: ObjectIdentifier,
    process: u32,
    next: u16,
    options: WatchOptions,
    budget: Duration,
    latest: Sample,
    verify_at: Instant,
    renew_at: Instant,
    lease_until: Option<Instant>,
    pending: Option<Pending>,
    needs_cancel: bool,
    dirty: bool,
    gap: bool,
    next_hint: Instant,
}

impl Client {
    /// Read a baseline and open a finite watch for a commissioned Device and Analog Input.
    /// The lease uses a dedicated source port on the same local interface as this client.
    /// Drive the first subscription and subsequent renewals with [Watch::next_until].
    ///
    /// ```no_run
    /// use bacnet_kit::client::{Client, WatchEvent, WatchOptions};
    /// use std::time::{Duration, Instant};
    ///
    /// let reader = Client::connect("127.0.0.1:47808".parse()?, Duration::from_secs(2))?;
    /// let mut watch = reader.watch(123, 1, WatchOptions::default())?;
    /// println!("Baseline: {}", watch.latest().value);
    /// let end = Instant::now() + Duration::from_secs(15);
    /// while let Some(event) = watch.next_until(end)? {
    ///     match event {
    ///         WatchEvent::Observation { trigger, sample, gap } =>
    ///             println!("{trigger:?}: {} (history gap: {gap})", sample.value),
    ///         event => println!("{event:?}"),
    ///     }
    /// }
    /// watch.cancel()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn watch(mut self, device: u32, input: u32, options: WatchOptions) -> Result<Watch, Error> {
        if options.lease_seconds == 0
            || options.max_age < Duration::from_millis(100)
            || options.max_age > Duration::from_secs(u32::MAX.into())
        {
            return Err(Error::Invalid(
                "watch requires a finite positive lease and a max age between 100 ms and 2^32-1 seconds",
            ));
        }
        let device = ObjectIdentifier::new_addressable(ObjectType::DEVICE, device)
            .map_err(|_| Error::Invalid("expected a concrete Device instance"))?;
        let input = ObjectIdentifier::new_addressable(ObjectType::ANALOG_INPUT, input)
            .map_err(|_| Error::Invalid("expected a concrete Analog Input instance"))?;
        let budget = self
            .timeout
            .min(options.max_age / 4)
            .min(Duration::from_secs(options.lease_seconds.into()) / 4);
        let latest = verified(&mut self, device, input, Instant::now() + budget)?;
        let socket = self.socket.reopen()?;
        let now = Instant::now();
        Ok(Watch {
            verify_at: latest.observed_at + (options.max_age - budget),
            socket,
            reader: self,
            device,
            input,
            options,
            budget,
            latest,
            process: PROCESS.fetch_add(1, Ordering::Relaxed),
            next: 0,
            pending: None,
            needs_cancel: false,
            renew_at: now,
            lease_until: None,
            dirty: false,
            gap: false,
            next_hint: now,
        })
    }
}

fn verified(
    reader: &mut Client,
    device: ObjectIdentifier,
    input: ObjectIdentifier,
    deadline: Instant,
) -> Result<Sample, Error> {
    let id = reader.read_until(device, Prop::OBJECT_IDENTIFIER, None, deadline)?;
    if !matches!(id.scalar()?,Value::ObjectIdentifier(value) if value==device) {
        return Err(Error::Invalid("watch Device identity changed"));
    }
    reader.sample_until(input, deadline)
}

impl Watch {
    /// The last fully verified sample, including the initial baseline.
    pub fn latest(&self) -> &Sample {
        &self.latest
    }

    fn lease(&self) -> Duration {
        Duration::from_secs(self.options.lease_seconds.into())
    }

    fn control(&self, invoke: u8, cancel: bool, deadline: Instant) -> Result<(), Error> {
        let req = SubscribeCOVRequest {
            subscriber_process_identifier: self.process,
            monitored_object_identifier: self.input,
            issue_confirmed_notifications: (!cancel).then_some(self.options.confirmed),
            lifetime: (!cancel).then_some(self.options.lease_seconds),
        };
        let mut body = BytesMut::new();
        req.encode(&mut body)
            .map_err(|_| Error::Invalid("invalid SubscribeCOV"))?;
        let mut out = [0; 64];
        let n = wire::confirmed_frame(invoke, Service::SUBSCRIBE_COV, &body, &mut out)
            .map_err(|_| Error::Invalid("cannot encode SubscribeCOV"))?;
        self.send(&out[..n], deadline)
    }

    fn send(&self, data: &[u8], deadline: Instant) -> Result<(), Error> {
        self.socket.send(deadline, data)
    }

    fn start_lease(&mut self, now: Instant, deadline: Instant) -> Result<(), Error> {
        if self.next == 255 {
            // Reserve the final invoke ID for cancellation, then change both parts of
            // the subscriber identity before reusing invoke IDs for control requests.
            let fresh = self.socket.reopen()?;
            let _ = self.control(255, true, deadline);
            self.socket = fresh;
            self.process = PROCESS.fetch_add(1, Ordering::Relaxed);
            self.lease_until = None;
            self.dirty = false;
            self.next = 0;
            self.needs_cancel = false;
        }
        let invoke = self.next as u8;
        self.next += 1;
        self.control(invoke, false, deadline)?;
        self.pending = Some(Pending { invoke, sent: now });
        Ok(())
    }

    fn failed_lease(&mut self, error: Error) -> WatchEvent {
        if self.pending.is_some() && matches!(error, Error::Timeout | Error::Io(_)) {
            self.needs_cancel = true; // The publisher may have accepted the lease.
        }
        self.pending = None;
        self.lease_until = None;
        self.dirty = false;
        self.gap = true;
        self.renew_at = Instant::now() + self.options.max_age.min(self.lease() / 2);
        WatchEvent::SubscriptionFailed(error)
    }

    fn observe(&mut self, trigger: WatchTrigger, deadline: Instant) -> WatchEvent {
        self.dirty = false;
        let result = verified(&mut self.reader, self.device, self.input, deadline);
        let now = Instant::now();
        self.next_hint = now + Duration::from_millis(100);
        match result {
            Ok(sample) => {
                let changed = sample.value.to_bits() != self.latest.value.to_bits()
                    || sample.flags != self.latest.flags
                    || sample.unit != self.latest.unit;
                let gap = self.gap
                    || self
                        .latest
                        .is_stale(sample.observed_at, self.options.max_age)
                    || (trigger == WatchTrigger::Verification && changed);
                self.verify_at = sample.observed_at + (self.options.max_age - self.budget);
                self.latest = sample.clone();
                self.gap = false;
                WatchEvent::Observation {
                    trigger,
                    sample,
                    gap,
                }
            }
            Err(error) => {
                self.gap = true;
                self.verify_at = now + self.options.max_age / 2;
                WatchEvent::VerificationFailed(error)
            }
        }
    }

    /// Drive timers and UDP until one event or the caller's absolute deadline.
    /// Verification continues when the publisher loses its subscription table.
    pub fn next_until(&mut self, end: Instant) -> Result<Option<WatchEvent>, Error> {
        let mut frame = [0; MAX_DATAGRAM + 1];
        loop {
            let now = Instant::now();
            if now >= end {
                return Ok(None);
            }
            let operation = end.min(now + self.budget);
            if self.lease_until.is_some_and(|expires| now >= expires) {
                self.lease_until = None;
                self.dirty = false;
                self.gap = true;
            }
            if now >= self.verify_at {
                return Ok(Some(self.observe(WatchTrigger::Verification, operation)));
            }
            if self
                .pending
                .as_ref()
                .is_some_and(|p| now >= p.sent + self.budget)
            {
                return Ok(Some(self.failed_lease(Error::Timeout)));
            }
            if self.pending.is_none()
                && now >= self.renew_at
                && let Err(error) = self.start_lease(now, operation)
            {
                return Ok(Some(self.failed_lease(error)));
            }
            let ready = self.dirty && self.lease_until.is_some();
            if ready && now >= self.next_hint {
                return Ok(Some(self.observe(WatchTrigger::Notification, operation)));
            }
            let control_due = self
                .pending
                .as_ref()
                .map_or(self.renew_at, |p| p.sent + self.budget);
            let mut wake = end.min(control_due).min(self.verify_at);
            if ready {
                wake = wake.min(self.next_hint);
            }
            let apdu = match self.socket.receive(&mut frame, wake) {
                Ok(Some(n)) => n,
                Ok(None) => continue,
                Err(error) => return Ok(Some(self.failed_lease(error))),
            };
            if let Some(p) = &self.pending
                && Instant::now() < p.sent + self.budget
                && let Some(result) = control_result(apdu, p.invoke)
            {
                if let Err(error) = result {
                    return Ok(Some(self.failed_lease(error)));
                }
                self.lease_until = Some(p.sent + self.lease());
                self.renew_at = p.sent + self.lease() / 2;
                self.pending = None;
                self.needs_cancel = true;
                return Ok(Some(WatchEvent::Subscribed));
            }
            let received_at = Instant::now();
            if received_at >= end {
                return Ok(None);
            }
            self.notification(apdu, end.min(received_at + self.budget))?;
        }
    }

    fn notification(&mut self, apdu: &[u8], deadline: Instant) -> Result<(), Error> {
        if let [flags, limits, invoke, _, _, service, ..] = apdu
            && flags >> 4 == 0
            && flags & 8 != 0
            && MaxApdu::from_bits(*limits).is_some()
            && *service == Service::CONFIRMED_COV_NOTIFICATION.to_raw()
        {
            let mut out = [0; 16];
            let n = wire::frame(
                ApduHeader::Abort {
                    server: true,
                    invoke_id: *invoke,
                    reason: AbortReason::SEGMENTATION_NOT_SUPPORTED,
                },
                &[],
                &mut out,
            )
            .map_err(|_| Error::Invalid("cannot encode notification abort"))?;
            return self.send(&out[..n], deadline);
        }
        let Ok((header, body)) = ApduHeader::decode(apdu) else {
            return Ok(());
        };
        let confirmed = match header {
            ApduHeader::ConfirmedRequest(h)
                if h.service_choice == Service::CONFIRMED_COV_NOTIFICATION =>
            {
                Some(h)
            }
            ApduHeader::UnconfirmedRequest(
                UnconfirmedServiceChoice::UNCONFIRMED_COV_NOTIFICATION,
            ) => None,
            _ => return Ok(()),
        };
        let mut out = [0; 32];
        let note = match COVNotificationRequest::decode_detailed(body) {
            Ok(note) => note,
            Err(error) => {
                if let Some(h) = confirmed {
                    let n = wire::frame(
                        ApduHeader::Reject {
                            invoke_id: h.invoke_id,
                            reason: error.reject_reason(),
                        },
                        &[],
                        &mut out,
                    )
                    .map_err(|_| Error::Invalid("cannot encode notification reject"))?;
                    self.send(&out[..n], deadline)?;
                }
                return Ok(());
            }
        };
        let live = self.pending.is_some() || self.lease_until.is_some_and(|t| Instant::now() < t);
        let matches = live
            && self.options.confirmed == confirmed.is_some()
            && note.subscriber_process_identifier == self.process
            && note.initiating_device_identifier == self.device
            && note.monitored_object_identifier == self.input;
        let valid = matches && point_values(&note);
        if let Some(h) = confirmed {
            if valid {
                self.send(
                    &wire::simple_ack(h.invoke_id, Service::CONFIRMED_COV_NOTIFICATION),
                    deadline,
                )?;
            } else {
                let (class, code) = if matches {
                    (ErrorClass::PROPERTY, ErrorCode::INVALID_DATA_TYPE)
                } else {
                    (ErrorClass::SERVICES, ErrorCode::UNKNOWN_SUBSCRIPTION)
                };
                let n = wire::service_error(h, class, code, &mut out)
                    .map_err(|_| Error::Invalid("cannot encode notification error"))?;
                self.send(&out[..n], deadline)?;
            }
        }
        self.dirty |= valid;
        Ok(())
    }

    /// Explicitly cancel and await the publisher's acknowledgement. Dropping a watch
    /// closes its sockets. Its finite subscription at the publisher then expires.
    pub fn cancel(mut self) -> Result<(), Error> {
        if self.pending.is_none() && !self.needs_cancel {
            return Ok(());
        }
        let invoke = self.next as u8;
        let deadline = Instant::now() + self.budget;
        self.control(invoke, true, deadline)?;
        let mut buf = [0; MAX_DATAGRAM + 1];
        loop {
            remaining(deadline)?;
            let Some(apdu) = self.socket.receive(&mut buf, deadline)? else {
                continue;
            };
            if let Some(result) = control_result(apdu, invoke) {
                return result;
            }
            self.notification(apdu, deadline)?;
        }
    }
}

fn point_values(note: &COVNotificationRequest) -> bool {
    let (mut real, mut flags) = (false, false);
    for p in &note.list_of_values {
        if !matches!(
            p.property_identifier,
            Prop::PRESENT_VALUE | Prop::STATUS_FLAGS
        ) {
            continue;
        }
        if p.property_array_index.is_some() || p.priority.is_some() {
            return false;
        }
        let Ok((v, n)) = decode_application_value_ref(&p.value, 0) else {
            return false;
        };
        if n != p.value.len() {
            return false;
        }
        match (p.property_identifier, v) {
            (Prop::PRESENT_VALUE, Value::Real(_)) if !real => real = true,
            (
                Prop::STATUS_FLAGS,
                Value::BitString {
                    unused_bits: 4,
                    data: [_],
                },
            ) if !flags => flags = true,
            _ => return false,
        }
    }
    real && flags
}
fn control_result(apdu: &[u8], invoke: u8) -> Option<Result<(), Error>> {
    if apdu == [0x20, invoke, Service::SUBSCRIBE_COV.to_raw()] {
        return Some(Ok(()));
    }
    reply(apdu, invoke, Service::SUBSCRIBE_COV, &|_| None)
}
