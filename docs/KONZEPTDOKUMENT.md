# BGP TrustWave — Technisches Konzeptdokument
## Zur notariellen Beglaubigung der Urheberschaft

---

**Erstellt von:** Alireza Shahsavar Khani
**Datum der Erstellung:** 08.03.2026
**Zweck dieses Dokuments:** Datierung und Dokumentation der geistigen Urheberschaft
an der nachfolgend beschriebenen technischen Erfindung.

---

## 1. Kurzbezeichnung der Erfindung

**BGP TrustWave System** — Ein Verfahren zur Erkennung und Abwehr von
BGP-Hijacking-Angriffen durch Analyse des Ausbreitungsmusters von
BGP-Ankündigungen über geografisch verteilte Messpunkte (Wellenphysik-Triangulation)
in Kombination mit einem kontinuierlichen Trust-Score-Modell pro Route.

---

## 2. Technischer Hintergrund

### 2.1 Das Problem: BGP-Hijacking

Das Border Gateway Protocol (BGP) ist das Routing-Protokoll des Internets.
Es regelt, welche Netzwerkpfade (Präfixe) über welche autonomen Systeme (ASes)
erreichbar sind. BGP hat keinen eingebauten Mechanismus zur Authentizitätsprüfung:
Jedes AS kann theoretisch beliebige IP-Präfixe ankündigen — auch solche, für die
es nicht legitimiert ist. Diesen Angriff nennt man BGP-Hijacking.

Bekannte historische Angriffe:
- **Pakistan Telecom (24.02.2008):** YouTube weltweit für ~2 Stunden unerreichbar
- **MyEtherWallet (24.04.2018):** Krypto-Wallet-Nutzer auf Phishing-Server umgeleitet,
  direkter finanzieller Schaden durch gestohlene Kryptowährungen
- **Rostelecom (01.04.2020):** Traffic von ~200 CDN/Cloud-Anbietern umgeleitet

Bestehende Gegenmaßnahmen (RPKI, IRR) sind notwendig aber nicht hinreichend:
- RPKI-Abdeckung ist unvollständig (ca. 50% aller Präfixe)
- IRR-Daten sind häufig veraltet und unzuverlässig
- Beide Systeme erkennen Angriffe reaktiv oder gar nicht

### 2.2 Bestehende Ansätze und ihre Grenzen

| System | Ansatz | Schwäche |
|---|---|---|
| RPKI | Kryptografische Signierung von Route-Origin-Autorisierungen | Nur ~50% Abdeckung, kein Schutz gegen AS-Path-Manipulation |
| IRR | Datenbank legitimer Routen-Ankündigungen | Veraltete Daten, keine Echtzeit-Validierung |
| BGPmon | Regelbasierte Anomalie-Erkennung | 5–15 Minuten Erkennungslatenz, viele False Positives |
| RIPE Stat | Manuelle Analyse-Tools | Keine automatische Echtzeit-Erkennung |

---

## 3. Die Erfindung: Wellenphysik-Triangulation

### 3.1 Kernidee

Eine legitime BGP-Ankündigung breitet sich physikalisch vorhersehbar durch das
Internet aus — analog zur Ausbreitung einer Welle durch ein Medium. Die
Ankunftszeiten derselben Ankündigung an geografisch verteilten Messpunkten
folgen einem charakteristischen Muster, das von der Netzwerktopologie und den
physikalischen Übertragungszeiten abhängt.

**Ein BGP-Hijacking-Angriff bricht dieses Muster zwangsläufig**, weil:
1. Der Angreifer die Ankündigung von einem anderen geografischen Ursprung sendet
2. Die gefälschte Ankündigung sich daher anders ausbreitet als die legitime
3. Die Zeitdifferenzen zwischen den Messpunkten statistisch signifikant vom
   historischen Erwartungswert abweichen

### 3.2 Datengrundlage: RIPE RIS Route Collectors

Das RIPE Network Coordination Centre betreibt ~26 Route Collector Server (RRCs)
an strategisch verteilten Internet Exchange Points (IXPs) weltweit:

| Kollektor | Standort | IXP |
|---|---|---|
| rrc00 | Amsterdam, NL | AMS-IX |
| rrc06 | Otemachi, JP | DIX-IE |
| rrc11 | New York, US | NYIIX |
| rrc12 | Frankfurt, DE | DE-CIX |
| rrc17 | Singapore, SG | Equinix SG |
| rrc21 | Paris, FR | France-IX |
| ... | (insgesamt 26) | |

Jeder Kollektor empfängt alle BGP-Updates von seinen Peers und speichert sie
mit Millisekunden-genauem Zeitstempel. Diese Daten sind öffentlich zugänglich
über das RIPE RIS Live WebSocket API sowie als historische MRT-Archivdaten.

### 3.3 Das Wellenmodell

Für eine bekannte, legitime Route `P` mit Ursprungs-AS `A` wird beobachtet:

```
Kollektor rrc12 (Frankfurt) empfängt Update zum Zeitpunkt t₀
Kollektor rrc00 (Amsterdam) empfängt Update zum Zeitpunkt t₀ + Δt₁
Kollektor rrc11 (New York)  empfängt Update zum Zeitpunkt t₀ + Δt₂
Kollektor rrc17 (Singapore) empfängt Update zum Zeitpunkt t₀ + Δt₃
```

Aus historischen Beobachtungen (Baseline) ist bekannt:
- Welche Kollektoren typischerweise zuerst empfangen (geografische Nähe zum Origin)
- Welche Zeitdifferenzen Δt zwischen den Kollektoren zu erwarten sind
- Mit welcher statistischen Streuung (Standardabweichung)

### 3.4 Anomalie-Erkennung durch Wellenabweichung

Ein neues BGP-Update für Route `P` wird als **anomal** eingestuft wenn
mindestens eines der folgenden Muster vorliegt:

**Signal 1 — Spread-Anomalie:**
Alle Kollektoren empfangen das Update nahezu gleichzeitig (Spread < 10% des
Baseline-Erwartungswerts). Ein Angreifer, der eine Route von einem anderen
Netzwerkpunkt injiziert, erreicht alle Punkte gleichzeitig anstatt in der
erwarteten geografischen Reihenfolge.

**Signal 2 — Reihenfolge-Anomalie:**
Die ersten drei empfangenden Kollektoren stimmen nicht mit der historischen
Baseline überein. Dies deutet auf einen anderen geografischen Ursprung hin.

**Signal 3 — Paarweise Delta-Anomalie:**
Die Zeitdifferenz zwischen zwei spezifischen Kollektoren weicht mehr als 3
Standardabweichungen vom historischen Mittelwert ab.

**Signal 4 — AS-Pfad-Verkürzung:**
Der AS-Pfad in der Ankündigung ist kürzer als der historische Erwartungswert
für diese Route. Ein Angreifer, der sich "zwischen" den legitimen AS und die
Kollektoren stellt, erzeugt zwangsläufig einen kürzeren Pfad.

**Signal 5 — Region-Inversion:**
Das Update wird zuerst von Kollektoren in einer geografischen Region empfangen,
die vom historischen Ursprungsbereich dieser Route weit entfernt ist.

### 3.5 Wave Score Formel

```
W = w₁ × spread_signal
  + w₂ × order_signal
  + w₃ × delta_signal
  + w₄ × path_signal
  + w₅ × region_signal

W ∈ [0.0, 1.0]
0.0 = vollständig normales Ausbreitungsmuster
1.0 = maximale Abweichung vom Erwartungswert

Standardgewichte:
  w₁ = 0.30  (Spread — stärkstes Signal)
  w₂ = 0.25  (Reihenfolge)
  w₃ = 0.25  (Paarweise Deltas)
  w₄ = 0.10  (AS-Pfad-Länge)
  w₅ = 0.10  (Region)
```

---

## 4. Trust Score System

### 4.1 Multi-Signal Trust Score

Der Wave Score ist ein Signal unter mehreren. Das vollständige Trust-Score-System
kombiniert:

```
T = α × rpki_signal
  + β × irr_signal
  + γ × (1.0 − wave_score)
  + δ × stability_signal
  − ε × flapping_rate

T ∈ [0.0, 1.0]
1.0 = vollständig vertrauenswürdig
0.0 = nicht vertrauenswürdig
```

| Signal | Quelle | Gewicht |
|---|---|---|
| RPKI-Validierung | Routinator / RIPE RPKI | α = 0.30 |
| IRR-Konsistenz | Internet Routing Registry | β = 0.20 |
| Wellenabweichung (invertiert) | Eigene Berechnung | γ = 0.40 |
| Historische Stabilität | Eigene Beobachtungshistorie | δ = 0.10 |
| Flapping-Malus | Eigene Berechnung | ε = variabel |

### 4.2 Score-Degradierung

Der Trust Score eines Präfixes degradiert während einer anhaltenden Anomalie:

```
T(t) = T₀ × 0.95^(minutes_anomalous)
```

Er erholt sich langsam wenn keine Anomalien mehr auftreten:

```
T(t) = T_current + 0.01 × minutes_normal
```

### 4.3 Lösungsraum-Einschränkung

Routen mit Trust Score unterhalb konfigurierbarer Schwellenwerte werden
in Quarantäne verschoben oder abgelehnt:

```
T > 0.70  → Route wird akzeptiert und weitergeleitet
T ∈ [0.40, 0.70] → Route in Quarantäne (Alert, kein Forwarding)
T < 0.40  → Route wird abgelehnt
```

---

## 5. Gruppen-Korrelation

Zusätzlich zum per-Route-Scoring wird analysiert, ob mehrere Präfixe sich
gleichzeitig bewegen. Bei BGP-Hijacking-Angriffen auf Krypto-Börsen werden
häufig mehrere verwandte Präfixe gleichzeitig übernommen.

**Methodik:** Präfixe die innerhalb eines 60-Sekunden-Fensters gleichzeitig
Trust-Score-Degradierungen erfahren und eine kleine gemeinsame Schnittmenge
im AS-Pfad haben, werden als Gruppe behandelt. Das Gruppen-Signal senkt den
Trust Score aller betroffenen Mitglieder zusätzlich.

---

## 6. Neuheit und Unterschied zum Stand der Technik

Das beschriebene System ist neu in der **Kombination** folgender Elemente:

1. **Wellenphysik-Modell für BGP-Propagation:** Die Modellierung von
   BGP-Ausbreitung als physikalische Welle durch ein Netzwerk-Medium und die
   Nutzung von Ankunftszeit-Mustern an verteilten Kollektoren als
   Authentizitätsmerkmal ist nach Kenntnis des Erfinders so nicht beschrieben.

2. **Triangulations-Ansatz ohne eigene Infrastruktur:** Die Nutzung des
   bestehenden RIPE RIS Kollektor-Netzwerks als Triangulationssystem ohne
   eigene Hardware an IXPs zu betreiben ist ein wesentlicher wirtschaftlicher
   Vorteil und technisch neu.

3. **Kontinuierlicher Trust Score mit Degradierung:** Die Kombination eines
   wellenphysik-basierten Signals mit RPKI, IRR und historischer Stabilität
   in einem einzigen kontinuierlichen Score pro Route mit zeitbasierter
   Degradierung ist als integriertes System neu.

4. **Lösungsraum-Einschränkung als BGP-Filter:** Die direkte Umsetzung des
   Trust Scores in BGP-Router-Policy (welche Routen werden an den BGP-Speaker
   weitergeleitet) ist eine neue Anwendung des Verfahrens.

---

## 7. Technische Umsetzung (Stand 08.03.2026)

### Bereits implementiert (log-gateway Codebase):
- BGP-Event-Ingestion via RIPE RIS Live WebSocket (Rust, tokio-tungstenite)
- RPKI-Validierung via Routinator-Integration (~825k VRPs)
- IRR-Cache-Integration
- FlappingDetector (Stabilitätssignal)
- NATS JetStream Event-Pipeline (~27.500 Events/Sekunde)
- Alert-System mit Webhook-Dispatch
- 295+ automatisierte Tests

### Noch zu implementieren (Roadmap Phase 1–7):
- Collector-Feld in BgpRecord (Phase 1.1)
- Propagation Aggregator (Phase 1.3)
- Wellenbaseline aus MRT-Archivdaten (Phase 2)
- Wave Anomaly Detector (Phase 3)
- Trust Score Engine (Phase 4)
- BGP-Speaker Integration / BIRD2 (Phase 6)

---

## 8. Erklärung des Urhebers

Ich, Alireza Shahsavar Khani, erkläre hiermit, dass ich der alleinige geistige
Urheber des in diesem Dokument beschriebenen technischen Konzepts bin. Das
Konzept wurde von mir eigenständig entwickelt. Die Entwicklung wurde durch
KI-Werkzeuge (Claude, Anthropic) unterstützt, die als Werkzeug dienten;
die zugrundeliegende Idee, die Problemstellung und die Lösungsarchitektur
stammen von mir.

Dieses Dokument wird zur notariellen Beglaubigung eingereicht, um Datum und
Urheberschaft rechtlich zu fixieren.

---

**Ort:** ________________________

**Datum:** 08.03.2026

**Unterschrift:** ________________________
Alireza Shahsavar Khani

---

*Dieses Dokument enthält vertrauliche technische Informationen.
Weitergabe nur unter NDA.*
