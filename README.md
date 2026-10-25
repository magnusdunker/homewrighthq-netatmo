# Netatmo for HomewrightHQ

An external integration for [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Netatmo's weather station, thermostat and radiator valves and the indoor and outdoor cameras, through Netatmo's API.

## Installation

In HomewrightHQ: **Settings → Integrations → From GitHub**. Paste

```
https://github.com/magnusdunker/homewrighthq-netatmo
```

and press **Install**.

Netatmo requires you to create your own app:

1. Log in at [dev.netatmo.com](https://dev.netatmo.com) with your Netatmo account, the same one as in the Netatmo app.
2. Create an app under **My apps**, and enter a redirect URI, e.g. your HomewrightHQ address followed by `/auth/external/callback`: `http://localhost:8123/auth/external/callback`.
3. Save the app's client ID and client secret.

Then add **Netatmo** under **Add integration**, and enter the client ID, client secret and the same redirect URI. The next step shows an address for Netatmo's login. When you have logged in and granted access, Netatmo sends the browser to the redirect address. That page may show an error; that does not matter. Copy the whole address from the browser's address bar (it contains `code=...`) and paste it into the form.

The last step is needed because an external integration in HomewrightHQ cannot receive Netatmo's response itself.

The integration asks for access to the weather station, thermostats and cameras at once. If you only have some of them, the rest is just empty.

## What you get

The weather station becomes one device for the base and one for each module, with:

- base: temperature, humidity, CO2, noise and air pressure
- outdoor module: temperature and humidity
- extra indoor module: temperature, humidity and CO2
- rain gauge: rain in the latest measurement, the last hour and today
- wind gauge: wind and gusts in km/h and their direction in degrees
- battery in percent on the modules and signal strength (disabled by default)

If a module loses its connection, its sensors become unavailable.

Each room with a thermostat or radiator valve becomes a device with:

- a climate entity with the modes `auto` (follow the weekly schedule), `heat` (manual temperature) and `off`, and the presets `schedule`, `manual`, `boost` (max), `away` and `frost_guard`. Whether the room is heating (`hvac_action`) comes from the room's heating demand, or from the boiler status if Netatmo does not report it.
- a sensor for an open window

`away` and `frost_guard` apply to the whole home, the way Netatmo does it, and so does going back to `schedule` from them. A manual temperature lasts as long as set in the Netatmo app (usually three hours).

Thermostats and radiator valves get a battery sensor (`full`, `high`, `medium`, `low`, `very_low`) and a low battery sensor. The thermostat gets a sensor for whether the boiler is on.

Cameras (Smart Indoor and Outdoor Camera) get a switch for monitoring, and the outdoor camera a select for the floodlight (`auto`, `on`, `off`). Known persons that the camera has learned to recognize each become a presence sensor on the home.

There is no video, no still images and no events from the cameras. The SDK for external integrations has no camera entity, and Netatmo's events arrive as webhooks, which an external integration cannot receive.

## Updates and Netatmo's limits

The integration reads the weather stations and the status of each home every five minutes, that is one request plus one per home. The weather station itself sends new measurements every ten minutes. Under **Options** on the integration the interval can be set from 2 to 60 minutes. If Netatmo says the request limit has been reached, the integration pauses for 30 minutes.

After a command the integration updates the state itself, and the next read fetches what Netatmo has in fact set.

The access token is renewed every three hours. Netatmo gives a new refresh token on renewal, and the integration saves it on the entry. If Netatmo rejects it, HomewrightHQ shows it under issues, and the integration can be authorized again.

Rooms, modules and persons are read when the integration starts. A new module or room is included when the integration is reloaded.

## Not tested against the real API

The integration is written from Netatmo's API documentation and the way Home Assistant's Netatmo integration uses it, and tested against a fake Netatmo with responses shaped like the real ones. These things are not checked:

- Whether Netatmo accepts an `http://` address or a local address as redirect URI. If it requires https, use an https address: the code is in the address bar even when the page cannot be opened.
- The error codes in Netatmo's responses: 2 and 3 for an invalid or expired access token, 26 for a used-up quota, and whether they come with HTTP 403.
- Netatmo's limits of 50 requests per 10 seconds and 500 per hour per user.
- `setroomthermpoint` with `mode=off` to turn off a room, and that a manual temperature without `endtime` lasts the time set in the app.
- `setstate` with `monitoring` and `floodlight` on the cameras, and that new indoor cameras (`NPC`) work like the old ones.
- That `wifi_status` and `rf_status` can be read as dBm without a sign.
- That `getstationsdata` always gives metric units (°C, mbar, mm, km/h), even when the account shows other units in the app.
- That `boiler_status` exists on the OpenTherm thermostat (`OTM`), and that Smarther (`BNS`) is controlled like the others.
- The fields `battery_state`, `open_window`, `heating_power_request` and `out_of_sight` on real systems.

Smoke alarms, CO alarms, doorbell, sirens and Legrand/BTicino Home + Control are not included. Switching between weekly schedules is not included either.

## Development

Built in Rust on the `thehome-plugin` SDK from [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: OAuth and Netatmo's API
- `src/model.rs`: Netatmo's data and how it becomes states and commands
- `src/lib.rs`: the connection to HomewrightHQ
- `tests/netatmo.rs`: tests against a fake Netatmo with the responses in `tests/fixtures`, and a fake HomewrightHQ in `tests/host`

```
cargo test
cargo build --release
```

A new version is released with a tag, e.g. `git tag v0.1.0 && git push origin v0.1.0`. The workflow builds for Windows, Linux (x86_64 and arm64, static musl) and macOS and puts the binaries and `homewright.json` on the release.

## License

MIT
