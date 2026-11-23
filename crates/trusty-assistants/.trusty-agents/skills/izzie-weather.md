---
name: izzie-weather
description: Weather forecasts and alerts via Open-Meteo and NWS
tags: [weather, forecast, nws, open-meteo, izzie]
agents: [personal-assistant]
---

# Weather — Forecasts and Severe Alerts

Real-time weather data for any location. No API key required.

## Data Sources

- **Open-Meteo** — free forecast API, global coverage, no auth required.
  Provides hourly + daily temperature, precipitation, wind, weather codes.
- **National Weather Service (NWS)** — official US severe weather alerts.
  Active warnings, watches, advisories. US locations only.

## When to use

- "What's the weather today?" / "Will it rain this weekend?"
- "Forecast for the next 3 days" / "Should I bring an umbrella?"
- "Any storm warnings near me?" / "Is there a flood watch?"
- "What's the weather in [city] tomorrow?"

## Default location

When the user asks about weather without specifying a location, default to
**Hastings-on-Hudson, NY** (Masa's home base). For travel queries, use the
location implied by recent calendar events or hotel bookings — call the
calendar/email tools first before asking the user where they are.

## Proactive Alert Conditions

Surface a weather alert proactively (e.g. in a morning briefing) when ANY of:

- Active NWS Severe or Extreme weather alert in the user's area
- Heavy rain forecast: ≥80% probability AND ≥0.5" expected
- Thunderstorm or severe weather code in the next 48 hours
- Extreme heat: ≥95°F daytime high
- Dangerous cold: ≤15°F overnight low
- High winds: ≥35 mph sustained or gusts

## Response style

- Lead with the most actionable fact ("Rain starting around 3pm — bring an umbrella.")
- Temperatures in °F by default
- Wind in mph
- Don't list every hour — pick the inflection points
- For multi-day: morning/afternoon/evening summary per day
- Cite the source briefly when relevant ("per NWS warning")

## Anti-patterns

- ❌ Refusing weather questions because "I don't have a weather tool" — you do
- ❌ Fabricating temperatures or precipitation amounts
- ❌ Reading a 24-hour hourly dump when the user asked "is it going to rain?"
