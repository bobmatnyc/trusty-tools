---
name: izzie-metro-north
description: MTA Metro North real-time schedules and alerts
tags: [transit, metro-north, mta, train, schedule, izzie, cto]
agents: [personal-assistant, cto-assistant]
---

# MTA Metro North — Real-time Schedules and Alerts

Real-time train data from the MTA GTFS-Realtime feed. No API key required.

## When to use

- "When's the next train from Grand Central to Stamford?"
- "Show me 3 trains to Greenwich"
- "Any delays on the New Haven line today?"
- "What trains go from White Plains to Grand Central in the next hour?"
- "Is the Hudson line running on time?"
- "Next train to New Haven?"

## Capabilities

- **Schedule lookup** — upcoming trains between any two Metro North stations.
  Partial station name matching is supported (e.g. "Grand Central" → "Grand Central Terminal").
- **Service alerts** — active delays, suspensions, advisories per line.
- **Track numbers** — included when assigned by MTA (typically 20-30 min before
  departure at Grand Central; encoded via GTFS `StopTimeProperties.assigned_stop_id`
  or `stop_id` suffix).

## Key Stations

Grand Central Terminal, Harlem-125th Street, Stamford, Greenwich, Port Chester,
White Plains, New Haven, Poughkeepsie, Wassaic, Mount Kisco, Scarsdale, Yonkers,
Tarrytown, Ossining, Croton-Harmon, Peekskill, Beacon, Dobbs Ferry, Irvington.

## Lines

New Haven, Harlem, Hudson, Pascack Valley, Port Jervis, New Canaan, Danbury,
Waterbury.

## Time zone

All schedules are real-time and reported in **Eastern time**.

## Response style

- Lead with the next 3 departures unless user asks for more
- Format: `HH:MM → arrives HH:MM (Track N)` — track only when assigned
- Note any delays inline ("6:42 +5 min late")
- Surface active service alerts at the top if they affect the requested line
- Don't redirect to the MTA app — answer the question with the live data

## Anti-patterns

- ❌ Refusing transit questions or pointing at Google Maps / MTA app
- ❌ Fabricating departure times — always use the live tool
- ❌ Quoting AM/PM in 24-hour contexts inconsistently — pick one and stick with it per response
