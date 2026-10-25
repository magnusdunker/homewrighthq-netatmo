//! Netatmo's data and how it maps to entity states and commands.
//!
//! The weather station comes from `getstationsdata`. Thermostats, valves and
//! cameras come from `homesdata` (rooms, modules, persons, names) and
//! `homestatus` (the current values per room and module).

use serde_json::{json, Map, Value};

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn num(m: &Value, k: &str) -> Option<f64> {
    m.get(k).and_then(Value::as_f64)
}

// ---------------------------------------------------------------------------
// weather station

#[derive(Clone, Debug, PartialEq)]
pub struct WeatherModule {
    /// The MAC address Netatmo uses as id.
    pub id: String,
    /// `NAMain`, `NAModule1` (outdoor), `NAModule2` (wind), `NAModule3`
    /// (rain) or `NAModule4` (extra indoor).
    pub kind: String,
    pub name: String,
    /// The station's base for a module, `None` for the base itself.
    pub parent: Option<String>,
    pub firmware: Option<String>,
}

/// A measurement shown as a sensor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measure {
    pub key: &'static str,
    /// The field in `dashboard_data`.
    pub field: &'static str,
    pub name: &'static str,
    pub class: Option<&'static str>,
    pub unit: &'static str,
    pub state_class: &'static str,
}

const fn m(key: &'static str, field: &'static str, name: &'static str, class: Option<&'static str>, unit: &'static str) -> Measure {
    Measure { key, field, name, class, unit, state_class: "measurement" }
}

pub const TEMPERATURE: Measure = m("temperature", "Temperature", "Temperatur", Some("temperature"), "°C");
pub const HUMIDITY: Measure = m("humidity", "Humidity", "Luftfugtighed", Some("humidity"), "%");
pub const CO2: Measure = m("co2", "CO2", "CO2", Some("carbon_dioxide"), "ppm");
pub const NOISE: Measure = m("noise", "Noise", "Støj", Some("sound_pressure"), "dB");
pub const PRESSURE: Measure = m("pressure", "Pressure", "Lufttryk", Some("atmospheric_pressure"), "mbar");
pub const RAIN: Measure = m("rain", "Rain", "Regn", Some("precipitation"), "mm");
pub const RAIN_HOUR: Measure = m("rain_hour", "sum_rain_1", "Regn sidste time", Some("precipitation"), "mm");
pub const RAIN_TODAY: Measure = Measure { state_class: "total_increasing", ..m("rain_today", "sum_rain_24", "Regn i dag", Some("precipitation"), "mm") };
pub const WIND: Measure = m("wind", "WindStrength", "Vind", Some("wind_speed"), "km/h");
pub const WIND_ANGLE: Measure = m("wind_angle", "WindAngle", "Vindretning", None, "°");
pub const GUST: Measure = m("gust", "GustStrength", "Vindstød", Some("wind_speed"), "km/h");
pub const GUST_ANGLE: Measure = m("gust_angle", "GustAngle", "Vindstødsretning", None, "°");

/// What each kind of weather module measures.
pub fn measures(kind: &str) -> &'static [Measure] {
    match kind {
        "NAMain" => &[TEMPERATURE, HUMIDITY, CO2, NOISE, PRESSURE],
        "NAModule1" => &[TEMPERATURE, HUMIDITY],
        "NAModule2" => &[WIND, WIND_ANGLE, GUST, GUST_ANGLE],
        "NAModule3" => &[RAIN, RAIN_HOUR, RAIN_TODAY],
        "NAModule4" => &[TEMPERATURE, HUMIDITY, CO2],
        _ => &[],
    }
}

pub fn weather_model(kind: &str) -> &'static str {
    match kind {
        "NAMain" => "Vejrstation, indendørs",
        "NAModule1" => "Udendørsmodul",
        "NAModule2" => "Vindmåler",
        "NAModule3" => "Regnmåler",
        "NAModule4" => "Ekstra indendørsmodul",
        _ => "Vejrmodul",
    }
}

/// Every station base and module in a `getstationsdata` answer.
pub fn weather_modules(v: &Value) -> Vec<WeatherModule> {
    let mut out = vec![];
    for d in v["body"]["devices"].as_array().into_iter().flatten() {
        let id = s(d, "_id");
        if id.is_empty() {
            continue;
        }
        let station = Some(s(d, "station_name")).filter(|n| !n.is_empty()).unwrap_or_else(|| "Netatmo".into());
        let name = match d["module_name"].as_str().filter(|n| !n.is_empty()) {
            Some(m) => format!("{station} {m}"),
            None => station.clone(),
        };
        out.push(WeatherModule { id: id.clone(), kind: s(d, "type"), name, parent: None, firmware: d["firmware"].as_u64().map(|f| f.to_string()) });
        for mo in d["modules"].as_array().into_iter().flatten() {
            let kind = s(mo, "type");
            let mname = mo["module_name"].as_str().filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| weather_model(&kind).into());
            out.push(WeatherModule {
                id: s(mo, "_id"),
                kind,
                name: format!("{station} {mname}"),
                parent: Some(id.clone()),
                firmware: mo["firmware"].as_u64().map(|f| f.to_string()),
            });
        }
    }
    out
}

/// The raw object of one base or module in a `getstationsdata` answer.
pub fn weather_raw<'a>(v: &'a Value, id: &str) -> Option<&'a Value> {
    for d in v["body"]["devices"].as_array().into_iter().flatten() {
        if d["_id"] == id {
            return Some(d);
        }
        if let Some(m) = d["modules"].as_array().into_iter().flatten().find(|m| m["_id"] == id) {
            return Some(m);
        }
    }
    None
}

/// Whether the module answers and has data. Netatmo leaves out
/// `dashboard_data` when it has lost contact.
pub fn weather_available(raw: &Value) -> bool {
    raw["reachable"].as_bool().unwrap_or(true) && raw["dashboard_data"].is_object()
}

pub fn measurement(raw: &Value, field: &str) -> Option<f64> {
    num(&raw["dashboard_data"], field)
}

pub fn battery_percent(raw: &Value) -> Option<f64> {
    num(raw, "battery_percent")
}

/// Signal strength in dBm. Netatmo gives `wifi_status` on the base and
/// `rf_status` on modules as a positive number where lower is better,
/// which reads as the dBm value without its sign.
pub fn signal_dbm(raw: &Value) -> Option<f64> {
    num(raw, "wifi_status").or_else(|| num(raw, "rf_status")).map(|v| -v.abs())
}

// ---------------------------------------------------------------------------
// homes: thermostats, valves and cameras

#[derive(Clone, Debug, PartialEq)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub module_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub room_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Home {
    pub id: String,
    pub name: String,
    pub rooms: Vec<Room>,
    pub modules: Vec<Module>,
    pub persons: Vec<Person>,
}

/// Modules that measure and set a room's temperature.
pub const THERMO_TYPES: [&str; 4] = ["NATherm1", "NRV", "OTM", "BNS"];
/// Modules with a boiler status.
pub const BOILER_TYPES: [&str; 2] = ["NATherm1", "OTM"];
/// Cameras whose monitoring can be switched.
pub const CAMERA_TYPES: [&str; 3] = ["NACamera", "NOC", "NPC"];
/// Battery powered energy modules.
pub const BATTERY_TYPES: [&str; 3] = ["NATherm1", "NRV", "OTM"];

pub fn module_model(kind: &str) -> &'static str {
    match kind {
        "NATherm1" => "Smart Thermostat",
        "NRV" => "Smart Radiator Valve",
        "NAPlug" => "Relæ",
        "OTH" => "OpenTherm-relæ",
        "OTM" => "OpenTherm-termostat",
        "BNS" => "Smarther",
        "NACamera" => "Smart Indoor Camera",
        "NOC" => "Smart Outdoor Camera",
        "NPC" => "Smart Indoor Camera (ny)",
        _ => "Netatmo-modul",
    }
}

/// The homes in a `homesdata` answer.
pub fn homes(v: &Value) -> Vec<Home> {
    v["body"]["homes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| Home {
            id: s(h, "id"),
            name: Some(s(h, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| "Hjem".into()),
            rooms: h["rooms"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| Room {
                    id: s(r, "id"),
                    name: s(r, "name"),
                    module_ids: r["module_ids"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect(),
                })
                .collect(),
            modules: h["modules"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|mo| {
                    let kind = s(mo, "type");
                    Module {
                        id: s(mo, "id"),
                        name: mo["name"].as_str().filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| module_model(&kind).into()),
                        kind,
                        room_id: mo["room_id"].as_str().map(String::from),
                    }
                })
                .collect(),
            persons: h["persons"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| Some(Person { id: s(p, "id"), name: p["pseudo"].as_str().filter(|n| !n.is_empty())?.to_string() }))
                .collect(),
        })
        .filter(|h| !h.id.is_empty())
        .collect()
}

impl Home {
    pub fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.id == id)
    }

    /// Rooms with a thermostat or valve.
    pub fn heated_rooms(&self) -> Vec<&Room> {
        self.rooms.iter().filter(|r| r.module_ids.iter().any(|id| self.module(id).is_some_and(|m| THERMO_TYPES.contains(&m.kind.as_str())))).collect()
    }

    pub fn room_name(&self, id: &str) -> Option<String> {
        self.rooms.iter().find(|r| r.id == id).map(|r| r.name.clone())
    }
}

/// The `home` object of a `homestatus` answer.
pub fn home_status(v: &Value) -> Value {
    v["body"]["home"].clone()
}

pub fn room_status<'a>(status: &'a Value, id: &str) -> Option<&'a Value> {
    status["rooms"].as_array()?.iter().find(|r| r["id"] == id)
}

pub fn module_status<'a>(status: &'a Value, id: &str) -> Option<&'a Value> {
    status["modules"].as_array()?.iter().find(|r| r["id"] == id)
}

pub fn reachable(v: &Value) -> bool {
    v["reachable"].as_bool().unwrap_or(true)
}

pub const HVAC_MODES: [&str; 3] = ["auto", "heat", "off"];
pub const PRESETS: [&str; 5] = ["schedule", "manual", "boost", "away", "frost_guard"];
/// target temperature, preset, turn off, turn on
pub const CLIMATE_FEATURES: u64 = 1 | 16 | 128 | 256;
pub const MIN_TEMP: f64 = 7.0;
pub const MAX_TEMP: f64 = 30.0;

/// Netatmo's `therm_setpoint_mode` as a preset; `None` when the room is off.
pub fn preset(mode: &str) -> Option<&'static str> {
    match mode {
        "manual" => Some("manual"),
        "max" => Some("boost"),
        "away" => Some("away"),
        "hg" => Some("frost_guard"),
        "off" => None,
        _ => Some("schedule"),
    }
}

/// The climate state of a room. `boiler_on` is the boiler status of the
/// room's thermostat, used when the room has no `heating_power_request`.
pub fn room_climate(room: &Value, boiler_on: Option<bool>) -> (String, Map<String, Value>) {
    let mode = room["therm_setpoint_mode"].as_str().unwrap_or("schedule");
    let state = match mode {
        "off" => "off",
        "manual" | "max" => "heat",
        _ => "auto",
    };
    let heating = match num(room, "heating_power_request") {
        Some(p) => p > 0.0,
        None => boiler_on.unwrap_or(false),
    };
    let action = if state == "off" {
        "off"
    } else if heating {
        "heating"
    } else {
        "idle"
    };
    let mut a = Map::new();
    a.insert("hvac_modes".into(), json!(HVAC_MODES));
    a.insert("preset_modes".into(), json!(PRESETS));
    a.insert("preset_mode".into(), json!(preset(mode)));
    a.insert("hvac_action".into(), json!(action));
    a.insert("current_temperature".into(), json!(num(room, "therm_measured_temperature")));
    a.insert("temperature".into(), json!(num(room, "therm_setpoint_temperature")));
    a.insert("min_temp".into(), json!(MIN_TEMP));
    a.insert("max_temp".into(), json!(MAX_TEMP));
    a.insert("target_temp_step".into(), json!(0.5));
    a.insert("supported_features".into(), json!(CLIMATE_FEATURES));
    if let Some(p) = num(room, "heating_power_request") {
        a.insert("heating_power_request".into(), json!(p));
    }
    (state.to_string(), a)
}

/// A command for one room or the whole home.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `setroomthermpoint` with `manual`, `max`, `off` or `home`.
    Room { mode: &'static str, temp: Option<f64> },
    /// `setthermmode` with `schedule`, `away` or `hg`.
    Home(&'static str),
}

fn temp_of(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

pub fn clamp_temp(t: f64) -> f64 {
    ((t * 2.0).round() / 2.0).clamp(MIN_TEMP, MAX_TEMP)
}

/// The command for a climate service on a room. `Err` with an empty
/// message means the service is not supported.
pub fn room_command(service: &str, data: &Map<String, Value>, room: &Value) -> Result<Command, String> {
    let mode = room["therm_setpoint_mode"].as_str().unwrap_or("schedule");
    let current = num(room, "therm_setpoint_temperature").unwrap_or(20.0);
    match service {
        "set_temperature" => {
            let t = data.get("temperature").and_then(temp_of).ok_or("temperature mangler")?;
            Ok(Command::Room { mode: "manual", temp: Some(clamp_temp(t)) })
        }
        "set_hvac_mode" => match data.get("hvac_mode").and_then(Value::as_str).unwrap_or("") {
            "off" => Ok(Command::Room { mode: "off", temp: None }),
            "heat" => Ok(Command::Room { mode: "manual", temp: Some(clamp_temp(current)) }),
            "auto" => Ok(Command::Room { mode: "home", temp: None }),
            m => Err(format!("ukendt tilstand: {m}")),
        },
        "set_preset_mode" => match data.get("preset_mode").and_then(Value::as_str).unwrap_or("") {
            // away and frost guard are set on the whole home, and so is
            // the way back from them
            "schedule" if matches!(mode, "away" | "hg") => Ok(Command::Home("schedule")),
            "schedule" => Ok(Command::Room { mode: "home", temp: None }),
            "manual" => Ok(Command::Room { mode: "manual", temp: Some(clamp_temp(current)) }),
            "boost" => Ok(Command::Room { mode: "max", temp: None }),
            "away" => Ok(Command::Home("away")),
            "frost_guard" => Ok(Command::Home("hg")),
            p => Err(format!("ukendt forindstilling: {p}")),
        },
        "turn_on" => Ok(Command::Room { mode: "home", temp: None }),
        "turn_off" => Ok(Command::Room { mode: "off", temp: None }),
        _ => Err(String::new()),
    }
}

/// Apply a command to the stored `homestatus` so the state changes before
/// the next read. `room_id` is the room the command was given on.
pub fn apply(status: &mut Value, room_id: &str, cmd: &Command) {
    let Some(rooms) = status["rooms"].as_array_mut() else { return };
    for r in rooms {
        match cmd {
            Command::Room { mode, temp } if r["id"] == room_id => {
                r["therm_setpoint_mode"] = json!(if *mode == "home" { "schedule" } else { mode });
                match (mode, temp) {
                    (_, Some(t)) => r["therm_setpoint_temperature"] = json!(t),
                    (&"max", None) => r["therm_setpoint_temperature"] = json!(MAX_TEMP),
                    _ => {}
                }
            }
            Command::Home(mode) => {
                // rooms in manual keep their setpoint until it runs out
                let cur = r["therm_setpoint_mode"].as_str().unwrap_or("");
                if !matches!(cur, "manual" | "max" | "off") || *mode != "schedule" {
                    r["therm_setpoint_mode"] = json!(mode);
                }
            }
            _ => {}
        }
    }
}

pub const BATTERY_STATES: [&str; 5] = ["full", "high", "medium", "low", "very_low"];

pub fn battery_state(module: &Value) -> Option<&str> {
    module["battery_state"].as_str()
}

pub fn battery_low(module: &Value) -> Option<bool> {
    battery_state(module).map(|b| matches!(b, "low" | "very_low"))
}

pub fn boiler_on(module: &Value) -> Option<bool> {
    module["boiler_status"].as_bool()
}

/// A camera's monitoring: `Some(true)` on, `Some(false)` off, `None` when
/// the camera is disconnected or Netatmo does not say.
pub fn monitoring(module: &Value) -> Option<bool> {
    match module["monitoring"].as_str() {
        Some("on") => Some(true),
        Some("off") => Some(false),
        _ => None,
    }
}

pub const FLOODLIGHT: [&str; 3] = ["auto", "on", "off"];

pub fn floodlight(module: &Value) -> Option<&str> {
    module["floodlight"].as_str().filter(|f| FLOODLIGHT.contains(f))
}

/// The `setstate` body that changes one field of one module.
pub fn set_state_body(home_id: &str, module_id: &str, field: &str, value: &str) -> Value {
    json!({ "home": { "id": home_id, "modules": [{ "id": module_id, field: value }] } })
}

/// Whether a known person is at home, from `homestatus`.
pub fn person_home(status: &Value, id: &str) -> Option<bool> {
    let p = status["persons"].as_array()?.iter().find(|p| p["id"] == id)?;
    p["out_of_sight"].as_bool().map(|o| !o)
}
