//! Registry of RIPE RIS Route Collectors.
//!
//! Contains a static table of all 26 RIPE RIS collectors with their metadata.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum Region {
    /// Europe
    EU,
    /// North America
    NA,
    /// Asia-Pacific
    APAC,
    /// South America
    SA,
    /// Africa
    AF,
    /// Middle East
    ME,
}

#[derive(Debug, Clone)]
pub struct CollectorInfo {
    /// Collector ID (e.g., "rrc12")
    pub id: &'static str,
    /// City where the collector is located
    pub city: &'static str,
    /// Latitude in decimal degrees
    pub lat: f64,
    /// Longitude in decimal degrees
    pub lon: f64,
    /// Internet Exchange Point name
    pub ixp: &'static str,
    /// Geographic region
    pub region: Region,
}

/// Static table of all 26 RIPE RIS Route Collectors.
pub static COLLECTORS: &[CollectorInfo] = &[
    CollectorInfo {
        id: "rrc00",
        city: "Amsterdam",
        lat: 52.37,
        lon: 4.90,
        ixp: "AMS-IX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc01",
        city: "London",
        lat: 51.51,
        lon: -0.13,
        ixp: "LINX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc03",
        city: "Amsterdam",
        lat: 52.37,
        lon: 4.90,
        ixp: "AMS-IX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc04",
        city: "Geneva",
        lat: 46.20,
        lon: 6.15,
        ixp: "CERN",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc05",
        city: "Vienna",
        lat: 48.21,
        lon: 16.37,
        ixp: "VIX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc06",
        city: "Otemachi",
        lat: 35.69,
        lon: 139.76,
        ixp: "DIX-IE",
        region: Region::APAC,
    },
    CollectorInfo {
        id: "rrc07",
        city: "Stockholm",
        lat: 59.33,
        lon: 18.07,
        ixp: "Netnod",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc10",
        city: "Milan",
        lat: 45.46,
        lon: 9.19,
        ixp: "MIX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc11",
        city: "New York",
        lat: 40.71,
        lon: -74.00,
        ixp: "NYIIX",
        region: Region::NA,
    },
    CollectorInfo {
        id: "rrc12",
        city: "Frankfurt",
        lat: 50.11,
        lon: 8.68,
        ixp: "DE-CIX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc13",
        city: "Moscow",
        lat: 55.75,
        lon: 37.62,
        ixp: "MSK-IX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc14",
        city: "Palo Alto",
        lat: 37.44,
        lon: -122.14,
        ixp: "Equinix SV",
        region: Region::NA,
    },
    CollectorInfo {
        id: "rrc15",
        city: "Sao Paulo",
        lat: -23.55,
        lon: -46.63,
        ixp: "PTT.br",
        region: Region::SA,
    },
    CollectorInfo {
        id: "rrc16",
        city: "Miami",
        lat: 25.77,
        lon: -80.19,
        ixp: "Equinix MI",
        region: Region::NA,
    },
    CollectorInfo {
        id: "rrc17",
        city: "Singapore",
        lat: 1.35,
        lon: 103.82,
        ixp: "Equinix SG",
        region: Region::APAC,
    },
    CollectorInfo {
        id: "rrc18",
        city: "Barcelona",
        lat: 41.39,
        lon: 2.15,
        ixp: "CATNIX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc19",
        city: "Johannesburg",
        lat: -26.20,
        lon: 28.04,
        ixp: "NAPAfrica",
        region: Region::AF,
    },
    CollectorInfo {
        id: "rrc20",
        city: "Zurich",
        lat: 47.38,
        lon: 8.54,
        ixp: "SwissIX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc21",
        city: "Paris",
        lat: 48.86,
        lon: 2.35,
        ixp: "France-IX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc22",
        city: "Bucharest",
        lat: 44.43,
        lon: 26.10,
        ixp: "Interlan",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc23",
        city: "Singapore",
        lat: 1.35,
        lon: 103.82,
        ixp: "Equinix SG",
        region: Region::APAC,
    },
    CollectorInfo {
        id: "rrc24",
        city: "Montevideo",
        lat: -34.90,
        lon: -56.19,
        ixp: "ANTEL",
        region: Region::SA,
    },
    CollectorInfo {
        id: "rrc25",
        city: "Amsterdam",
        lat: 52.37,
        lon: 4.90,
        ixp: "AMS-IX",
        region: Region::EU,
    },
    CollectorInfo {
        id: "rrc26",
        city: "Dubai",
        lat: 25.20,
        lon: 55.27,
        ixp: "UAE-IX",
        region: Region::ME,
    },
];

/// Thread-safe lookup map for collector IDs.
static LOOKUP: OnceLock<HashMap<&'static str, &'static CollectorInfo>> = OnceLock::new();

/// Gibt CollectorInfo für eine Kollektor-ID zurück.
/// Gibt None zurück wenn die ID unbekannt ist.
pub fn lookup(id: &str) -> Option<&'static CollectorInfo> {
    let map = LOOKUP.get_or_init(|| COLLECTORS.iter().map(|c| (c.id, c)).collect());
    map.get(id).copied()
}

/// Berechnet die Großkreisdistanz zwischen zwei Kollektoren in km.
/// Gibt None wenn einer der IDs unbekannt ist.
pub fn geographic_distance_km(id_a: &str, id_b: &str) -> Option<f64> {
    // Haversine-Formel
    // Erdradius: 6371.0 km
    let collector_a = lookup(id_a)?;
    let collector_b = lookup(id_b)?;

    // Convert degrees to radians
    let lat_a = collector_a.lat.to_radians();
    let lon_a = collector_a.lon.to_radians();
    let lat_b = collector_b.lat.to_radians();
    let lon_b = collector_b.lon.to_radians();

    // Differences
    let delta_lat = lat_b - lat_a;
    let delta_lon = lon_b - lon_a;

    // Haversine formula
    let a = (delta_lat / 2.0).sin().powi(2)
        + lat_a.cos() * lat_b.cos() * (delta_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    // Distance in kilometers
    Some(6371.0 * c)
}

/// Berechnet die physikalisch minimale Latenz zwischen zwei Kollektoren in ms.
/// Basis: Glasfaser-Lichtgeschwindigkeit ≈ 200.000 km/s (2/3 c).
/// Gibt None wenn einer der IDs unbekannt ist.
pub fn min_latency_ms(id_a: &str, id_b: &str) -> Option<f64> {
    let dist_km = geographic_distance_km(id_a, id_b)?;
    Some(dist_km / 200.0) // 200 km pro Millisekunde
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_known_collector() {
        let info = lookup("rrc12").expect("rrc12 must exist");
        assert_eq!(info.city, "Frankfurt");
        assert_eq!(info.ixp, "DE-CIX");
        assert!(matches!(info.region, Region::EU));
    }

    #[test]
    fn test_lookup_unknown_returns_none() {
        assert!(lookup("rrc99").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("unknown").is_none());
    }

    #[test]
    fn test_all_26_collectors_are_reachable() {
        let ids = [
            "rrc00", "rrc01", "rrc03", "rrc04", "rrc05", "rrc06", "rrc07", "rrc10", "rrc11",
            "rrc12", "rrc13", "rrc14", "rrc15", "rrc16", "rrc17", "rrc18", "rrc19", "rrc20",
            "rrc21", "rrc22", "rrc23", "rrc24", "rrc25", "rrc26",
        ];
        for id in ids {
            assert!(lookup(id).is_some(), "Collector {id} not found");
        }
    }

    #[test]
    fn test_distance_amsterdam_frankfurt_approx_400km() {
        let d = geographic_distance_km("rrc00", "rrc12").unwrap();
        // Amsterdam → Frankfurt ≈ 370–410 km
        assert!(d > 350.0 && d < 450.0, "Expected ~400km, got {d:.1}km");
    }

    #[test]
    fn test_distance_same_collector_is_zero() {
        let d = geographic_distance_km("rrc12", "rrc12").unwrap();
        assert!(d < 0.001, "Same collector distance must be ~0, got {d}");
    }

    #[test]
    fn test_distance_is_symmetric() {
        let d1 = geographic_distance_km("rrc12", "rrc11").unwrap();
        let d2 = geographic_distance_km("rrc11", "rrc12").unwrap();
        assert!((d1 - d2).abs() < 0.001);
    }

    #[test]
    fn test_distance_unknown_collector_returns_none() {
        assert!(geographic_distance_km("rrc12", "rrc99").is_none());
    }

    #[test]
    fn test_latency_amsterdam_frankfurt_approx_2ms() {
        let ms = min_latency_ms("rrc00", "rrc12").unwrap();
        // ~400km / 200 km/ms ≈ 2ms
        assert!(ms > 1.5 && ms < 3.0, "Expected ~2ms, got {ms:.2}ms");
    }

    #[test]
    fn test_latency_frankfurt_new_york_approx_35ms() {
        let ms = min_latency_ms("rrc12", "rrc11").unwrap();
        // Frankfurt → New York ≈ 6200km / 200 km/ms ≈ 31ms
        assert!(ms > 25.0 && ms < 45.0, "Expected ~31ms, got {ms:.2}ms");
    }

    #[test]
    fn test_latency_is_symmetric() {
        let t1 = min_latency_ms("rrc12", "rrc17").unwrap();
        let t2 = min_latency_ms("rrc17", "rrc12").unwrap();
        assert!((t1 - t2).abs() < 0.001);
    }

    #[test]
    fn test_latency_unknown_collector_returns_none() {
        assert!(min_latency_ms("rrc12", "rrc99").is_none());
    }

    #[test]
    fn test_latency_same_collector_is_zero() {
        let ms = min_latency_ms("rrc12", "rrc12").unwrap();
        assert!(ms < 0.001, "Same collector latency must be ~0, got {ms}");
    }
}
