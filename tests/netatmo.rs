//! Mapping tests on responses shaped like Netatmo's, and the plugin against
//! a fake Netatmo with HomewrightHQ played over a pipe.

mod host;

use homewright_netatmo::api::{self, ApiError, Client, Urls};
use homewright_netatmo::model::{self, Command};
use homewright_netatmo::Netatmo;
use serde_json::{json, Map, Value};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

const HOME: &str = "5f0000000000000000000001";
const FIRST_REFRESH: &str = "5e1a2b3c4d5e6f7a8b9c0d1e|r1r1r1r1r1r1r1r1r1r1r1r1r1r1r1r1";

#[test]
fn weather_station_and_modules() {
    let st = fixture("getstationsdata.json");
    let w = model::weather_modules(&st);
    assert_eq!(w.len(), 5);
    assert_eq!(w[0].name, "Hjemme Stue");
    assert_eq!(w[0].kind, "NAMain");
    assert_eq!(w[0].parent, None);
    assert_eq!(w[0].firmware.as_deref(), Some("202"));
    assert_eq!(w[1].name, "Hjemme Ude");
    assert_eq!(w[1].parent.as_deref(), Some("70:ee:50:00:00:01"));

    let main = model::weather_raw(&st, "70:ee:50:00:00:01").unwrap();
    assert_eq!(model::measurement(main, "CO2"), Some(612.0));
    assert_eq!(model::measurement(main, "Pressure"), Some(1013.2));
    assert_eq!(model::signal_dbm(main), Some(-52.0));
    let rain = model::weather_raw(&st, "05:00:00:00:00:04").unwrap();
    assert_eq!(model::measurement(rain, model::RAIN_TODAY.field), Some(3.4));
    assert_eq!(model::battery_percent(rain), Some(12.0));
    let wind = model::weather_raw(&st, "06:00:00:00:00:03").unwrap();
    assert_eq!(model::measurement(wind, model::GUST.field), Some(27.0));
    // out of reach and without data
    assert!(!model::weather_available(model::weather_raw(&st, "03:00:00:00:00:05").unwrap()));
    assert_eq!(model::measures("NAModule3").len(), 3);
    assert!(model::measures("NAModule9").is_empty());
}

#[test]
fn homes_rooms_and_persons() {
    let homes = model::homes(&fixture("homesdata.json"));
    assert_eq!(homes.len(), 2);
    let h = &homes[0];
    let rooms: Vec<&str> = h.heated_rooms().iter().map(|r| r.id.as_str()).collect();
    // the hall only has a camera
    assert_eq!(rooms, ["1001", "1002"]);
    assert_eq!(h.persons.len(), 1);
    assert_eq!(h.persons[0].name, "Anna");
    assert_eq!(h.module("09:00:00:00:00:12").unwrap().name, "Ventil bad");
    assert!(homes[1].heated_rooms().is_empty());

    let st = model::home_status(&fixture("homestatus.json"));
    assert_eq!(model::person_home(&st, "p-111"), Some(true));
    assert_eq!(model::person_home(&st, "p-222"), Some(false));
    let valve = model::module_status(&st, "09:00:00:00:00:12").unwrap();
    assert_eq!(model::battery_state(valve), Some("very_low"));
    assert_eq!(model::battery_low(valve), Some(true));
    assert_eq!(model::boiler_on(model::module_status(&st, "04:00:00:00:00:10").unwrap()), Some(true));
    assert_eq!(model::monitoring(model::module_status(&st, "70:ee:50:00:00:21").unwrap()), Some(false));
    assert_eq!(model::floodlight(model::module_status(&st, "70:ee:50:00:00:21").unwrap()), Some("auto"));
}

#[test]
fn room_climate_state() {
    let st = model::home_status(&fixture("homestatus.json"));
    let (state, a) = model::room_climate(model::room_status(&st, "1001").unwrap(), Some(true));
    assert_eq!(state, "auto");
    assert_eq!(a["preset_mode"], "schedule");
    // the room says it needs no heat, so the boiler is for another room
    assert_eq!(a["hvac_action"], "idle");
    assert_eq!(a["current_temperature"], 20.5);
    assert_eq!(a["temperature"], 21.0);

    let (state, a) = model::room_climate(model::room_status(&st, "1002").unwrap(), None);
    assert_eq!(state, "heat");
    assert_eq!(a["preset_mode"], "manual");
    assert_eq!(a["hvac_action"], "heating");

    // without heating_power_request the boiler decides
    let (_, a) = model::room_climate(&json!({ "therm_setpoint_mode": "away" }), Some(true));
    assert_eq!(a["preset_mode"], "away");
    assert_eq!(a["hvac_action"], "heating");
    let (state, a) = model::room_climate(&json!({ "therm_setpoint_mode": "off" }), Some(true));
    assert_eq!(state, "off");
    assert_eq!(a["preset_mode"], Value::Null);
    assert_eq!(a["hvac_action"], "off");
}

#[test]
fn room_commands() {
    let room = json!({ "id": "1001", "therm_setpoint_mode": "schedule", "therm_setpoint_temperature": 21 });
    let d = |v: Value| obj(v);
    assert_eq!(model::room_command("set_temperature", &d(json!({ "temperature": 22.3 })), &room).unwrap(), Command::Room { mode: "manual", temp: Some(22.5) });
    assert_eq!(model::room_command("set_temperature", &d(json!({ "temperature": 2 })), &room).unwrap(), Command::Room { mode: "manual", temp: Some(7.0) });
    assert_eq!(model::room_command("set_hvac_mode", &d(json!({ "hvac_mode": "heat" })), &room).unwrap(), Command::Room { mode: "manual", temp: Some(21.0) });
    assert_eq!(model::room_command("set_hvac_mode", &d(json!({ "hvac_mode": "auto" })), &room).unwrap(), Command::Room { mode: "home", temp: None });
    assert_eq!(model::room_command("set_preset_mode", &d(json!({ "preset_mode": "boost" })), &room).unwrap(), Command::Room { mode: "max", temp: None });
    assert_eq!(model::room_command("set_preset_mode", &d(json!({ "preset_mode": "frost_guard" })), &room).unwrap(), Command::Home("hg"));
    assert_eq!(model::room_command("turn_off", &Map::new(), &room).unwrap(), Command::Room { mode: "off", temp: None });
    assert!(model::room_command("set_hvac_mode", &d(json!({ "hvac_mode": "cool" })), &room).is_err());
    assert_eq!(model::room_command("set_fan_mode", &Map::new(), &room).unwrap_err(), "");
    // back from away goes through the home
    let away = json!({ "therm_setpoint_mode": "away" });
    assert_eq!(model::room_command("set_preset_mode", &d(json!({ "preset_mode": "schedule" })), &away).unwrap(), Command::Home("schedule"));

    let mut st = model::home_status(&fixture("homestatus.json"));
    model::apply(&mut st, "1001", &Command::Room { mode: "max", temp: None });
    assert_eq!(model::room_status(&st, "1001").unwrap()["therm_setpoint_mode"], "max");
    assert_eq!(model::room_status(&st, "1001").unwrap()["therm_setpoint_temperature"], 30.0);
    model::apply(&mut st, "1001", &Command::Home("away"));
    assert_eq!(model::room_status(&st, "1002").unwrap()["therm_setpoint_mode"], "away");
}

#[test]
fn redirect_and_authorize_address() {
    let url = api::authorize_url(&Urls::default(), "cid", "http://localhost:8123/auth/external/callback", "st1");
    assert!(url.starts_with("https://api.netatmo.com/oauth2/authorize?response_type=code&client_id=cid"));
    assert!(url.contains("scope=read_station+read_thermostat+write_thermostat"));
    assert!(url.contains("state=st1"));
    assert_eq!(api::parse_redirect("http://localhost:8123/auth/external/callback?state=st1&code=abc", "st1").unwrap(), "abc");
    assert_eq!(api::parse_redirect(" abc ", "st1").unwrap(), "abc");
    assert_eq!(api::parse_redirect("http://x/?state=other&code=abc", "st1").unwrap_err(), "state_mismatch");
    assert_eq!(api::parse_redirect("http://x/?state=st1&error=access_denied", "st1").unwrap_err(), "access_denied");
    assert_eq!(api::parse_redirect("http://x/?state=st1", "st1").unwrap_err(), "missing_code");
}

async fn netatmo(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=good"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("token.json")))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })))
        .mount(server)
        .await;
    // a refresh replaces the refresh token
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "A2", "refresh_token": "R2", "expires_in": 10800 })))
        .mount(server)
        .await;
    let first = fixture("token.json")["access_token"].as_str().unwrap().to_string();
    for token in [first.as_str(), "A2"] {
        let auth = format!("Bearer {token}");
        Mock::given(method("GET"))
            .and(path("/api/getstationsdata"))
            .and(header("authorization", auth.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("getstationsdata.json")))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/homesdata"))
            .and(header("authorization", auth.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("homesdata.json")))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/homestatus"))
            .and(query_param("home_id", HOME))
            .and(header("authorization", auth.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("homestatus.json")))
            .mount(server)
            .await;
    }
    for p in ["/api/setroomthermpoint", "/api/setthermmode", "/api/setstate"] {
        Mock::given(method("POST"))
            .and(path(p))
            .and(header("authorization", "Bearer A2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "ok", "time_server": 1761400000 })))
            .mount(server)
            .await;
    }
}

fn flow(data: Value) -> Value {
    json!({ "flow_id": "f1", "source": "user", "data": data })
}

#[tokio::test(flavor = "multi_thread")]
async fn login_setup_and_commands() {
    let server = MockServer::start().await;
    netatmo(&server).await;
    let h = host::FakeHost::start(Netatmo::default());
    let urls = json!({ "authorize_url": format!("{}/oauth2/authorize", server.uri()), "token_url": format!("{}/oauth2/token", server.uri()), "api_url": server.uri() });

    let r = h.call("config_flow", json!({ "flow": flow(urls.clone()), "step": "user", "input": null })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "user");
    let keys: Vec<&String> = r["result"]["schema"]["properties"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["client_id", "client_secret", "redirect_uri"]);

    let r = h.call("config_flow", json!({ "flow": flow(urls.clone()), "step": "user", "input": { "client_id": "cid", "client_secret": "", "redirect_uri": "x" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_client");

    let input = json!({ "client_id": "cid", "client_secret": "sec", "redirect_uri": "https://hjem.example/auth/external/callback" });
    let r = h.call("config_flow", json!({ "flow": flow(urls.clone()), "step": "user", "input": input })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "auth");
    let url = r["result"]["description_placeholders"]["url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("{}/oauth2/authorize?", server.uri())));
    let data = r["data"].clone();
    let state = data["oauth_state"].as_str().unwrap().to_string();

    let bad = json!({ "redirect_url": format!("https://hjem.example/auth/external/callback?code=old&state={state}") });
    let r = h.call("config_flow", json!({ "flow": flow(data.clone()), "step": "auth", "input": bad })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_grant");

    let ok = json!({ "redirect_url": format!("https://hjem.example/auth/external/callback?state={state}&code=good") });
    let r = h.call("config_flow", json!({ "flow": flow(data.clone()), "step": "auth", "input": ok })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], "Netatmo");
    assert_eq!(r["unique_id"], "cid");
    let entry_data = r["result"]["data"].clone();
    assert_eq!(entry_data["refresh_token"], FIRST_REFRESH);
    assert!(entry_data.get("oauth_state").is_none());
    assert!(entry_data.get("authorize").is_none());

    // setup refreshes the token; the new refresh token is saved on the entry
    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "netatmo", "data": entry_data } })).await.unwrap();
    let updates = h.rec.lock().unwrap().entry_updates.clone();
    assert_eq!(updates.last().unwrap()["refresh_token"], "R2");
    assert_eq!(updates.last().unwrap()["client_secret"], "sec");

    // five weather modules, two rooms, three energy modules, two cameras, one home
    assert_eq!(h.rec.lock().unwrap().devices.len(), 13);
    assert_eq!(h.state(&h.entity("70:ee:50:00:00:01-temperature")).unwrap().0, "21.4");
    assert_eq!(h.state(&h.entity("70:ee:50:00:00:01-noise")).unwrap().0, "38");
    assert_eq!(h.state(&h.entity("02:00:00:00:00:02-humidity")).unwrap().0, "88");
    assert_eq!(h.state(&h.entity("02:00:00:00:00:02-battery")).unwrap().0, "64");
    assert_eq!(h.state(&h.entity("06:00:00:00:00:03-wind_angle")).unwrap().0, "250");
    assert_eq!(h.state(&h.entity("05:00:00:00:00:04-rain_today")).unwrap().0, "3.4");
    assert_eq!(h.state(&h.entity("03:00:00:00:00:05-co2")).unwrap().0, "unavailable");
    let signal = h.entity("70:ee:50:00:00:01-signal");
    let (spec, _) = h.rec.lock().unwrap().entities[&signal].clone();
    assert_eq!(spec["disabled_by_default"], true);

    let stue = h.entity(&format!("{HOME}-1001"));
    let bad = h.entity(&format!("{HOME}-1002"));
    assert_eq!(h.state(&stue).unwrap().0, "auto");
    assert_eq!(h.state(&bad).unwrap().0, "heat");
    assert_eq!(h.state(&bad).unwrap().1["hvac_action"], "heating");
    assert_eq!(h.state(&h.entity(&format!("{HOME}-1002-window"))).unwrap().0, "on");
    assert_eq!(h.state(&h.entity("09:00:00:00:00:12-battery_state")).unwrap().0, "very_low");
    assert_eq!(h.state(&h.entity("09:00:00:00:00:12-battery_low")).unwrap().0, "on");
    assert_eq!(h.state(&h.entity("04:00:00:00:00:10-boiler")).unwrap().0, "on");
    assert_eq!(h.state(&h.entity("70:ee:50:00:00:20-monitoring")).unwrap().0, "on");
    assert_eq!(h.state(&h.entity("70:ee:50:00:00:21-floodlight")).unwrap().0, "auto");
    assert_eq!(h.state(&h.entity(&format!("{HOME}-person-p-111"))).unwrap().0, "on");
    // the room with only a camera gets no climate entity
    assert!(h.rec.lock().unwrap().entities.keys().all(|k| !k.ends_with("_1003")));
    let (spec, handler) = h.rec.lock().unwrap().entities[&stue].clone();
    assert!(spec.get("name").is_none());
    assert!(handler);

    let call = |entity: String, service: &str, data: Value| json!({ "entry_id": "e1", "entity_id": entity, "service": service, "data": data });
    h.call("entity/call_service", call(stue.clone(), "set_temperature", json!({ "temperature": 22.2 }))).await.unwrap();
    let (state, a) = h.state(&stue).unwrap();
    assert_eq!(state, "heat");
    assert_eq!(a["temperature"], 22.0);
    assert_eq!(a["preset_mode"], "manual");
    h.call("entity/call_service", call(stue.clone(), "set_preset_mode", json!({ "preset_mode": "away" }))).await.unwrap();
    assert_eq!(h.state(&bad).unwrap().1["preset_mode"], "away");
    let cam = h.entity("70:ee:50:00:00:21-monitoring");
    h.call("entity/call_service", call(cam.clone(), "turn_on", json!({}))).await.unwrap();
    assert_eq!(h.state(&cam).unwrap().0, "on");
    let light = h.entity("70:ee:50:00:00:21-floodlight");
    h.call("entity/call_service", call(light.clone(), "select_option", json!({ "option": "on" }))).await.unwrap();
    assert_eq!(h.state(&light).unwrap().0, "on");

    let err = h.call("entity/call_service", call(stue.clone(), "set_hvac_mode", json!({ "hvac_mode": "cool" }))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");
    let err = h.call("entity/call_service", call(light.clone(), "select_option", json!({ "option": "disco" }))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");

    let sent: Vec<(String, String)> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().starts_with("/api/"))
        .map(|r| (r.url.path().to_string(), String::from_utf8_lossy(&r.body).to_string()))
        .collect();
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[0].0, "/api/setroomthermpoint");
    assert!(sent[0].1.contains("room_id=1001") && sent[0].1.contains("mode=manual") && sent[0].1.contains("temp=22"));
    assert_eq!(sent[1].0, "/api/setthermmode");
    assert!(sent[1].1.contains("mode=away"));
    let body: Value = serde_json::from_str(&sent[2].1).unwrap();
    assert_eq!(body, json!({ "home": { "id": HOME, "modules": [{ "id": "70:ee:50:00:00:21", "monitoring": "on" }] } }));
    let body: Value = serde_json::from_str(&sent[3].1).unwrap();
    assert_eq!(body["home"]["modules"][0]["floodlight"], "on");

    let diag = h.call("diagnostics", json!({ "entry": { "entry_id": "e1", "data": { "client_secret": "sec", "refresh_token": "R2" } } })).await.unwrap();
    assert_eq!(diag["entry"]["refresh_token"], "**REDACTED**");
    assert_eq!(diag["entry"]["client_secret"], "**REDACTED**");

    let opts = h.call("options_flow", json!({ "flow": { "flow_id": "o1", "source": "options", "entry": { "entry_id": "e1", "data": {} } }, "step": "init", "input": { "scan_interval": 1 } })).await.unwrap();
    assert_eq!(opts["result"]["data"]["scan_interval"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_token_is_refreshed_and_quota_is_not_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "access_token": "A2", "refresh_token": "R2", "expires_in": 10800 })))
        .mount(&server)
        .await;
    // Netatmo answers an expired token with 403 and code 3
    Mock::given(method("GET"))
        .and(path("/api/homesdata"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "error": { "code": 3, "message": "Access token expired" } })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/homesdata"))
        .and(header("authorization", "Bearer A2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "body": { "homes": [] } })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/getstationsdata"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "error": { "code": 26, "message": "User usage reached" } })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/setroomthermpoint"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": { "code": 21, "message": "Invalid temp" } })))
        .mount(&server)
        .await;
    let urls = Urls { authorize: String::new(), token: format!("{}/oauth2/token", server.uri()), api: server.uri() };
    let old = api::Tokens { access_token: "old".into(), refresh_token: None, expires_in: 10800 };
    let c = Client::new(urls, "cid", "sec", "R1", Some(old));
    assert_eq!(c.homes().await.unwrap(), json!({ "body": { "homes": [] } }));
    assert_eq!(c.take_new_refresh().as_deref(), Some("R2"));
    assert_eq!(c.stations().await.unwrap_err(), ApiError::RateLimited);
    assert!(matches!(c.set_room(HOME, "1001", "manual", Some(99.0)).await.unwrap_err(), ApiError::Refused(m) if m.contains("Invalid temp")));
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_refresh_token_means_login_again() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })))
        .mount(&server)
        .await;
    let h = host::FakeHost::start(Netatmo::default());
    let data = json!({ "client_id": "cid", "client_secret": "sec", "refresh_token": "gone", "token_url": format!("{}/oauth2/token", server.uri()), "api_url": server.uri() });
    let err = h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "netatmo", "data": data } })).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "auth_failed");
}
