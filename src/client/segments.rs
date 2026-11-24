//! Stop-and-wait segmented transactions for the blocking read client.
use super::link::{DirectedSocket, MAX_DATAGRAM};
use super::{Error, PeerLimits, Service, remaining, reply, wire};
use bacnet_encoding::{
    apdu::{
        AbortPdu, Apdu, ConfirmedRequest, SegmentAck, decode_apdu, encode_apdu, header::MaxSegments,
    },
    segmentation::{SegmentedPduType, max_segment_payload},
};
use bacnet_types::enums::AbortReason;
use bytes::{Bytes, BytesMut};
use std::time::{Duration, Instant};

/// Maximum number of request or reply segments in one host read.
pub const MAX_SEGMENTS: u8 = 16;
const MAX_APDU: u16 = 1476;
const SEGMENT_TIMEOUT: Duration = Duration::from_millis(500);
const RETRIES: u8 = 2;

fn datagram(apdu: Apdu, expects_reply: bool) -> Result<Bytes, Error> {
    let mut frame = BytesMut::from(&[0x81, 10, 0, 0, 1, if expects_reply { 4 } else { 0 }][..]);
    encode_apdu(&mut frame, &apdu).map_err(|_| Error::Invalid("invalid read APDU"))?;
    let len = frame.len();
    if len > wire::MAX_FRAME {
        return Err(Error::Invalid("read APDU exceeds local capacity"));
    }
    frame[2..4].copy_from_slice(&(len as u16).to_be_bytes());
    Ok(frame.freeze())
}

struct Outgoing {
    frames: Vec<Bytes>,
    index: usize,
    retries: u8,
    ack_due: Option<Instant>,
}
impl Outgoing {
    fn start(
        socket: &DirectedSocket,
        deadline: Instant,
        request: &mut [u8],
        limits: PeerLimits,
        invoke: u8,
        service: Service,
    ) -> Result<Self, Error> {
        let mut tx = Self {
            frames: Vec::new(),
            index: 0,
            retries: 0,
            ack_due: None,
        };
        // The read encoders supply a local-link prefix and an unsegmented ConfirmedRequest.
        // Use that very frame for short reads, without allocating per-segment storage.
        if request.len() - 6 <= usize::from(limits.max_apdu) {
            request[6] |= 0x02;
            let max = MaxSegments::from_capacity(Some(MAX_SEGMENTS)).expect("valid receive limit");
            request[7] = (request[7] & 0x0f) | (max.bits() << 4);
            socket.send(deadline, request)?;
            return Ok(tx);
        }
        let body = &request[10..];
        let chunks = max_segment_payload(limits.max_apdu, SegmentedPduType::ConfirmedRequest);
        if body.len().div_ceil(chunks) > usize::from(limits.max_request_segments) {
            return Err(Error::Invalid(
                "request exceeds peer receive capacity; use fewer property references",
            ));
        }
        let mut parts = body.chunks(chunks).peekable();
        while let Some(part) = parts.next() {
            tx.frames.push(datagram(
                Apdu::ConfirmedRequest(ConfirmedRequest {
                    invoke_id: invoke,
                    service_choice: service,
                    segmented: true,
                    more_follows: parts.peek().is_some(),
                    segmented_response_accepted: true,
                    max_segments: Some(MAX_SEGMENTS),
                    max_apdu_length: MAX_APDU,
                    sequence_number: Some(tx.frames.len() as u8),
                    proposed_window_size: Some(1),
                    service_request: Bytes::copy_from_slice(part),
                }),
                true,
            )?);
        }
        tx.send_current(socket, deadline)?;
        Ok(tx)
    }
    fn send_current(&mut self, socket: &DirectedSocket, deadline: Instant) -> Result<(), Error> {
        socket.send(deadline, &self.frames[self.index])?;
        self.ack_due = (self.frames.len() > 1).then(|| Instant::now() + SEGMENT_TIMEOUT);
        Ok(())
    }
    fn retry(
        &mut self,
        socket: &DirectedSocket,
        deadline: Instant,
        invoke: u8,
    ) -> Result<(), Error> {
        if self.retries >= RETRIES {
            abort(socket, deadline, invoke, AbortReason::TSM_TIMEOUT);
            return Err(Error::Timeout);
        }
        self.retries += 1;
        self.send_current(socket, deadline)
    }
    fn acknowledge(
        &mut self,
        ack: SegmentAck,
        socket: &DirectedSocket,
        deadline: Instant,
    ) -> Result<(), Error> {
        if self.ack_due.is_none() || !ack.sent_by_server || ack.actual_window_size != 1 {
            return Ok(());
        }
        let seq = self.index as u8;
        if ack.negative_ack && ack.sequence_number == seq.wrapping_sub(1) {
            return self.retry(socket, deadline, ack.invoke_id);
        }
        if !ack.negative_ack && ack.sequence_number == seq {
            self.ack_due = None;
            if !self.sent_last() {
                self.index += 1;
                self.retries = 0;
                self.send_current(socket, deadline)?;
            }
        }
        Ok(())
    }
    fn sent_last(&self) -> bool {
        self.index + 1 >= self.frames.len()
    }
}

fn feedback(
    socket: &DirectedSocket,
    deadline: Instant,
    invoke: u8,
    seq: u8,
    negative: bool,
) -> Result<(), Error> {
    socket.send(
        deadline,
        &datagram(
            Apdu::SegmentAck(SegmentAck {
                sent_by_server: false,
                invoke_id: invoke,
                sequence_number: seq,
                actual_window_size: 1,
                negative_ack: negative,
            }),
            false,
        )?,
    )
}

fn abort(socket: &DirectedSocket, deadline: Instant, invoke: u8, reason: AbortReason) {
    if let Ok(frame) = datagram(
        Apdu::Abort(AbortPdu {
            sent_by_server: false,
            invoke_id: invoke,
            abort_reason: reason,
        }),
        false,
    ) {
        let _ = socket.send(deadline, &frame);
    }
}

pub(super) fn exchange<T>(
    socket: &DirectedSocket,
    deadline: Instant,
    limits: PeerLimits,
    request: &mut [u8],
    invoke: u8,
    service: Service,
    decode: impl Fn(&[u8]) -> Option<T>,
) -> Result<T, Error> {
    let mut tx = Outgoing::start(socket, deadline, request, limits, invoke, service)?;
    let mut packet = [0; MAX_DATAGRAM + 1];
    let mut body = Vec::new();
    let mut next_seq = 0u8;
    let mut receive_due = None;
    loop {
        let left = remaining(deadline)?;
        let due = tx.ack_due.or(receive_due).unwrap_or(deadline);
        let wait = due.saturating_duration_since(Instant::now()).min(left);
        if wait.is_zero() {
            if tx.ack_due.is_some() {
                tx.retry(socket, deadline, invoke)?;
                continue;
            }
            abort(socket, deadline, invoke, AbortReason::TSM_TIMEOUT);
            return Err(Error::Timeout);
        }
        let Some(apdu) = socket.receive(&mut packet, due.min(deadline))? else {
            continue;
        };
        remaining(deadline)?;
        match apdu {
            [flags, id, _, _] if flags >> 4 == 4 && *id == invoke => {
                if let Ok(Apdu::SegmentAck(ack)) = decode_apdu(Bytes::copy_from_slice(apdu)) {
                    tx.acknowledge(ack, socket, deadline)?;
                }
            }
            [flags, id, _, _, svc, ..]
                if flags & 0xf8 == 0x38
                    && *id == invoke
                    && *svc == service.to_raw()
                    && tx.sent_last() =>
            {
                let Ok(Apdu::ComplexAck(ack)) = decode_apdu(Bytes::copy_from_slice(apdu)) else {
                    continue;
                };
                if !matches!(ack.proposed_window_size, Some(1..=127)) {
                    abort(
                        socket,
                        deadline,
                        invoke,
                        AbortReason::WINDOW_SIZE_OUT_OF_RANGE,
                    );
                    return Err(Error::Invalid(
                        "segmented reply has an invalid proposed window",
                    ));
                }
                let Some(seq) = ack.sequence_number else {
                    continue;
                };
                if seq != next_seq {
                    if next_seq > 0 {
                        let last = next_seq - 1;
                        feedback(socket, deadline, invoke, last, seq != last)?;
                    }
                    continue;
                }
                if next_seq + 1 == MAX_SEGMENTS && ack.more_follows {
                    abort(socket, deadline, invoke, AbortReason::BUFFER_OVERFLOW);
                    return Err(Error::Invalid(
                        "segmented reply exceeds the 16-segment receive limit",
                    ));
                }
                tx.ack_due = None;
                body.extend_from_slice(&ack.service_ack);
                next_seq += 1;
                if !ack.more_follows {
                    let Some(value) = decode(&body) else {
                        abort(
                            socket,
                            deadline,
                            invoke,
                            AbortReason::INVALID_APDU_IN_THIS_STATE,
                        );
                        return Err(Error::Invalid(
                            "segmented reply does not match the requested properties",
                        ));
                    };
                    feedback(socket, deadline, invoke, seq, false)?;
                    return Ok(value);
                }
                feedback(socket, deadline, invoke, seq, false)?;
                receive_due = Some(Instant::now() + 4 * SEGMENT_TIMEOUT);
            }
            _ => {
                // Errors, Rejects and Aborts can terminate transmission early. A successful
                // unsegmented result is eligible only after sending the entire request.
                if let Some(result) = reply(apdu, invoke, service, &decode)
                    && (result.is_err() || (tx.sent_last() && next_seq == 0))
                {
                    return result;
                }
            }
        }
    }
}
