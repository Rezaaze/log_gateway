# Projektbewertung — BGP TrustWave / Log Gateway

**Erstellt:** 08.09.2026
**Grundlage:** Code-Review des kompletten Repos + eigene Messungen an echten
RIPE-RIS-Daten (2 Minuten Live-Stream, 1.95 Mio. Announcements; 5 MRT-Archive
vom 01.01.2024, 1.5 Mio. Announcements).
**Frage:** Lohnt es sich, an diesem Projekt weiterzuarbeiten?

---

## Kurzfassung

**Ja — aber nicht an dem Teil, an dem zuletzt gearbeitet wurde.**

Das Repo enthält zwei Projekte in einem:

1. **Ein solides, produktionsreifes BGP-Monitoring-System** (RPKI-Validierung,
   IRR-Abgleich, Hijack-/Flapping-Erkennung, RIS-Live-Ingest, NATS,
   Alert-Routing, Webhooks, Metriken). Das funktioniert, ist getestet und hat
   realen Nutzwert.
2. **Eine Forschungshypothese ("Wellenphysik-Triangulation")**, auf die die
   gesamte Roadmap ab Phase 3 aufbaut und die als Alleinstellungsmerkmal
   gedacht ist. Diese Hypothese habe ich an echten Daten gemessen — sie
   trägt in der aktuellen Form **nicht**.

Die Empfehlung ist also nicht "weitermachen" oder "aufhören", sondern:
**Schwerpunkt umkehren.** Teil 1 zu Ende bringen und live schalten; Teil 2 auf
ein billiges, klar begrenztes Vorab-Experiment zurückstufen, statt Phase 4–7
darauf aufzubauen.

---

## 1. Was solide ist

Verifiziert in dieser Session (Testlauf lokal, `cargo test --workspace --no-fail-fast`):

```
lib          271 passed, 1 failed   (der eine Fehlschlag ist ein Sandbox-Artefakt:
                                     der Test erwartet einen Permission-Fehler beim
                                     Schreiben nach /root, läuft hier aber als root)
hardcore      27 passed
integration   16 passed
weitere       14 passed, 3 ignored
```

Konkret belastbar:

| Komponente | Bewertung |
|---|---|
| `src/rpki_cache.rs` | Echte VRP-Validierung inkl. `max_length`-Semantik, Index aus Routinator `/json`, 10-Min-Refresh. Sauber. |
| `src/irr_cache.rs`, `src/roa_poller.rs` | Ergänzende Signale, TTL-Caching, produktiv nutzbar. |
| `src/anomaly_detector.rs` | Hijack-/Flapping-Erkennung mit RPKI/IRR-Anreicherung; nachvollziehbare Confidence-Stufen. |
| `src/detector_runner.rs` + `escalation.rs` + `webhook.rs` | Durchgehender Pfad von Erkennung bis Slack/Webhook, seit dem Escalation-Fix vom 31.08. tatsächlich verdrahtet. |
| `tools/bgp_stream/` | RIS-Live-Ingest mit Batching, robuste TLS-Konfiguration. |
| Log-Gateway-Kern (HTTP, PII, Cache, Rate-Limit, S3, TLS, Metriken) | Vollständig, getestet, gebenchmarkt. Gehört inhaltlich aber nicht mehr zum BGP-Produkt. |
| Betrieb | Docker-Compose-Cluster, Prometheus/Grafana/Alertmanager, CI mit 4 Jobs, Deploy-Skript mit Rollback. Überdurchschnittlich für ein Ein-Personen-Projekt. |

Die Code-Qualität ist durchweg ordentlich: sinnvolle Modulgrenzen, echte
Unit-Tests statt Attrappen, ehrliche Kommentare an den schwachen Stellen. Die
Dokumentation (`CLAUDE.md`, `TRUSTWAVE_ROADMAP.md`) protokolliert auch
unangenehme Funde, statt sie zu glätten — das ist selten und wertvoll.

---

## 2. Die Wellenphysik-Hypothese — gemessen, nicht geschätzt

Die Kernidee laut Roadmap: Dieselbe BGP-Ankündigung erreicht die RIS-Kollektoren
zu physikalisch vorhersagbaren Zeiten (Frankfurt +0 ms, Amsterdam +89 ms,
New York +891 ms). Ein Hijack bricht dieses Muster, weil alles gleichzeitig
ankommt.

Ich habe das an echten Daten nachgemessen. Ergebnisse:

### 2.1 Die gemessenen Abstände sind kein Lichtlaufzeit-Effekt

2 Minuten RIS Live, gruppiert nach `(prefix, origin_as, voller AS-Pfad)` im
10-s-Fenster, korrekt nach Kollektor getrennt:

```
Events mit >= 3 Kollektoren:  44.847
spread_ms   p10 = 90    p50 = 1.000    p90 = 5.090    p99 = 8.970
Events mit spread < 5 ms:  6 von 44.847  (0,013 %)
```

Der Median liegt bei **einer Sekunde**, nicht bei ~100 ms. Interkontinentale
Lichtlaufzeit über Glasfaser liegt bei maximal ~100 ms (20.000 km / 200.000 km/s).
Was hier gemessen wird, ist also zu >90 % **BGP-Verarbeitung, MRAI-Timer und
Router-Queueing** — nicht Ausbreitungsphysik. Das physikalische Signal, das
das Projekt sucht, liegt unter dem Rauschen der Protokoll-Timer.

Zusätzlich: `p99 = 8.970 ms` bei einem 10-s-Fenster heißt, dass das Fenster die
Verteilung abschneidet. Ein relevanter Teil der Propagation dauert länger als
das Aggregationsfenster und wird verworfen.

### 2.2 Nur ~3,5 % der Ankündigungen sind überhaupt messbar

```
Gruppen gesamt:              1.284.772
davon 1 Kollektor:           1.124.850  (87,6 %)
davon >= 3 Kollektoren:         44.847  ( 3,5 %)
```

Die Bedingung "gleiches Präfix, gleiches Origin, **gleicher AS-Pfad**, bei
mindestens 3 Kollektoren im selben Fenster" trifft auf 3,5 % der Ankündigungen
zu. Ohne die Pfad-Bedingung sind es 10 %, dann wird der Spread aber noch
größer (p50 = 4.990 ms), weil unterschiedliche Pfade unterschiedlich lange
brauchen. Beides ist zu wenig, um daraus ein Erkennungsprodukt zu bauen: das
Signal fehlt für 90–96 % des Traffics.

### 2.3 Die Archivdaten haben keine Millisekunden

Die Baseline (Phase 2) und das Backtesting (Phase 3.2/3.3 — das
Entscheidungs-Gate vor Phase 4) beruhen auf MRT-Archiven von
`data.ris.ripe.net`. Gemessen über 1.505.894 Announce-Records aus 5 Kollektoren:

```
Records mit Sub-Sekunden-Anteil im Zeitstempel:  0  (0,00 %)
```

Die Archive haben **Sekunden-Auflösung**. Alle Spread-Werte daraus sind
Vielfache von 1000 ms. Eine aus Archivdaten gebaute Baseline kann die
Millisekunden-Statistik, gegen die der Live-Pfad prüft, prinzipiell nicht
enthalten — die Einheiten passen nicht zusammen. Das Backtesting-Framework
kann in dieser Konstruktion das Entscheidungs-Gate ("Trefferquote ≥ 80 %,
FPR ≤ 1 %") nicht valide beantworten, egal wie viele Fallstudien noch
eingetragen werden.

*(Der Live-Pfad über RIS Live hat dagegen echte Millisekunden — verifiziert:
`"timestamp":1788880690.730`. Das Problem ist die Kombination aus
Sekunden-Baseline und Millisekunden-Messung.)*

### 2.4 Zwei der fünf Signale sind in der Praxis konstant

Gemessen an denselben 44.847 Live-Events:

| Signal | Gewicht | Gemessenes Verhalten |
|---|---|---|
| `collector_gap_ratio` | 0,15 | Mittelwert **0,93**, bei **73,9 %** der Events exakt 1,0. Die 4-ms-Schwelle liegt weit unter dem typischen Abstand (p50 des Gesamtspreads = 1000 ms), also überschreitet praktisch jede Lücke sie. |
| `propagation_speed` | 0,05 | Bei **89,4 %** der Events auf 1,0 geclampt (jeder Spread ≥ 100 ms überschreitet die maximal mögliche Lichtlaufzeit zwischen zwei beliebigen Kollektoren). |

Beide liefern damit keinen Unterschied zwischen Ereignissen, sondern einen
konstanten Sockel von ~0,19 auf jedem Score. Das ist derselbe Fehlertyp, der
am 01.09. bereits bei `calculate_order_entropy` gefunden wurde (Signal war
konstant 1,0) — er tritt hier nur bei zwei weiteren Signalen auf und ist
diesmal nicht aus dem Code allein sichtbar, sondern nur an echten Daten.

Zusätzlich ist `propagation_speed` gegenüber seiner eigenen Dokumentation
**invertiert**: der Doc-Kommentar beschreibt "Spread deutlich *kleiner* als
die Lichtlaufzeit = Hijack-Signatur", der Code (`spread_ms / light_time_ms`)
schlägt aber bei *großem* Spread aus und geht bei der beschriebenen
Hijack-Signatur gegen 0.

### 2.5 Baselines sind für stabile Präfixe kaum erreichbar

`BaselineBuilder` verlangt ≥ 30 Beobachtungen je `(prefix, origin, path)`-Gruppe.
Gemessen über 2 Minuten Live-Stream (982.260 verschiedene Schlüssel):

```
Schlüssel mit >= 30 Announcements in 2 Min:  2.721  (0,28 %)
Schlüssel mit >=  2 Announcements in 2 Min:  31,5 %
```

Das ist ein struktureller Selektionseffekt, kein Datenmengen-Problem: ein
*stabiles* Präfix kündigt sich per Definition selten an. Genau für die Präfixe,
die ein Kunde schützen lassen will, entsteht also am langsamsten eine Baseline
— während flappende, unruhige Präfixe schnell eine bekommen. Mehr Archivdaten
herunterladen löst das nicht.

---

## 3. Konkrete Bugs (unabhängig von der Hypothese)

Diese vier stehen für sich und sind auch dann relevant, wenn die Wellenphysik
verworfen wird:

### 3.1 Das Kollektor-Feld wird falsch ausgelesen — kritisch

`tools/bgp_stream/src/main.rs:470` liest den Kollektor aus `data.id`:

```rust
let collector = data.id.as_deref().unwrap_or("unknown");   // Zeile 470
id: Option<String>,       // collector name, e.g. "rrc12"   // Zeile 164
```

Im echten RIS-Live-Stream ist `id` aber eine **pro Nachricht eindeutige** ID:

```json
{"id":"80.81.196.197-01a0819950b60000","host":"rrc12.ripe.net", ...}
```

Nachgemessen an 20.000 Live-Nachrichten: 20.000 verschiedene `id`-Werte, während
`host` korrekt `rrc25.ripe.net`, `rrc20.ripe.net`, … enthält. Der Kollektorname
steht in `host`, nicht in `id`.

Folge in Produktion: jedes BGP-Record bekommt einen eigenen, einmaligen
"Kollektor"-String. Damit ist
- `collector_registry::lookup()` immer erfolglos → keine Geodistanz → `propagation_speed` = 0,
- die `arrivals`-Map des `PropagationAggregator` nach Nachrichten-IDs statt nach
  Kollektoren geschlüsselt → der gemessene "Spread" ist keine
  Kollektor-Triangulation,
- `expected_order` in der Baseline über Einmal-Schlüssel gebildet.

Die gesamte Wellenphysik-Kette läuft im Live-Betrieb also auf einem Eingabefeld,
das nie den Kollektor enthielt. Fix ist einzeilig (`data.host`, `.ripe.net`
abschneiden) — aber alles, was bisher live daraus abgeleitet wurde, ist
ungültig.

### 3.2 Baseline-Schlüssel passt nicht zum Live-Schlüssel

`tools/baseline_builder/src/main.rs:148` (und identisch `tools/backtest`):

```rust
let as_path = vec![origin_as];   // Vereinfachung für Batch-Verarbeitung
```

`BaselineBuilder::add()` hasht anschließend `event.as_path` — also `[origin_as]`,
Hash = `origin_as`. Der Live-Pfad berechnet in `wave_anomaly_detector::score_event()`
dagegen `calculate_path_hash(&event.as_path)` über den **vollständigen** AS-Pfad.

Folge: `find_entry()` findet für Live-Events praktisch nie einen Baseline-Eintrag
(nur bei Ein-Hop-Pfaden). Der Wave-Detector fällt still auf
`AnomalyScore::default()` / `Normal` zurück — er ist im Live-Betrieb faktisch
abgeschaltet, ohne dass irgendetwas Alarm schlägt. Zusätzlich kollabieren im
Builder alle verschiedenen AS-Pfade eines Präfixes in **einen** Baseline-Eintrag,
was die Gruppierungsabsicht (Statistik je Pfad) aufhebt.

### 3.3 Baseline-Builder und Backtest haben kein Zeitfenster

`build_events()` in beiden Tools gruppiert über den **gesamten** Datensatz nach
`(prefix, origin, path_hash)` und nimmt je Kollektor den frühesten Zeitstempel.
Es gibt keine Zeit-Bucketierung. Gemessen an den Archivdaten ergeben sich dabei
Spread-Werte bis **293.000 ms** — also Ankündigungen, die fünf Minuten
auseinanderliegen, werden zu einem "Propagationsereignis" verschmolzen. Die
daraus berechneten `p50`/`p99`/`std_dev` beschreiben nichts Physikalisches.
(Der Live-Aggregator macht es mit seinem 10-s-Fenster richtig — die beiden
Pfade sind also auch hier inkonsistent.)

### 3.4 Build hängt an einem GitHub-Download

`utoipa-swagger-ui` lädt zur Build-Zeit ein ZIP von `github.com`. In jeder
Umgebung ohne diesen Zugriff schlägt `cargo build` fehl — nicht nur in der
Sandbox, sondern auch bei jedem CI-Runner oder Kunden-Build hinter einer
restriktiven Policy. Das ist kein Sandbox-Artefakt, sondern eine echte
Lieferketten-Abhängigkeit. Lösung: `vendored`-Feature der Crate nutzen oder
Swagger-UI ganz herausnehmen (die BGP-Komponente braucht sie nicht).

---

## 4. Einordnung: Markt

Der funktionierende Teil (RPKI/IRR/Realtime-Alerting) ist gut gebaut, aber
nicht neu. Es gibt kostenlose und etablierte Alternativen — u. a. BGPalerter
(Open Source), Qrator.Radar, Cisco/ThousandEyes, Cloudflare Radar, bgp.tools,
RIPE RIS eigene Alerts. Das eigentliche Differenzierungsversprechen war die
Wellenphysik. Genau die trägt nach den Messungen oben nicht in ihrer aktuellen
Form.

Das heißt nicht, dass kein Produkt möglich ist — aber die Differenzierung müsste
woanders herkommen (Latenz bis zum Alert, Integrationstiefe, Multi-Tenant-API,
Betriebsqualität), und das sind Wettbewerbsvorteile im Ausführungs-, nicht im
Forschungsbereich.

---

## 5. Empfehlung

### Weitermachen — mit dieser Reihenfolge

**Sofort (Tage):**
1. Bug 3.1 fixen (`data.host` statt `data.id`) — einzeilig, aber Voraussetzung
   für jede weitere Aussage über Kollektor-Daten.
2. Bug 3.4 fixen (`vendored` Swagger-UI oder entfernen) — sonst ist der Build
   nicht reproduzierbar.
3. Bug 3.2/3.3 entweder fixen oder den Wave-Pfad **explizit deaktivieren**,
   solange er nicht validiert ist. Ein still auf `Normal` zurückfallender
   Detektor ist schlimmer als ein abgeschalteter.

**Danach (Wochen) — der eigentliche Wert:**
4. Den RPKI/IRR/Hijack-Pfad end-to-end live nehmen und 30 Tage messen: Wie
   viele Alerts pro Tag? Wie viele davon falsch? Wie schnell nach dem echten
   BGP-Event? Das sind die Zahlen, mit denen man mit einem ersten Nutzer reden
   kann — und sie erfordern keine neue Forschung.
5. Log-Gateway-Teil in ein eigenes Repo trennen (steht schon als Option A in
   `PRODUCT_ROADMAP.md` 1.3). Er verwässert das Produkt und verlängert jeden
   Build.

**Wellenphysik (Phasen 3.2–7): stoppen und durch ein 2-Tage-Experiment ersetzen**

Nicht endgültig verwerfen — aber vor jeder weiteren Investition beantworten:
> Nimm einen einzigen, gut sichtbaren Präfix mit vielen Kollektoren. Sammle
> 7 Tage Live-Daten (Millisekunden, korrekte Kollektor-IDs). Frage: ist der
> Spread desselben Präfixes über die Zeit reproduzierbar genug, dass eine
> Abweichung überhaupt auffallen könnte — oder dominieren MRAI und
> Router-Queueing komplett?

Kostet ein Wochenende und beantwortet die Frage, an der die Phasen 3.3 bis 7
hängen. Nach den Zahlen oben (p50 = 1000 ms, 74 % der Events mit saturiertem
Gap-Signal) ist mein Erwartungswert: das Rauschen dominiert. Aber das ist eine
Prognose, keine Messung — und diese eine Messung ist billig genug, um sie zu
machen, statt zu spekulieren.

**Was in jedem Fall nicht mehr gemacht werden sollte:** Wochen an MRT-Archiven
herunterladen, um die drei historischen Fallstudien zu befüllen (Abschnitt 3.2.1/
3.2.2). Die Archive haben keine Millisekunden — das Ergebnis dieses Downloads
kann die Frage nicht beantworten, egal wie viel davon heruntergeladen wird.

---

## Anhang — Reproduktion der Messungen

```bash
# Archiv-Auflösung (Sub-Sekunden-Anteil):
curl -O https://data.ris.ripe.net/rrc00/2024.01/updates.20240101.0800.gz
# Zeitstempel je Record via bgpkit-parser prüfen -> fract() == 0.0 für alle

# Live-Spread und Signal-Sättigung:
curl -sN "https://ris-live.ripe.net/v1/stream/?format=json&client=eval" > live.ndjson
# gruppieren nach (prefix, origin, path, floor(ts/10)), Kollektor aus data.host

# Testlauf (mit lokal bereitgestelltem Swagger-UI-ZIP wegen Bug 3.4):
SWAGGER_UI_DOWNLOAD_URL="file:///pfad/swaggerui.zip" cargo test --workspace --no-fail-fast
```

Alle Zahlen in diesem Dokument stammen aus Läufen vom 08.09.2026 gegen die
jeweils genannten Datenquellen.

---

## Nachtrag 08.09.2026 — Bugs 3.1–3.4 behoben

Alle vier in Abschnitt 3 beschriebenen Bugs sind gefixt. Stand danach:
**338 Tests grün** (vorher 328; +10 neue Regressionstests), `cargo clippy
--workspace --all-targets` und `cargo fmt --check` sauber.

| Bug | Fix | Regressionstest |
|---|---|---|
| 3.1 Kollektor aus `data.id` | `RisData.host` statt `id`, neuer Helper `collector_from_host()` schneidet `.ripe.net` ab | 3 Tests in `tools/bgp_stream`, u. a. gegen ein wörtlich aus dem Live-Stream übernommenes RIS-Frame |
| 3.2 Baseline-Schlüssel-Mismatch | Ein einziger `propagation::path_hash()` für alle drei bisherigen Kopien; Batch-Builder trägt den **vollen** AS-Pfad ins Event statt `vec![origin_as]` | `test_archive_built_baseline_is_found_by_live_lookup` (Archiv → Builder → save/load → Live-Lookup), `test_batch_and_live_group_keys_agree` |
| 3.3 Kein Zeitfenster im Batch | Gemeinsame `propagation::build_events_batch()` mit Sessionisierung (Fenster ab erster Ankunft, wie im Live-Aggregator), `--window-secs` in beiden Tools, Default 10 s | `test_build_events_batch_splits_on_window`, `_preserves_full_as_path`, `_separates_distinct_paths`, `_drops_below_min_collectors` |
| 3.4 Build lädt von github.com | `utoipa-swagger-ui` mit `vendored`-Feature; zusätzlich `target-cpu=native` aus `.cargo/config.toml` entfernt (siehe unten) | Build läuft in dieser Umgebung ohne Workaround durch |

**Zusätzlich gefunden beim Fixen von 3.4:** `.cargo/config.toml` setzte
`-C target-cpu=native` unbedingt. In dieser VM meldet CPUID AVX-512, der
Hypervisor stellt es aber nicht bereit — der Build bricht mit **SIGILL** im
Build-Script einer Dependency ab. Dasselbe Muster trifft den Release-Pfad: der
GitHub-Actions-Runner backt seine CPU-Features in ein Binary, das anschließend
auf einem anderen Host läuft. Das Flag ist jetzt opt-in
(`RUSTFLAGS="-C target-cpu=native" cargo build --release` auf einem Build-Host,
auf dem Build- und Laufzeit-Maschine identisch sind).

**Zusätzlich eingebaut:** `WaveAnomalyDetector` zählt jetzt Baseline-Treffer
und -Fehlschläge (`baseline_coverage()`), und `DetectorRunner` loggt einmalig
einen Fehler, wenn eine geladene Baseline nach 1000 Events kein einziges Mal
gematcht hat. Genau dieses stille Zurückfallen auf `Normal` hatte Bug 3.2
monatelang unsichtbar gemacht.

### Gegen echte Daten verifiziert

`tools/baseline_builder` gegen dieselben RIS-Archivdaten wie oben
(5 Kollektoren, 01.01.2024, 14,4 MB, 5897 Events):

```
vorher:  max_spread_ms bis 293.000 (Ankündigungen 5 Minuten auseinander verschmolzen)
nachher: max_spread_ms p50 = 8.000, max = 10.000, Einträge über dem 10-s-Fenster: 0
```

### Was die Fixes nicht ändern

Die Messergebnisse aus Abschnitt 2 bleiben unverändert gültig. Die Bugfixes
machen die Pipeline **korrekt**, nicht die Hypothese **richtig**:

- Die Archive haben weiterhin Sekunden-Auflösung — der neue Baseline-Lauf zeigt
  das direkt (p50 der Spreads = 8000 ms, Vielfache von 1000).
- Der Anteil messbarer Ankündigungen (~3,5 %) und die Sättigung von
  `collector_gap_ratio` und `propagation_speed` sind unberührt.

Die Empfehlung aus Abschnitt 5 gilt also unverändert: erst den RPKI/IRR-Pfad
live messen, und die Wellenphysik über das beschriebene 7-Tage-Experiment
entscheiden — das jetzt überhaupt erst aussagekräftig durchführbar ist, weil
der Kollektor-Bug (3.1) vorher jede Live-Messung wertlos gemacht hätte.
