//! Minimal, dependency-free GTFS-Realtime protobuf decoder (epic #3052).
//!
//! Why: The MTA Metro-North real-time feed is published as GTFS-Realtime
//! protobuf. The only alternatives for reading it are (a) pulling in a full
//! `prost`/`protobuf-codegen` toolchain — which drags a `protoc` build
//! dependency into CI and a large generated binding file that would blow the
//! 500-SLOC file cap — or (b) decoding, by hand, the *tiny stable subset* of
//! the wire format we actually consume (trip updates + service alerts). The
//! GTFS-RT field numbers are frozen by the published spec and never change,
//! so a hand-written reader for that subset is both smaller and more portable
//! than a code-generation pipeline, and it is fully unit-testable against
//! byte fixtures we encode ourselves (no live network, no `protoc`).
//! What: A cursor-based reader over the four protobuf wire types plus typed
//! extraction of exactly the fields the `get_train_schedule` /
//! `get_train_alerts` tools need — `TripUpdate` (trip descriptor + per-stop
//! arrival/departure epochs + assigned track) and `Alert` (informed routes +
//! header/description text). Everything else in the feed is skipped.
//! Test: `decode_feed_reads_trip_updates`, `decode_feed_reads_alerts`,
//! `read_varint_multibyte`, `decode_feed_skips_unknown_fields`.

use anyhow::{Result, bail};

// ── GTFS-Realtime field numbers (frozen by the published .proto spec) ──────
// FeedMessage
const F_FEED_ENTITY: u32 = 2;
// FeedEntity
const F_ENTITY_TRIP_UPDATE: u32 = 3;
const F_ENTITY_ALERT: u32 = 5;
// TripUpdate
const F_TU_TRIP: u32 = 1;
const F_TU_STOP_TIME_UPDATE: u32 = 2;
// TripDescriptor
const F_TD_TRIP_ID: u32 = 1;
const F_TD_START_TIME: u32 = 2;
const F_TD_START_DATE: u32 = 3;
const F_TD_ROUTE_ID: u32 = 5;
// StopTimeUpdate
const F_STU_STOP_SEQUENCE: u32 = 1;
const F_STU_ARRIVAL: u32 = 2;
const F_STU_DEPARTURE: u32 = 3;
const F_STU_STOP_ID: u32 = 4;
const F_STU_STOP_TIME_PROPERTIES: u32 = 6;
// StopTimeEvent
const F_STE_TIME: u32 = 2;
// StopTimeProperties
const F_STP_ASSIGNED_STOP_ID: u32 = 1;
// Alert
const F_ALERT_INFORMED_ENTITY: u32 = 5;
const F_ALERT_HEADER_TEXT: u32 = 10;
const F_ALERT_DESCRIPTION_TEXT: u32 = 11;
// EntitySelector
const F_ES_ROUTE_ID: u32 = 3;
// TranslatedString
const F_TS_TRANSLATION: u32 = 1;
// Translation
const F_TRANSLATION_TEXT: u32 = 1;

/// One protobuf wire value borrowed from the underlying buffer.
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    /// 64-bit and 32-bit fixed fields — unused by our subset. The reader still
    /// consumes their bytes so following fields stay correctly framed; the
    /// value itself is discarded.
    Fixed,
}

/// Cursor over a single protobuf message body.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Read a base-128 varint. Returns `None` at clean end-of-buffer.
    fn read_varint(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            if self.pos >= self.buf.len() {
                bail!("gtfs-rt: truncated varint");
            }
            let byte = self.buf[self.pos];
            self.pos += 1;
            if shift >= 64 {
                bail!("gtfs-rt: varint overflow");
            }
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Yield the next `(field_number, wire_value)` or `None` at end-of-buffer.
    fn next_field(&mut self) -> Result<Option<(u32, Wire<'a>)>> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let key = self.read_varint()?;
        let field = (key >> 3) as u32;
        let wire_type = (key & 0x7) as u32;
        let value = match wire_type {
            0 => Wire::Varint(self.read_varint()?),
            2 => {
                let len = self.read_varint()? as usize;
                let end = self
                    .pos
                    .checked_add(len)
                    .filter(|e| *e <= self.buf.len())
                    .ok_or_else(|| anyhow::anyhow!("gtfs-rt: length-delimited overrun"))?;
                let slice = &self.buf[self.pos..end];
                self.pos = end;
                Wire::Bytes(slice)
            }
            1 => {
                let end = self.pos + 8;
                if end > self.buf.len() {
                    bail!("gtfs-rt: truncated 64-bit field");
                }
                self.pos = end;
                Wire::Fixed
            }
            5 => {
                let end = self.pos + 4;
                if end > self.buf.len() {
                    bail!("gtfs-rt: truncated 32-bit field");
                }
                self.pos = end;
                Wire::Fixed
            }
            other => bail!("gtfs-rt: unsupported wire type {other}"),
        };
        Ok(Some((field, value)))
    }
}

/// A single upcoming stop within a trip: when the train reaches `stop_id` and
/// (when the MTA has assigned it) the track it will be on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StopTimeUpdate {
    pub stop_id: Option<String>,
    pub stop_sequence: Option<u32>,
    /// Arrival time as a Unix epoch (seconds), when present.
    pub arrival: Option<i64>,
    /// Departure time as a Unix epoch (seconds), when present.
    pub departure: Option<i64>,
    /// Assigned track, decoded from `StopTimeProperties.assigned_stop_id`.
    pub assigned_track: Option<String>,
}

/// A real-time update for one scheduled trip.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TripUpdate {
    pub trip_id: Option<String>,
    pub route_id: Option<String>,
    pub start_date: Option<String>,
    pub start_time: Option<String>,
    pub stops: Vec<StopTimeUpdate>,
}

/// A service alert: which routes it affects plus its human-readable text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Alert {
    pub route_ids: Vec<String>,
    pub header: Option<String>,
    pub description: Option<String>,
}

/// The decoded subset of a GTFS-Realtime feed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feed {
    pub trip_updates: Vec<TripUpdate>,
    pub alerts: Vec<Alert>,
}

/// Decode a GTFS-Realtime `FeedMessage` into the trip-update + alert subset.
///
/// Why: The tools only ever read departures and alerts; decoding the whole
/// schema would be wasted work and wasted code.
/// What: Walks each `FeedEntity`, dispatching `trip_update` (field 3) and
/// `alert` (field 5) into their typed decoders and skipping everything else.
/// Test: `decode_feed_reads_trip_updates`, `decode_feed_reads_alerts`.
pub fn decode_feed(bytes: &[u8]) -> Result<Feed> {
    let mut feed = Feed::default();
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        if field == F_FEED_ENTITY
            && let Wire::Bytes(entity) = value
        {
            decode_entity(entity, &mut feed)?;
        }
    }
    Ok(feed)
}

fn decode_entity(bytes: &[u8], feed: &mut Feed) -> Result<()> {
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        match (field, value) {
            (F_ENTITY_TRIP_UPDATE, Wire::Bytes(b)) => {
                feed.trip_updates.push(decode_trip_update(b)?)
            }
            (F_ENTITY_ALERT, Wire::Bytes(b)) => feed.alerts.push(decode_alert(b)?),
            _ => {}
        }
    }
    Ok(())
}

fn decode_trip_update(bytes: &[u8]) -> Result<TripUpdate> {
    let mut tu = TripUpdate::default();
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        match (field, value) {
            (F_TU_TRIP, Wire::Bytes(b)) => decode_trip_descriptor(b, &mut tu)?,
            (F_TU_STOP_TIME_UPDATE, Wire::Bytes(b)) => tu.stops.push(decode_stop_time_update(b)?),
            _ => {}
        }
    }
    Ok(tu)
}

fn decode_trip_descriptor(bytes: &[u8], tu: &mut TripUpdate) -> Result<()> {
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        if let Wire::Bytes(b) = value {
            match field {
                F_TD_TRIP_ID => tu.trip_id = Some(utf8(b)),
                F_TD_START_TIME => tu.start_time = Some(utf8(b)),
                F_TD_START_DATE => tu.start_date = Some(utf8(b)),
                F_TD_ROUTE_ID => tu.route_id = Some(utf8(b)),
                _ => {}
            }
        }
    }
    Ok(())
}

fn decode_stop_time_update(bytes: &[u8]) -> Result<StopTimeUpdate> {
    let mut stu = StopTimeUpdate::default();
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        match (field, value) {
            (F_STU_STOP_SEQUENCE, Wire::Varint(v)) => stu.stop_sequence = Some(v as u32),
            (F_STU_STOP_ID, Wire::Bytes(b)) => stu.stop_id = Some(utf8(b)),
            (F_STU_ARRIVAL, Wire::Bytes(b)) => stu.arrival = decode_stop_time_event(b)?,
            (F_STU_DEPARTURE, Wire::Bytes(b)) => stu.departure = decode_stop_time_event(b)?,
            (F_STU_STOP_TIME_PROPERTIES, Wire::Bytes(b)) => {
                stu.assigned_track = decode_stop_time_properties(b)?;
            }
            _ => {}
        }
    }
    Ok(stu)
}

/// Extract `StopTimeEvent.time` (field 2, int64 varint) — the epoch seconds.
fn decode_stop_time_event(bytes: &[u8]) -> Result<Option<i64>> {
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        if field == F_STE_TIME
            && let Wire::Varint(v) = value
        {
            return Ok(Some(v as i64));
        }
    }
    Ok(None)
}

/// Extract `StopTimeProperties.assigned_stop_id` (field 1) — the track label
/// the MTA assigns ~20-30 min before departure at terminals.
fn decode_stop_time_properties(bytes: &[u8]) -> Result<Option<String>> {
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        if field == F_STP_ASSIGNED_STOP_ID
            && let Wire::Bytes(b) = value
        {
            return Ok(Some(utf8(b)));
        }
    }
    Ok(None)
}

fn decode_alert(bytes: &[u8]) -> Result<Alert> {
    let mut alert = Alert::default();
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        match (field, value) {
            (F_ALERT_INFORMED_ENTITY, Wire::Bytes(b)) => {
                if let Some(route) = decode_entity_selector(b)? {
                    alert.route_ids.push(route);
                }
            }
            (F_ALERT_HEADER_TEXT, Wire::Bytes(b)) => alert.header = decode_translated_string(b)?,
            (F_ALERT_DESCRIPTION_TEXT, Wire::Bytes(b)) => {
                alert.description = decode_translated_string(b)?;
            }
            _ => {}
        }
    }
    Ok(alert)
}

/// Extract `EntitySelector.route_id` (field 3) if present.
fn decode_entity_selector(bytes: &[u8]) -> Result<Option<String>> {
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        if field == F_ES_ROUTE_ID
            && let Wire::Bytes(b) = value
        {
            return Ok(Some(utf8(b)));
        }
    }
    Ok(None)
}

/// Extract the first `Translation.text` from a `TranslatedString`.
fn decode_translated_string(bytes: &[u8]) -> Result<Option<String>> {
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.next_field()? {
        if field == F_TS_TRANSLATION
            && let Wire::Bytes(translation) = value
        {
            let mut inner = Reader::new(translation);
            while let Some((tf, tv)) = inner.next_field()? {
                if tf == F_TRANSLATION_TEXT
                    && let Wire::Bytes(b) = tv
                {
                    return Ok(Some(utf8(b)));
                }
            }
        }
    }
    Ok(None)
}

/// Lossy UTF-8 decode — feed text fields are UTF-8 but we never want a
/// malformed byte to abort a whole feed parse.
fn utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
#[path = "gtfs_rt_tests.rs"]
mod tests;
