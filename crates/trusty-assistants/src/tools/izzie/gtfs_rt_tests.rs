//! Fixture-driven tests for the minimal GTFS-Realtime decoder.
//!
//! Why: The decoder is hand-written against the frozen wire format, so it
//! needs direct proof it frames fields correctly. These tests encode known
//! messages with a tiny local protobuf writer and assert the decoder returns
//! exactly what was written — no live network, no `protoc`.
//! What: A `Pb` builder emits the four wire types we consume, then each test
//! composes a realistic `FeedMessage` and checks the decoded `Feed`.
//! Test: This file IS the test module for `gtfs_rt`.

use super::*;

// ── Tiny protobuf writer (test-only fixture builder) ───────────────────────

/// Minimal protobuf encoder producing exactly the wire shapes the decoder
/// consumes: varint (type 0) and length-delimited (type 2).
#[derive(Default)]
struct Pb {
    buf: Vec<u8>,
}

impl Pb {
    fn varint(mut self, field: u32, value: u64) -> Self {
        self.write_key(field, 0);
        self.write_varint(value);
        self
    }

    fn bytes(mut self, field: u32, value: &[u8]) -> Self {
        self.write_key(field, 2);
        self.write_varint(value.len() as u64);
        self.buf.extend_from_slice(value);
        self
    }

    fn string(self, field: u32, value: &str) -> Self {
        self.bytes(field, value.as_bytes())
    }

    fn message(self, field: u32, inner: Pb) -> Self {
        self.bytes(field, &inner.buf)
    }

    /// Emit a 32-bit fixed field (type 5) — used only to prove the decoder
    /// skips wire types it does not consume without mis-framing.
    fn fixed32(mut self, field: u32, value: u32) -> Self {
        self.write_key(field, 5);
        self.buf.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn write_key(&mut self, field: u32, wire_type: u32) {
        self.write_varint(((field as u64) << 3) | wire_type as u64);
    }

    fn write_varint(&mut self, mut value: u64) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            self.buf.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn done(self) -> Vec<u8> {
        self.buf
    }
}

/// StopTimeEvent { time }.
fn stop_time_event(epoch: i64) -> Pb {
    Pb::default().varint(2, epoch as u64)
}

#[test]
fn read_varint_multibyte() {
    // 300 = 0xAC 0x02 in base-128 varint; wrap it as field 1 varint.
    let bytes = Pb::default().varint(1, 300).done();
    let mut reader = Reader::new(&bytes);
    let (field, value) = reader.next_field().unwrap().unwrap();
    assert_eq!(field, 1);
    match value {
        Wire::Varint(v) => assert_eq!(v, 300),
        _ => panic!("expected varint"),
    }
    assert!(reader.next_field().unwrap().is_none());
}

#[test]
fn decode_feed_reads_trip_updates() {
    // TripDescriptor { trip_id, route_id, start_date, start_time }
    let trip = Pb::default()
        .string(F_TD_TRIP_ID, "8501")
        .string(F_TD_START_TIME, "18:42:00")
        .string(F_TD_START_DATE, "20260722")
        .string(F_TD_ROUTE_ID, "New Haven");
    // StopTimeUpdate at Grand Central (stop_id "1") with a departure + track.
    let stop_props = Pb::default().string(F_STP_ASSIGNED_STOP_ID, "24");
    let stop = Pb::default()
        .varint(F_STU_STOP_SEQUENCE, 1)
        .string(F_STU_STOP_ID, "1")
        .message(F_STU_DEPARTURE, stop_time_event(1_753_224_120))
        .message(F_STU_STOP_TIME_PROPERTIES, stop_props);
    let trip_update = Pb::default()
        .message(F_TU_TRIP, trip)
        .message(F_TU_STOP_TIME_UPDATE, stop);
    let entity = Pb::default().message(F_ENTITY_TRIP_UPDATE, trip_update);
    let feed_bytes = Pb::default().message(F_FEED_ENTITY, entity).done();

    let feed = decode_feed(&feed_bytes).unwrap();
    assert_eq!(feed.trip_updates.len(), 1);
    let tu = &feed.trip_updates[0];
    assert_eq!(tu.trip_id.as_deref(), Some("8501"));
    assert_eq!(tu.route_id.as_deref(), Some("New Haven"));
    assert_eq!(tu.start_date.as_deref(), Some("20260722"));
    assert_eq!(tu.start_time.as_deref(), Some("18:42:00"));
    assert_eq!(tu.stops.len(), 1);
    let s = &tu.stops[0];
    assert_eq!(s.stop_id.as_deref(), Some("1"));
    assert_eq!(s.stop_sequence, Some(1));
    assert_eq!(s.departure, Some(1_753_224_120));
    assert_eq!(s.arrival, None);
    assert_eq!(s.assigned_track.as_deref(), Some("24"));
}

#[test]
fn decode_feed_reads_alerts() {
    let informed = Pb::default().string(F_ES_ROUTE_ID, "Hudson");
    let header = Pb::default().message(
        F_TS_TRANSLATION,
        Pb::default().string(F_TRANSLATION_TEXT, "Hudson Line Delays"),
    );
    let desc = Pb::default().message(
        F_TS_TRANSLATION,
        Pb::default().string(
            F_TRANSLATION_TEXT,
            "Trains delayed up to 20 minutes due to signal problems.",
        ),
    );
    let alert = Pb::default()
        .message(F_ALERT_INFORMED_ENTITY, informed)
        .message(F_ALERT_HEADER_TEXT, header)
        .message(F_ALERT_DESCRIPTION_TEXT, desc);
    let entity = Pb::default().message(F_ENTITY_ALERT, alert);
    let feed_bytes = Pb::default().message(F_FEED_ENTITY, entity).done();

    let feed = decode_feed(&feed_bytes).unwrap();
    assert_eq!(feed.alerts.len(), 1);
    let a = &feed.alerts[0];
    assert_eq!(a.route_ids, vec!["Hudson".to_string()]);
    assert_eq!(a.header.as_deref(), Some("Hudson Line Delays"));
    assert_eq!(
        a.description.as_deref(),
        Some("Trains delayed up to 20 minutes due to signal problems.")
    );
}

#[test]
fn decode_feed_skips_unknown_fields() {
    // An entity carrying an unrelated fixed32 field plus a vehicle-position
    // (field 4) message must be skipped without corrupting the trip update.
    let trip = Pb::default().string(F_TD_TRIP_ID, "9001");
    let trip_update = Pb::default().message(F_TU_TRIP, trip);
    let entity = Pb::default()
        .fixed32(99, 0xDEAD_BEEF)
        .bytes(4, &Pb::default().string(1, "ignored-vehicle").done())
        .message(F_ENTITY_TRIP_UPDATE, trip_update);
    let feed_bytes = Pb::default()
        .fixed32(1, 12345) // FeedHeader-ish noise at feed level
        .message(F_FEED_ENTITY, entity)
        .done();

    let feed = decode_feed(&feed_bytes).unwrap();
    assert_eq!(feed.trip_updates.len(), 1);
    assert_eq!(feed.trip_updates[0].trip_id.as_deref(), Some("9001"));
    assert!(feed.alerts.is_empty());
}

#[test]
fn decode_feed_rejects_truncated_varint() {
    // A key that promises a varint but ends the buffer must error, not panic.
    let bytes = vec![0x08, 0x80]; // field 1, varint, continuation bit set, no more
    assert!(decode_feed(&bytes).is_err());
}
