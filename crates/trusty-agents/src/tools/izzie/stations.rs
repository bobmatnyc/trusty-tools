//! Metro-North station + line reference data (epic #3052).
//!
//! Why: The GTFS-Realtime feed identifies stops and routes only by their
//! numeric GTFS ids; a human asks for "Stamford" or "the New Haven line". This
//! module is the name<->id bridge, seeded from the MTA's published Metro-North
//! GTFS static `stops.txt` / `routes.txt` (downloaded from
//! `web.mta.info/developers/data/mnr/google_transit.zip`). The stop_ids here
//! are the real values that appear in the live feed, so schedule matching
//! works out of the box.
//! What: `STATIONS` (curated key stations with aliases), `match_station` for
//! fuzzy name lookup, `ROUTES` mapping the six GTFS route ids to line names,
//! and `route_matches_line` for the optional `line` filter. Data-only; no I/O.
//! Test: `match_station_exact_and_fuzzy`, `route_matches_line_by_name_or_id`.

/// A Metro-North station: its canonical display name, GTFS `stop_id`, and the
/// alias substrings a user might type.
#[derive(Debug, Clone, Copy)]
pub struct Station {
    pub name: &'static str,
    /// GTFS `stop_id` as it appears in the real-time feed (string form).
    pub stop_id: &'static str,
    pub aliases: &'static [&'static str],
}

/// Curated key stations (the set the `izzie-metro-north` skill advertises)
/// with their real MNR GTFS stop_ids.
pub const STATIONS: &[Station] = &[
    Station {
        name: "Grand Central Terminal",
        stop_id: "1",
        aliases: &["grand central", "gct", "grand central terminal"],
    },
    Station {
        name: "Harlem-125th Street",
        stop_id: "4",
        aliases: &["harlem", "125th", "harlem-125", "harlem 125"],
    },
    Station {
        name: "Fordham",
        stop_id: "56",
        aliases: &["fordham"],
    },
    Station {
        name: "Yonkers",
        stop_id: "18",
        aliases: &["yonkers"],
    },
    Station {
        name: "Greystone",
        stop_id: "20",
        aliases: &["greystone"],
    },
    Station {
        name: "Dobbs Ferry",
        stop_id: "23",
        aliases: &["dobbs ferry", "dobbs"],
    },
    Station {
        name: "Irvington",
        stop_id: "25",
        aliases: &["irvington"],
    },
    Station {
        name: "Tarrytown",
        stop_id: "27",
        aliases: &["tarrytown"],
    },
    Station {
        name: "Ossining",
        stop_id: "31",
        aliases: &["ossining"],
    },
    Station {
        name: "Croton-Harmon",
        stop_id: "33",
        aliases: &["croton-harmon", "croton harmon", "croton"],
    },
    Station {
        name: "Peekskill",
        stop_id: "39",
        aliases: &["peekskill"],
    },
    Station {
        name: "Beacon",
        stop_id: "46",
        aliases: &["beacon"],
    },
    Station {
        name: "Poughkeepsie",
        stop_id: "51",
        aliases: &["poughkeepsie"],
    },
    Station {
        name: "Scarsdale",
        stop_id: "71",
        aliases: &["scarsdale"],
    },
    Station {
        name: "White Plains",
        stop_id: "74",
        aliases: &["white plains"],
    },
    Station {
        name: "North White Plains",
        stop_id: "76",
        aliases: &["north white plains"],
    },
    Station {
        name: "Mount Kisco",
        stop_id: "84",
        aliases: &["mount kisco", "mt kisco", "kisco"],
    },
    Station {
        name: "Bedford Hills",
        stop_id: "85",
        aliases: &["bedford hills"],
    },
    Station {
        name: "Katonah",
        stop_id: "86",
        aliases: &["katonah"],
    },
    Station {
        name: "Port Chester",
        stop_id: "115",
        aliases: &["port chester"],
    },
    Station {
        name: "Greenwich",
        stop_id: "116",
        aliases: &["greenwich"],
    },
    Station {
        name: "Old Greenwich",
        stop_id: "121",
        aliases: &["old greenwich"],
    },
    Station {
        name: "Stamford",
        stop_id: "124",
        aliases: &["stamford"],
    },
    Station {
        name: "New Haven",
        stop_id: "149",
        aliases: &["new haven"],
    },
    Station {
        name: "Wassaic",
        stop_id: "177",
        aliases: &["wassaic"],
    },
];

/// Resolve a user-typed station name to a known station.
///
/// Why: Users type partial, lowercase, or aliased names ("gct", "mt kisco").
/// What: Normalizes to lowercase and matches, in priority order: exact
/// canonical name, exact alias, canonical-name substring, then alias
/// substring. Returns the first hit or `None`.
/// Test: `match_station_exact_and_fuzzy`.
pub fn match_station(query: &str) -> Option<&'static Station> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    // 1. exact canonical name.
    if let Some(s) = STATIONS.iter().find(|s| s.name.to_lowercase() == q) {
        return Some(s);
    }
    // 2. exact alias.
    if let Some(s) = STATIONS.iter().find(|s| s.aliases.iter().any(|a| *a == q)) {
        return Some(s);
    }
    // 3. canonical-name substring.
    if let Some(s) = STATIONS.iter().find(|s| s.name.to_lowercase().contains(&q)) {
        return Some(s);
    }
    // 4. alias substring (either direction).
    STATIONS
        .iter()
        .find(|s| s.aliases.iter().any(|a| a.contains(&q) || q.contains(*a)))
}

/// A Metro-North line: its GTFS `route_id` and human name.
#[derive(Debug, Clone, Copy)]
pub struct Route {
    pub route_id: &'static str,
    pub name: &'static str,
}

/// The six MNR GTFS routes (from `routes.txt`).
pub const ROUTES: &[Route] = &[
    Route {
        route_id: "1",
        name: "Hudson",
    },
    Route {
        route_id: "2",
        name: "Harlem",
    },
    Route {
        route_id: "3",
        name: "New Haven",
    },
    Route {
        route_id: "4",
        name: "New Canaan",
    },
    Route {
        route_id: "5",
        name: "Danbury",
    },
    Route {
        route_id: "6",
        name: "Waterbury",
    },
];

/// Human line name for a GTFS `route_id`, or the raw id if unknown.
pub fn route_name(route_id: &str) -> String {
    ROUTES
        .iter()
        .find(|r| r.route_id == route_id)
        .map(|r| r.name.to_string())
        .unwrap_or_else(|| route_id.to_string())
}

/// Whether a feed `route_id` matches a user-supplied `line` filter.
///
/// Why: The `line` argument accepts either a line name ("New Haven", "hudson")
/// or a raw route id; both must resolve.
/// What: True when the filter equals the route id, equals the mapped line name
/// (case-insensitive), or is a substring of that name.
/// Test: `route_matches_line_by_name_or_id`.
pub fn route_matches_line(route_id: &str, line: &str) -> bool {
    let want = line.trim().to_lowercase();
    if want.is_empty() {
        return true;
    }
    if route_id == want {
        return true;
    }
    let name = route_name(route_id).to_lowercase();
    name == want || name.contains(&want) || want.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_station_exact_and_fuzzy() {
        assert_eq!(match_station("Stamford").unwrap().stop_id, "124");
        assert_eq!(match_station("gct").unwrap().stop_id, "1");
        assert_eq!(match_station("GRAND CENTRAL").unwrap().stop_id, "1");
        assert_eq!(match_station("mt kisco").unwrap().stop_id, "84");
        // substring on canonical name
        assert_eq!(match_station("haven").unwrap().stop_id, "149");
        assert!(match_station("Nonexistent Town").is_none());
        assert!(match_station("   ").is_none());
    }

    #[test]
    fn route_matches_line_by_name_or_id() {
        assert!(route_matches_line("3", "New Haven"));
        assert!(route_matches_line("3", "new haven"));
        assert!(route_matches_line("1", "hudson"));
        assert!(route_matches_line("1", "1"));
        assert!(!route_matches_line("1", "Harlem"));
        // empty filter matches anything
        assert!(route_matches_line("2", ""));
    }

    #[test]
    fn route_name_falls_back_to_id() {
        assert_eq!(route_name("3"), "New Haven");
        assert_eq!(route_name("99"), "99");
    }
}
