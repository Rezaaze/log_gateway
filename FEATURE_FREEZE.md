# FEATURE FREEZE — log-gateway

**Datum:** 08.03.2026
**Status:** 🔒 EINGEFROREN

---

## Was das bedeutet

Ab diesem Datum werden **keine neuen Features** mehr am bestehenden
`log-gateway`-System entwickelt.

Die Weiterentwicklung erfolgt ausschließlich auf dem Branch `trustwave-core`
mit dem Ziel der Umsetzung des BGP TrustWave Systems gemäß `TRUSTWAVE_ROADMAP.md`.

## Was weiterhin erlaubt ist (auf `main`)

- ✅ Bugfixes die den laufenden Betrieb betreffen
- ✅ Security-Patches
- ✅ Dependency-Updates (Sicherheit)
- ✅ Dokumentation

## Was nicht mehr erlaubt ist (auf `main`)

- ❌ Neue Feature-Entwicklung am Log-Gateway
- ❌ Neue API-Endpunkte
- ❌ Neue Integrationen (S3, ClickHouse, etc.)
- ❌ Erweiterungen des Tenant-Systems

## Aktiver Entwicklungsbranch

```
trustwave-core
```

Alle neuen Entwicklungen gemäß `TRUSTWAVE_ROADMAP.md` laufen dort.

## Begründung

Das System wird zur BGP TrustWave Plattform weiterentwickelt.
Ressourcen-Fokus auf das Kernsystem: Wellenphysik-Triangulation,
Trust Score Engine, BGP-Speaker-Integration.

Siehe: `TRUSTWAVE_ROADMAP.md` für den vollständigen Plan.
