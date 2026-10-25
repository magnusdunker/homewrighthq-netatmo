# Netatmo til HomewrightHQ

En ekstern integration til [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Netatmos vejrstation, termostat og radiatorventiler og de indendørs og udendørs kameraer, gennem Netatmos API.

## Installation

I HomewrightHQ: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/homewrighthq-netatmo
```

og tryk **Installér**.

Netatmo kræver, at du opretter din egen app:

1. Log ind på [dev.netatmo.com](https://dev.netatmo.com) med din Netatmo-konto, den samme som i Netatmo-appen.
2. Opret en app under **My apps**, og skriv en redirect URI, f.eks. din HomewrightHQ-adresse med `/auth/external/callback` efter: `http://localhost:8123/auth/external/callback`.
3. Gem appens client ID og client secret.

Tilføj derefter **Netatmo** under **Tilføj integration**, og skriv client ID, client secret og den samme redirect URI. Næste trin viser en adresse til Netatmos login. Når du har logget ind og givet adgang, sender Netatmo browseren til redirect-adressen. Den side kan vise en fejl; det gør ikke noget. Kopiér hele adressen fra browserens adresselinje (den indeholder `code=...`) og indsæt den i formularen.

Det sidste trin er nødvendigt, fordi en ekstern integration i HomewrightHQ ikke kan tage imod Netatmos svar selv.

Integrationen beder om adgang til vejrstation, termostater og kameraer på én gang. Har du kun noget af det, er resten bare tomt.

## Hvad du får

Vejrstationen bliver en enhed for basen og en for hvert modul, med:

- basen: temperatur, luftfugtighed, CO2, støj og lufttryk
- udendørsmodul: temperatur og luftfugtighed
- ekstra indendørsmodul: temperatur, luftfugtighed og CO2
- regnmåler: regn i den seneste måling, sidste time og i dag
- vindmåler: vind og vindstød i km/t og deres retning i grader
- batteri i procent på modulerne og signalstyrke (slået fra som standard)

Mister et modul forbindelsen, bliver dets sensorer utilgængelige.

Hvert rum med termostat eller radiatorventil bliver en enhed med:

- et klimaanlæg med tilstandene `auto` (følg ugeplanen), `heat` (manuel temperatur) og `off`, og forindstillingerne `schedule`, `manual`, `boost` (maks), `away` og `frost_guard`. Om rummet varmer (`hvac_action`), kommer fra rummets varmebehov, eller fra kedlens status, hvis Netatmo ikke oplyser det.
- en sensor for åbent vindue

`away` og `frost_guard` gælder hele hjemmet, sådan som Netatmo gør det, og det gør vejen tilbage til `schedule` fra dem også. En manuel temperatur varer så længe, som er sat i Netatmo-appen (normalt tre timer).

Termostater og radiatorventiler får en batterisensor (`full`, `high`, `medium`, `low`, `very_low`) og en sensor for lavt batteri. Termostaten får en sensor for, om kedlen er tændt.

Kameraer (Smart Indoor og Outdoor Camera) får en kontakt for overvågning, og udendørskameraet et valg for projektøren (`auto`, `on`, `off`). Kendte personer, som kameraet har lært at genkende, bliver hver en tilstedeværelsessensor på hjemmet.

Der er ingen video, stillbilleder eller hændelser fra kameraerne. SDK'en til eksterne integrationer har ingen kamera-entity, og Netatmos hændelser kommer som webhooks, som en ekstern integration ikke kan tage imod.

## Opdatering og Netatmos grænser

Integrationen læser vejrstationerne og status for hvert hjem hvert femte minut, altså én forespørgsel plus én pr. hjem. Vejrstationen sender selv nye målinger hvert tiende minut. Under **Indstillinger** på integrationen kan tiden sættes fra 2 til 60 minutter. Siger Netatmo, at grænsen for forespørgsler er nået, holder integrationen pause i 30 minutter.

Efter en kommando opdaterer integrationen tilstanden selv, og den næste læsning henter det, Netatmo faktisk har sat.

Adgangstokenet fornyes hver tredje time. Netatmo giver et nyt refresh token ved fornyelsen, og integrationen gemmer det på opsætningen. Afviser Netatmo det, viser HomewrightHQ det under problemer, og integrationen kan godkendes igen.

Rum, moduler og personer læses, når integrationen starter. Et nyt modul eller rum kommer med, når integrationen genindlæses.

## Ikke afprøvet mod det rigtige API

Integrationen er skrevet ud fra Netatmos dokumentation af API'et og den måde, Home Assistants Netatmo-integration bruger det på, og afprøvet mod en falsk Netatmo med svar formet som de rigtige. Disse ting er ikke kontrolleret:

- Om Netatmo godtager en `http://`-adresse eller en lokal adresse som redirect URI. Kræver den https, så brug en https-adresse: koden står i adresselinjen, også når siden ikke kan åbnes.
- Fejlkoderne i Netatmos svar: 2 og 3 for et ugyldigt eller udløbet adgangstoken, 26 for brugt kvote, og om de kommer med HTTP 403.
- Netatmos grænser på 50 forespørgsler pr. 10 sekunder og 500 i timen pr. bruger.
- `setroomthermpoint` med `mode=off` for at slukke et rum, og at en manuel temperatur uden `endtime` varer den tid, der er sat i appen.
- `setstate` med `monitoring` og `floodlight` på kameraerne, og at nye indendørskameraer (`NPC`) virker som de gamle.
- At `wifi_status` og `rf_status` kan læses som dBm uden fortegn.
- At `getstationsdata` altid giver metriske enheder (°C, mbar, mm, km/t), også når kontoen viser andre enheder i appen.
- At `boiler_status` findes på OpenTherm-termostaten (`OTM`), og at Smarther (`BNS`) styres som de andre.
- Felterne `battery_state`, `open_window`, `heating_power_request` og `out_of_sight` på rigtige anlæg.

Røgalarmer, CO-alarmer, dørklokke, sirener og Legrand/BTicino Home + Control er ikke med. Skift mellem ugeplaner er heller ikke med.

## Udvikling

Bygget i Rust på SDK'en `thehome-plugin` fra [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: OAuth og Netatmos API
- `src/model.rs`: Netatmos data og hvordan de bliver til tilstande og kommandoer
- `src/lib.rs`: forbindelsen til HomewrightHQ
- `tests/netatmo.rs`: tests mod en falsk Netatmo med svarene i `tests/fixtures`, og en falsk HomewrightHQ i `tests/host`

```
cargo test
cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.1.0 && git push origin v0.1.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne og `homewright.json` på releasen.

## Licens

MIT
