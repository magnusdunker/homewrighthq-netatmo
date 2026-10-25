//! Netatmo: the weather station, thermostats and radiator valves, and the
//! indoor and outdoor cameras, through the Netatmo Connect API, as an
//! external integration for HomewrightHQ.
//!
//! The user creates an app on dev.netatmo.com and enters its client id and
//! secret. The setup shows Netatmo's login page as a link; after the login
//! Netatmo sends the browser to the app's redirect address, and the user
//! pastes that address back into the form. The plugin protocol has no way
//! to receive the redirect itself.
//!
//! Every five minutes the integration reads the weather stations and the
//! status of each home: one request plus one per home.

pub mod api;
pub mod model;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use homewright_plugin::{async_trait, json, Device, Entity, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};

use api::{ApiError, Client, Urls};
use model::{Command, Home, Measure};

pub const DOMAIN: &str = "netatmo";
pub const DEFAULT_SCAN_MINUTES: u64 = 5;
const MIN_SCAN_MINUTES: u64 = 2;
const MAX_SCAN_MINUTES: u64 = 60;
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(30 * 60);
pub const DEFAULT_REDIRECT: &str = "http://localhost:8123/auth/external/callback";

/// Logging to HomewrightHQ's log through stderr.
#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    homewright_plugin::log(level, message);
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::emit("INFO", format!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::emit("WARN", format!($($t)*)) };
}

const SETUP_TEXT: &str = "Netatmo kræver, at du opretter din egen app:

1. Log ind på dev.netatmo.com med din Netatmo-konto (den samme som i Netatmo-appen).
2. Opret en app under »My apps«. Skriv denne adresse som redirect URI:
   {redirect_uri}
3. Kopiér appens client ID og client secret ind nedenfor.

Redirect-adressen skal stå præcis ens her og hos Netatmo. HomewrightHQ kan ikke selv tage imod svaret fra Netatmo, så i næste trin kopierer du adressen fra browseren tilbage hertil.";

const AUTH_TEXT: &str = "Åbn denne adresse, log ind med din Netatmo-konto og giv appen adgang:

{url}

Netatmo sender derefter browseren videre til redirect-adressen. Siden dér kan vise en fejl; det gør ikke noget. Kopiér hele adressen fra browserens adresselinje (den indeholder code=...) og indsæt den nedenfor. Koden virker kun i nogle minutter.";

fn user_form(d: &Map<String, Value>, errors: Value) -> FlowReply {
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let redirect = Some(s("redirect_uri")).filter(|r| !r.is_empty()).unwrap_or_else(|| DEFAULT_REDIRECT.into());
    let schema = json!({
        "type": "object",
        "required": ["client_id", "client_secret", "redirect_uri"],
        "properties": {
            "client_id": { "type": "string", "title": "Client ID", "default": s("client_id") },
            "client_secret": { "type": "string", "format": "password", "title": "Client secret" },
            "redirect_uri": { "type": "string", "title": "Redirect URI", "default": redirect, "description": "Skal stå præcis sådan i appen på dev.netatmo.com" }
        }
    });
    let mut r = FlowReply::form_with_errors("user", schema, errors);
    r.result["description"] = json!(SETUP_TEXT);
    r.result["description_placeholders"] = json!({ "redirect_uri": redirect });
    r
}

fn auth_form(url: &str, errors: Value) -> FlowReply {
    let schema = json!({
        "type": "object",
        "required": ["redirect_url"],
        "properties": {
            "redirect_url": { "type": "string", "title": "Adressen, Netatmo sendte dig til", "description": "Hele adressen fra adresselinjen, eller kun koden efter code=" }
        }
    });
    let mut r = FlowReply::form_with_errors("auth", schema, errors);
    r.result["description"] = json!(AUTH_TEXT);
    r.result["description_placeholders"] = json!({ "url": url });
    r
}

fn options_schema(minutes: u64) -> Value {
    json!({
        "type": "object",
        "properties": {
            "scan_interval": {
                "type": "integer", "title": "Minutter mellem opdateringer", "default": minutes,
                "minimum": MIN_SCAN_MINUTES, "maximum": MAX_SCAN_MINUTES,
                "description": "Vejrstationen sender nye målinger hvert tiende minut. Hver opdatering bruger én forespørgsel plus én pr. hjem."
            }
        }
    })
}

/// Endpoints; tests put mock URLs in the entry or flow data.
fn urls(get: impl Fn(&str) -> Option<String>) -> Urls {
    let d = Urls::default();
    Urls { authorize: get("authorize_url").unwrap_or(d.authorize), token: get("token_url").unwrap_or(d.token), api: get("api_url").unwrap_or(d.api) }
}

const URL_KEYS: [&str; 3] = ["authorize_url", "token_url", "api_url"];

pub fn scan_interval(entry: &Entry) -> Duration {
    let m = entry.get("scan_interval").and_then(Value::as_u64).unwrap_or(DEFAULT_SCAN_MINUTES).clamp(MIN_SCAN_MINUTES, MAX_SCAN_MINUTES);
    Duration::from_secs(m * 60)
}

fn str_of(m: &Map<String, Value>, k: &str) -> String {
    m.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Homes with something to show: heated rooms, cameras or known persons.
fn useful(homes: Vec<Home>) -> Vec<Home> {
    homes
        .into_iter()
        .filter(|h| !h.heated_rooms().is_empty() || !h.persons.is_empty() || h.modules.iter().any(|m| model::CAMERA_TYPES.contains(&m.kind.as_str())))
        .collect()
}

#[derive(Default)]
pub struct Netatmo {
    accounts: Mutex<HashMap<String, Arc<Account>>>,
}

impl Netatmo {
    fn account(&self, entry_id: &str) -> Result<Arc<Account>, Error> {
        self.accounts.lock().unwrap().get(entry_id).cloned().ok_or_else(|| Error::unavailable("opsætningen kører ikke"))
    }
}

#[async_trait]
impl Integration for Netatmo {
    async fn config_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let entry_data = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
        match (step, input) {
            ("user", None) => Ok(user_form(&flow.data, json!({}))),
            ("reauth", None) => {
                let mut d = flow.data.clone();
                for (k, v) in &entry_data {
                    d.entry(k.clone()).or_insert(v.clone());
                }
                Ok(user_form(&d, json!({})).with_data(d))
            }
            ("user", Some(v)) => {
                let mut d = flow.data.clone();
                for k in ["client_id", "client_secret", "redirect_uri"] {
                    d.insert(k.into(), json!(v[k].as_str().unwrap_or("").trim()));
                }
                if str_of(&d, "client_id").is_empty() || str_of(&d, "client_secret").is_empty() || reqwest::Url::parse(&str_of(&d, "redirect_uri")).is_err() {
                    return Ok(user_form(&d, json!({ "base": "invalid_client" })).with_data(d));
                }
                let state = api::new_state();
                d.insert("oauth_state".into(), json!(state));
                let u = urls(|k| d.get(k).and_then(Value::as_str).map(String::from));
                let url = api::authorize_url(&u, &str_of(&d, "client_id"), &str_of(&d, "redirect_uri"), &state);
                d.insert("authorize".into(), json!(url));
                Ok(auth_form(&url, json!({})).with_data(d))
            }
            ("auth", Some(v)) => {
                let d = flow.data.clone();
                let url = str_of(&d, "authorize");
                let code = match api::parse_redirect(v["redirect_url"].as_str().unwrap_or(""), &str_of(&d, "oauth_state")) {
                    Ok(c) => c,
                    Err(e) => return Ok(auth_form(&url, json!({ "base": e })).with_data(d)),
                };
                let u = urls(|k| d.get(k).and_then(Value::as_str).map(String::from));
                let (id, secret, redirect) = (str_of(&d, "client_id"), str_of(&d, "client_secret"), str_of(&d, "redirect_uri"));
                let tokens = match api::exchange_code(&api::http(), &u, &id, &secret, &redirect, &code).await {
                    Ok(t) => t,
                    Err(e) => {
                        info!("{e}");
                        let code = if matches!(e, ApiError::Auth(_)) { "invalid_grant" } else { "cannot_connect" };
                        return Ok(auth_form(&url, json!({ "base": code })).with_data(d));
                    }
                };
                let Some(refresh) = tokens.refresh_token.clone() else {
                    return Ok(auth_form(&url, json!({ "base": "no_refresh_token" })).with_data(d));
                };
                let client = Client::new(u, &id, &secret, &refresh, Some(tokens));
                let found = match (client.stations().await, client.homes().await) {
                    (Ok(st), Ok(h)) => !model::weather_modules(&st).is_empty() || !useful(model::homes(&h)).is_empty(),
                    (Err(e), _) | (_, Err(e)) => {
                        info!("{e}");
                        return Ok(auth_form(&url, json!({ "base": "cannot_connect" })).with_data(d));
                    }
                };
                let refresh = client.take_new_refresh().unwrap_or(refresh);
                let mut data = if flow.source == "reauth" { entry_data } else { Map::new() };
                for k in ["client_id", "client_secret", "redirect_uri"].into_iter().chain(URL_KEYS) {
                    if let Some(x) = d.get(k) {
                        data.insert(k.into(), x.clone());
                    }
                }
                data.insert("refresh_token".into(), json!(refresh));
                if flow.source == "reauth" {
                    return Ok(FlowReply::create("", Value::Object(data)));
                }
                if !found {
                    return Ok(FlowReply::abort("no_devices_found"));
                }
                Ok(FlowReply::create("Netatmo", Value::Object(data)).with_unique_id(id))
            }
            (s, _) => Err(Error::invalid(format!("ukendt trin {s}"))),
        }
    }

    async fn options_flow(&self, _host: &Host, flow: Flow, _step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let current = flow.entry.as_ref().map(|e| scan_interval(e).as_secs() / 60).unwrap_or(DEFAULT_SCAN_MINUTES);
        match input {
            None => Ok(FlowReply::form("init", options_schema(current))),
            Some(v) => {
                let m = v["scan_interval"].as_u64().unwrap_or(current).clamp(MIN_SCAN_MINUTES, MAX_SCAN_MINUTES);
                Ok(FlowReply::create("", json!({ "scan_interval": m })))
            }
        }
    }

    async fn setup_entry(&self, host: &Host, entry: Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let data = entry.data.as_object().cloned().unwrap_or_default();
        let refresh = str_of(&data, "refresh_token");
        if refresh.is_empty() {
            return Err(Error::auth_failed("intet refresh token; log ind igen"));
        }
        let client = Arc::new(Client::new(urls(|k| entry.get_str(k).map(String::from)), &str_of(&data, "client_id"), &str_of(&data, "client_secret"), &refresh, None));
        let account = Arc::new(Account {
            entry_id: entry_id.clone(),
            host: host.clone(),
            client: client.clone(),
            data: Mutex::new(data),
            stations: Mutex::new(Value::Null),
            homes: Mutex::new(vec![]),
            status: Mutex::new(HashMap::new()),
            entities: Mutex::new(vec![]),
            task: Mutex::new(None),
            paused_until: Mutex::new(None),
        });
        let first = async {
            let stations = client.stations().await?;
            let homes = useful(model::homes(&client.homes().await?));
            let mut status = HashMap::new();
            for h in &homes {
                status.insert(h.id.clone(), model::home_status(&client.home_status(&h.id).await?));
            }
            Ok::<_, ApiError>((stations, homes, status))
        };
        let (stations, homes, status) = match first.await {
            Ok(x) => x,
            Err(ApiError::Auth(e)) => {
                account.persist().await;
                return Err(Error::auth_failed(format!("Netatmo afviste login: {e}")));
            }
            Err(e) => {
                account.persist().await;
                return Err(Error::not_ready(e));
            }
        };
        account.persist().await;
        let _ = host.delete_issue(&entry_id, "auth").await;
        *account.stations.lock().unwrap() = stations;
        *account.homes.lock().unwrap() = homes;
        *account.status.lock().unwrap() = status;
        account.register().await?;
        account.publish().await;

        let interval = scan_interval(&entry);
        let acc = account.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                acc.poll().await;
            }
        });
        *account.task.lock().unwrap() = Some(task);
        if let Some(old) = self.accounts.lock().unwrap().insert(entry_id, account) {
            old.stop();
        }
        Ok(())
    }

    async fn unload_entry(&self, _host: &Host, entry_id: &str) -> Result<(), Error> {
        if let Some(a) = self.accounts.lock().unwrap().remove(entry_id) {
            a.stop();
        }
        Ok(())
    }

    async fn entity_service(&self, _host: &Host, entry_id: &str, entity_id: &str, service: &str, data: Map<String, Value>) -> Result<(), Error> {
        self.account(entry_id)?.call_service(entity_id, service, &data).await
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = entry.data.clone();
        if let Value::Object(m) = &mut d {
            for k in ["client_secret", "refresh_token"] {
                if m.contains_key(k) {
                    m.insert(k.into(), json!("**REDACTED**"));
                }
            }
        }
        let (stations, status) = self
            .accounts
            .lock()
            .unwrap()
            .get(&entry.entry_id)
            .map(|a| (a.stations.lock().unwrap().clone(), json!(a.status.lock().unwrap().clone())))
            .unwrap_or((Value::Null, Value::Null));
        json!({ "entry": d, "stations": stations, "home_status": status })
    }
}

/// A state and its attributes.
type StateUpdate = (String, Map<String, Value>);

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Measure(String, Measure),
    WeatherBattery(String),
    WeatherSignal(String),
    /// (home, room)
    Climate(String, String),
    Window(String, String),
    /// (home, module)
    Battery(String, String),
    BatteryLow(String, String),
    Boiler(String, String),
    Monitoring(String, String),
    Floodlight(String, String),
    /// (home, person)
    Person(String, String),
}

struct Account {
    entry_id: String,
    host: Host,
    client: Arc<Client>,
    /// The entry's data, kept to save a new refresh token.
    data: Mutex<Map<String, Value>>,
    /// The last `getstationsdata` answer.
    stations: Mutex<Value>,
    homes: Mutex<Vec<Home>>,
    /// The `home` of the last `homestatus` per home, with commands applied.
    status: Mutex<HashMap<String, Value>>,
    entities: Mutex<Vec<(String, Kind)>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    paused_until: Mutex<Option<Instant>>,
}

fn on_off(b: bool) -> String {
    if b { "on" } else { "off" }.to_string()
}

impl Account {
    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }

    /// Save a refresh token Netatmo replaced.
    async fn persist(&self) {
        let Some(r) = self.client.take_new_refresh() else { return };
        let data = {
            let mut d = self.data.lock().unwrap();
            d.insert("refresh_token".into(), json!(r));
            d.clone()
        };
        if let Err(e) = self.host.update_entry_data(&self.entry_id, Value::Object(data)).await {
            warn!("kunne ikke gemme det nye refresh token: {e}");
        }
    }

    async fn register(&self) -> Result<(), Error> {
        let host = &self.host;
        let stations = self.stations.lock().unwrap().clone();
        let homes = self.homes.lock().unwrap().clone();
        let mut list: Vec<(Kind, Entity, bool)> = vec![];

        for w in model::weather_modules(&stations) {
            let dev = host
                .register_device(
                    &self.entry_id,
                    Device {
                        identifiers: vec![(DOMAIN.into(), w.id.clone())],
                        name: w.name.clone(),
                        manufacturer: Some("Netatmo".into()),
                        model: Some(model::weather_model(&w.kind).into()),
                        sw_version: w.firmware.clone(),
                        via_device: w.parent.clone().map(|p| (DOMAIN.to_string(), p)),
                        ..Default::default()
                    },
                )
                .await?;
            for me in model::measures(&w.kind) {
                let mut e = Entity::new("sensor", format!("{}-{}", w.id, me.key)).name(me.name).device(&dev).unit(me.unit).state_class(me.state_class);
                if let Some(c) = me.class {
                    e = e.device_class(c);
                }
                if me.unit == "°" {
                    e = e.icon("mdi:compass-outline");
                }
                list.push((Kind::Measure(w.id.clone(), *me), e, false));
            }
            if w.parent.is_some() {
                let e = Entity::new("sensor", format!("{}-battery", w.id)).name("Batteri").device(&dev).device_class("battery").unit("%").state_class("measurement").category("diagnostic");
                list.push((Kind::WeatherBattery(w.id.clone()), e, false));
            }
            let e = Entity::new("sensor", format!("{}-signal", w.id))
                .name("Signal")
                .device(&dev)
                .device_class("signal_strength")
                .unit("dBm")
                .state_class("measurement")
                .category("diagnostic")
                .disabled_by_default();
            list.push((Kind::WeatherSignal(w.id.clone()), e, false));
        }

        for h in &homes {
            for room in h.heated_rooms() {
                let dev = host
                    .register_device(
                        &self.entry_id,
                        Device {
                            identifiers: vec![(DOMAIN.into(), format!("{}-{}", h.id, room.id))],
                            name: room.name.clone(),
                            manufacturer: Some("Netatmo".into()),
                            model: Some("Rum".into()),
                            suggested_area: Some(room.name.clone()),
                            ..Default::default()
                        },
                    )
                    .await?;
                let e = Entity::new("climate", format!("{}-{}", h.id, room.id)).device(&dev).capabilities(json!({
                    "hvac_modes": model::HVAC_MODES, "preset_modes": model::PRESETS, "min_temp": model::MIN_TEMP,
                    "max_temp": model::MAX_TEMP, "target_temp_step": 0.5, "supported_features": model::CLIMATE_FEATURES }));
                list.push((Kind::Climate(h.id.clone(), room.id.clone()), e, true));
                let e = Entity::new("binary_sensor", format!("{}-{}-window", h.id, room.id)).name("Åbent vindue").device(&dev).device_class("window");
                list.push((Kind::Window(h.id.clone(), room.id.clone()), e, false));
            }

            for mo in &h.modules {
                let kind = mo.kind.as_str();
                let battery = model::BATTERY_TYPES.contains(&kind);
                let boiler = model::BOILER_TYPES.contains(&kind);
                let camera = model::CAMERA_TYPES.contains(&kind);
                if !battery && !boiler && !camera {
                    continue;
                }
                let dev = host
                    .register_device(
                        &self.entry_id,
                        Device {
                            identifiers: vec![(DOMAIN.into(), mo.id.clone())],
                            name: mo.name.clone(),
                            manufacturer: Some("Netatmo".into()),
                            model: Some(model::module_model(kind).into()),
                            suggested_area: mo.room_id.as_deref().and_then(|r| h.room_name(r)),
                            ..Default::default()
                        },
                    )
                    .await?;
                let ids = (h.id.clone(), mo.id.clone());
                if battery {
                    let e = Entity::new("sensor", format!("{}-battery_state", mo.id))
                        .name("Batteri")
                        .device(&dev)
                        .device_class("enum")
                        .icon("mdi:battery")
                        .category("diagnostic")
                        .capabilities(json!({ "options": model::BATTERY_STATES }));
                    list.push((Kind::Battery(ids.0.clone(), ids.1.clone()), e, false));
                    let e = Entity::new("binary_sensor", format!("{}-battery_low", mo.id)).name("Lavt batteri").device(&dev).device_class("battery").category("diagnostic");
                    list.push((Kind::BatteryLow(ids.0.clone(), ids.1.clone()), e, false));
                }
                if boiler {
                    let e = Entity::new("binary_sensor", format!("{}-boiler", mo.id)).name("Kedel").device(&dev).device_class("heat");
                    list.push((Kind::Boiler(ids.0.clone(), ids.1.clone()), e, false));
                }
                if camera {
                    let e = Entity::new("switch", format!("{}-monitoring", mo.id)).name("Overvågning").device(&dev).icon("mdi:cctv");
                    list.push((Kind::Monitoring(ids.0.clone(), ids.1.clone()), e, true));
                    if kind == "NOC" {
                        let e = Entity::new("select", format!("{}-floodlight", mo.id))
                            .name("Projektør")
                            .device(&dev)
                            .icon("mdi:light-flood-down")
                            .capabilities(json!({ "options": model::FLOODLIGHT }));
                        list.push((Kind::Floodlight(ids.0.clone(), ids.1.clone()), e, true));
                    }
                }
            }

            if !h.persons.is_empty() {
                let dev = host
                    .register_device(
                        &self.entry_id,
                        Device { identifiers: vec![(DOMAIN.into(), h.id.clone())], name: h.name.clone(), manufacturer: Some("Netatmo".into()), model: Some("Hjem".into()), ..Default::default() },
                    )
                    .await?;
                for p in &h.persons {
                    let e = Entity::new("binary_sensor", format!("{}-person-{}", h.id, p.id)).name(&format!("{} hjemme", p.name)).device(&dev).device_class("presence");
                    list.push((Kind::Person(h.id.clone(), p.id.clone()), e, false));
                }
            }
        }

        for (kind, e, handler) in list {
            if let Some(entity_id) = host.add_entity(&self.entry_id, e, handler).await? {
                self.entities.lock().unwrap().push((entity_id, kind));
            }
        }
        Ok(())
    }

    /// The state of one entity; `None` means unavailable.
    fn value(&self, kind: &Kind, stations: &Value, status: &HashMap<String, Value>) -> Option<StateUpdate> {
        let plain = |s: String| Some((s, Map::new()));
        let opt = |v: Option<String>| Some((v.unwrap_or_else(|| "unknown".into()), Map::new()));
        let weather = |id: &str| model::weather_raw(stations, id).filter(|r| model::weather_available(r));
        let module = |home: &str, id: &str| status.get(home).and_then(|s| model::module_status(s, id)).filter(|m| model::reachable(m));
        match kind {
            Kind::Measure(id, me) => opt(model::measurement(weather(id)?, me.field).map(|v| v.to_string())),
            Kind::WeatherBattery(id) => opt(model::battery_percent(model::weather_raw(stations, id)?).map(|v| v.to_string())),
            Kind::WeatherSignal(id) => opt(model::signal_dbm(model::weather_raw(stations, id)?).map(|v| v.to_string())),
            Kind::Climate(home, room) => {
                let s = status.get(home)?;
                let r = model::room_status(s, room).filter(|r| model::reachable(r))?;
                let h = self.homes.lock().unwrap().iter().find(|h| h.id == *home).cloned()?;
                let boiler = h
                    .rooms
                    .iter()
                    .find(|x| x.id == *room)
                    .and_then(|x| x.module_ids.iter().filter_map(|m| model::module_status(s, m)).find_map(model::boiler_on));
                Some(model::room_climate(r, boiler))
            }
            Kind::Window(home, room) => {
                let r = model::room_status(status.get(home)?, room).filter(|r| model::reachable(r))?;
                opt(r["open_window"].as_bool().map(on_off))
            }
            Kind::Battery(home, id) => opt(model::battery_state(module(home, id)?).map(String::from)),
            Kind::BatteryLow(home, id) => opt(model::battery_low(module(home, id)?).map(on_off)),
            Kind::Boiler(home, id) => opt(model::boiler_on(module(home, id)?).map(on_off)),
            Kind::Monitoring(home, id) => model::monitoring(module(home, id)?).map(on_off).and_then(plain),
            Kind::Floodlight(home, id) => opt(model::floodlight(module(home, id)?).map(String::from)),
            Kind::Person(home, id) => opt(model::person_home(status.get(home)?, id).map(on_off)),
        }
    }

    /// Push states for every entity.
    async fn publish(&self) {
        let updates: Vec<(String, Option<StateUpdate>)> = {
            let stations = self.stations.lock().unwrap().clone();
            let status = self.status.lock().unwrap().clone();
            let entities = self.entities.lock().unwrap().clone();
            entities.iter().map(|(id, kind)| (id.clone(), self.value(kind, &stations, &status))).collect()
        };
        for (entity_id, value) in updates {
            let res = match value {
                Some((state, attrs)) => self.host.set_state(&entity_id, state, attrs).await,
                None => self.host.set_unavailable(&entity_id).await,
            };
            if let Err(e) = res {
                warn!("kunne ikke opdatere {entity_id}: {e}");
            }
        }
    }

    fn paused(&self) -> bool {
        self.paused_until.lock().unwrap().is_some_and(|t| Instant::now() < t)
    }

    async fn poll(&self) {
        if self.paused() {
            return;
        }
        let ok = match self.client.stations().await {
            Ok(v) => {
                *self.stations.lock().unwrap() = v;
                true
            }
            Err(e) => {
                self.report(&e).await;
                false
            }
        };
        let homes: Vec<String> = self.homes.lock().unwrap().iter().map(|h| h.id.clone()).collect();
        let mut all_ok = ok;
        for id in homes {
            if self.paused() {
                break;
            }
            match self.client.home_status(&id).await {
                Ok(v) => {
                    self.status.lock().unwrap().insert(id, model::home_status(&v));
                }
                Err(e) => {
                    all_ok = false;
                    self.report(&e).await;
                }
            }
        }
        self.persist().await;
        if all_ok {
            let _ = self.host.delete_issue(&self.entry_id, "auth").await;
        }
        self.publish().await;
    }

    async fn report(&self, e: &ApiError) {
        match e {
            ApiError::Auth(_) => {
                let r = self
                    .host
                    .create_issue(&self.entry_id, "auth", "error", "Netatmo: login afvist", "Netatmo afviste integrationens adgang. Godkend integrationen igen.")
                    .await;
                if let Err(e) = r {
                    warn!("kunne ikke oprette problem: {e}");
                }
            }
            ApiError::RateLimited => {
                warn!("Netatmos grænse for forespørgsler er nået; venter {} minutter", RATE_LIMIT_PAUSE.as_secs() / 60);
                *self.paused_until.lock().unwrap() = Some(Instant::now() + RATE_LIMIT_PAUSE);
            }
            other => warn!("{other}"),
        }
    }

    async fn call_service(&self, entity_id: &str, service: &str, data: &Map<String, Value>) -> Result<(), Error> {
        let kind = self.entities.lock().unwrap().iter().find(|(e, _)| e == entity_id).map(|(_, k)| k.clone()).ok_or_else(|| Error::new("not_found", entity_id))?;
        if self.paused() {
            return Err(Error::unavailable("Netatmo har bedt om en pause efter for mange forespørgsler"));
        }
        let res = match &kind {
            Kind::Climate(home, room) => {
                let r = self.status.lock().unwrap().get(home).and_then(|s| model::room_status(s, room).cloned()).ok_or_else(|| Error::unavailable(entity_id))?;
                let cmd = model::room_command(service, data, &r).map_err(|m| if m.is_empty() { Error::not_supported(service) } else { Error::invalid(m) })?;
                let res = match &cmd {
                    Command::Room { mode, temp } => self.client.set_room(home, room, mode, *temp).await,
                    Command::Home(mode) => self.client.set_home_mode(home, mode).await,
                };
                if res.is_ok() {
                    if let Some(s) = self.status.lock().unwrap().get_mut(home) {
                        model::apply(s, room, &cmd);
                    }
                }
                res
            }
            Kind::Monitoring(home, id) => {
                let on = match service {
                    "turn_on" => "on",
                    "turn_off" => "off",
                    "toggle" => {
                        let cur = self.status.lock().unwrap().get(home).and_then(|s| model::module_status(s, id).and_then(model::monitoring));
                        if cur == Some(true) {
                            "off"
                        } else {
                            "on"
                        }
                    }
                    _ => return Err(Error::not_supported(service)),
                };
                self.set_module(home, id, "monitoring", on).await
            }
            Kind::Floodlight(home, id) if service == "select_option" => {
                let opt = data.get("option").and_then(Value::as_str).unwrap_or_default();
                let Some(o) = model::FLOODLIGHT.iter().find(|f| **f == opt) else {
                    return Err(Error::invalid(format!("ukendt valg: {opt}")));
                };
                self.set_module(home, id, "floodlight", o).await
            }
            _ => return Err(Error::not_supported(service)),
        };
        self.persist().await;
        if let Err(e) = res {
            self.report(&e).await;
            return Err(match e {
                ApiError::Refused(m) => Error::invalid(m),
                e => Error::failed(e),
            });
        }
        self.publish().await;
        Ok(())
    }

    /// `setstate` for one field of one module, and the stored status after it.
    async fn set_module(&self, home: &str, id: &str, field: &str, value: &str) -> Result<(), ApiError> {
        self.client.set_state(&model::set_state_body(home, id, field, value)).await?;
        if let Some(m) = self.status.lock().unwrap().get_mut(home).and_then(|s| s["modules"].as_array_mut()).and_then(|ms| ms.iter_mut().find(|m| m["id"] == id)) {
            m[field] = json!(value);
        }
        Ok(())
    }
}
