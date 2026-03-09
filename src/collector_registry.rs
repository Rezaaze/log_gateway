/// Kollektor-Registrierung — Geografische Daten aller RIPE RIS Kollektoren
/// ================================================================
///
/// Statische Kompilierungszeit-Daten für alle 26 RIPE RIS Kollektoren
/// (Stand: 2026)
use std::collections::HashMap;
use std::sync::OnceLock;

/// Regionen für geografische Gruppierung
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Region {
    EU,   // Europa
    NA,   // Nordamerika
    APAC, // Asia-Pacific
    SA,   // Südamerika
    AF,   // Afrika
    OC,   // Ozeanien
}

impl Region {
    pub fn as_str(&self) -> &'static str {
        match self {
            Region::EU => "EU",
            Region::NA => "NA",
            Region::APAC => "APAC",
            Region::SA => "SA",
            Region::AF => "AF",
            Region::OC => "OC",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "EU" => Some(Region::EU),
            "NA" => Some(Region::NA),
            "APAC" => Some(Region::APAC),
            "SA" => Some(Region::SA),
            "AF" => Some(Region::AF),
            "OC" => Some(Region::OC),
            _ => None,
        }
    }
}

/// Information über einen einzelnen Kollektor
#[derive(Debug, Clone, PartialEq)]
pub struct CollectorInfo {
    pub id: &'static str,      // z.B. "rrc12"
    pub city: &'static str,    // Stadt
    pub country: &'static str, // Land (ISO 3166-1 alpha-2)
    pub lat: f64,              // Latitude (-90 bis 90)
    pub lon: f64,              // Longitude (-180 bis 180)
    pub ixp: &'static str,     // Peering-Exchange Name
    pub region: Region,        // Geografische Region
}

/// Statische Kollektor-Tabelle (26 RIPE RIS Kollektoren)
pub static COLLECTORS: OnceLock<Vec<CollectorInfo>> = OnceLock::new();

/// Lookup-Tabelle für O(1)-Zugriff
static COLLECTOR_LOOKUP: OnceLock<HashMap<&'static str, &'static CollectorInfo>> = OnceLock::new();

/// Initialize collector registry
fn init_collectors() -> Vec<CollectorInfo> {
    vec![
        // Europa
        CollectorInfo {
            id: "rrc00",
            city: "Amsterdam",
            country: "NL",
            lat: 52.3702,
            lon: 4.8952,
            ixp: "AMS-IX",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc01",
            city: "Dresden",
            country: "DE",
            lat: 51.0504,
            lon: 13.7373,
            ixp: "DE-CIX Dresden",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc02",
            city: "Frankfurt",
            country: "DE",
            lat: 50.1109,
            lon: 8.6821,
            ixp: "DE-CIX Frankfurt",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc04",
            city: "Dublin",
            country: "IE",
            lat: 53.3498,
            lon: -6.2603,
            ixp: "Dublin Internet Exchange",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc06",
            city: "Ljubljana",
            country: "SI",
            lat: 46.0569,
            lon: 14.5058,
            ixp: "LIX",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc07",
            city: "Bonn",
            country: "DE",
            lat: 50.7374,
            lon: 7.0982,
            ixp: "Cologne IX",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc08",
            city: "Warsaw",
            country: "PL",
            lat: 52.2297,
            lon: 21.0122,
            ixp: "PLIX Warsaw",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc09",
            city: "Zurich",
            country: "CH",
            lat: 47.3769,
            lon: 8.5417,
            ixp: "SWISSIX",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc10",
            city: "Stockholm",
            country: "SE",
            lat: 59.3293,
            lon: 18.0686,
            ixp: "STHIX",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc12",
            city: "Frankfurt",
            country: "DE",
            lat: 50.1109,
            lon: 8.6821,
            ixp: "DE-CIX Frankfurt",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc13",
            city: "Kigali",
            country: "RW",
            lat: -1.9736,
            lon: 30.1044,
            ixp: "RINAX",
            region: Region::AF,
        },
        CollectorInfo {
            id: "rrc14",
            city: "Chisinau",
            country: "MD",
            lat: 47.0105,
            lon: 28.8638,
            ixp: "MOLNAP",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc15",
            city: "Poznan",
            country: "PL",
            lat: 52.4064,
            lon: 16.9252,
            ixp: "PLIX Poznan",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc16",
            city: "London",
            country: "GB",
            lat: 51.5074,
            lon: -0.1278,
            ixp: "LINX London",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc18",
            city: "Porto",
            country: "PT",
            lat: 41.1579,
            lon: -8.6291,
            ixp: "PTIX",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc19",
            city: "Helsinki",
            country: "FI",
            lat: 60.1699,
            lon: 24.9384,
            ixp: "HEIX",
            region: Region::EU,
        },
        CollectorInfo {
            id: "rrc20",
            city: "Istanbul",
            country: "TR",
            lat: 41.0082,
            lon: 28.9784,
            ixp: "ILS",
            region: Region::EU,
        },
        // Nordamerika
        CollectorInfo {
            id: "rrc03",
            city: "New York",
            country: "US",
            lat: 40.7128,
            lon: -74.0060,
            ixp: "NYIIX",
            region: Region::NA,
        },
        CollectorInfo {
            id: "rrc05",
            city: "San Jose",
            country: "US",
            lat: 37.3382,
            lon: -121.8863,
            ixp: "Equinix",
            region: Region::NA,
        },
        CollectorInfo {
            id: "rrc11",
            city: "Montreal",
            country: "CA",
            lat: 45.5017,
            lon: -73.5673,
            ixp: "Montreal Peer",
            region: Region::NA,
        },
        CollectorInfo {
            id: "rrc24",
            city: "Bogota",
            country: "CO",
            lat: 4.7110,
            lon: -74.0721,
            ixp: "Compulink",
            region: Region::SA,
        },
        CollectorInfo {
            id: "rrc25",
            city: "Quito",
            country: "EC",
            lat: -0.1807,
            lon: -78.4678,
            ixp: "NIC Ecuador",
            region: Region::SA,
        },
        // Asia-Pacific
        CollectorInfo {
            id: "rrc17",
            city: "Singapore",
            country: "SG",
            lat: 1.3521,
            lon: 103.8198,
            ixp: "Equinix SG",
            region: Region::APAC,
        },
        CollectorInfo {
            id: "rrc21",
            city: "Brisbane",
            country: "AU",
            lat: -27.4698,
            lon: 153.0251,
            ixp: "APNIC",
            region: Region::OC,
        },
        CollectorInfo {
            id: "rrc22",
            city: "Manila",
            country: "PH",
            lat: 14.5995,
            lon: 120.9842,
            ixp: "PhXL",
            region: Region::APAC,
        },
        // Afrika/Mitteost
        CollectorInfo {
            id: "rrc23",
            city: "Casablanca",
            country: "MA",
            lat: 33.5731,
            lon: -7.5898,
            ixp: "MA-IX",
            region: Region::AF,
        },
    ]
}

/// Initialize lookup table
fn init_lookup() -> HashMap<&'static str, &'static CollectorInfo> {
    let collectors = init_collectors();
    let mut map: HashMap<&'static str, &'static CollectorInfo> =
        HashMap::with_capacity(collectors.len());
    for c in collectors {
        map.insert(c.id, Box::leak(Box::new(c)));
    }
    map
}

/// Initialize collectors (returns the vector for OnceLock)
fn init_collectors_static() -> Vec<CollectorInfo> {
    init_collectors()
}

/// Initialize lookup (returns the HashMap for OnceLock)
fn init_lookup_static() -> HashMap<&'static str, &'static CollectorInfo> {
    init_lookup()
}

/// Get the list of all known collectors
pub fn all_collectors() -> &'static [CollectorInfo] {
    COLLECTORS.get_or_init(init_collectors_static)
}

/// Lookup a collector by ID
#[must_use]
pub fn lookup(id: &str) -> Option<&'static CollectorInfo> {
    COLLECTOR_LOOKUP
        .get_or_init(init_lookup_static)
        .get(id)
        .copied()
}

/// Check if a collector ID is known
#[must_use]
pub fn is_known(id: &str) -> bool {
    lookup(id).is_some()
}

/// Calculate geographic distance between two collectors using Haversine formula
///
/// Parameters:
/// - lat1, lon1: First collector coordinates (degrees)
/// - lat2, lon2: Second collector coordinates (degrees)
///
/// Returns:
/// - Distance in kilometers
#[must_use]
pub fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;

    let lat1_rad = lat1.to_radians();
    let lat2_rad = lat2.to_radians();
    let delta_lat = (lat2 - lat1).to_radians();
    let delta_lon = (lon2 - lon1).to_radians();

    let a = (delta_lat / 2.0).sin().powi(2)
        + lat1_rad.cos() * lat2_rad.cos() * (delta_lon / 2.0).sin().powi(2);

    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    EARTH_RADIUS_KM * c
}

/// Distance between two collector IDs (looks up their coordinates first)
#[must_use]
pub fn distance_between_collectors(id1: &str, id2: &str) -> Option<f64> {
    let c1 = lookup(id1)?;
    let c2 = lookup(id2)?;
    Some(distance_km(c1.lat, c1.lon, c2.lat, c2.lon))
}

/// Calculate expected light transit time between two points
///
/// In fiber optic cables, light travels at approximately 2/3 the speed of light in vacuum
/// (refractive index ~1.5), which equals ~200,000 km/s.
///
/// Parameters:
/// - dist_km: Distance in kilometers
///
/// Returns:
/// - Expected transit time in milliseconds
#[must_use]
pub fn light_transit_time_ms(dist_km: f64) -> f64 {
    const LIGHT_SPEED_KM_PER_S: f64 = 200_000.0; // in fiber
    (dist_km / LIGHT_SPEED_KM_PER_S) * 1_000.0 // convert to ms
}

/// Expected light transit time between two collectors
#[must_use]
pub fn expected_transit_time(id1: &str, id2: &str) -> Option<f64> {
    let dist = distance_between_collectors(id1, id2)?;
    Some(light_transit_time_ms(dist))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_collectors_count() {
        let collectors = all_collectors();
        assert_eq!(collectors.len(), 26);
    }

    #[test]
    fn test_lookup_known_collector() {
        let info = lookup("rrc12").expect("rrc12 should exist");
        assert_eq!(info.id, "rrc12");
        assert_eq!(info.city, "Frankfurt");
        assert_eq!(info.region, Region::EU);
    }

    #[test]
    fn test_lookup_unknown_collector() {
        assert!(lookup("rrc99").is_none());
    }

    #[test]
    fn test_distance_self() {
        // Distance to itself should be ~0
        let dist = distance_km(52.3702, 4.8952, 52.3702, 4.8952);
        assert!(dist < 0.001);
    }

    #[test]
    fn test_distance_ams_fra() {
        // Amsterdam (rrc00) to Frankfurt (rrc12) - actual ~360km
        let dist = distance_km(52.3702, 4.8952, 50.1109, 8.6821);
        // The test uses rrc00 coords (52.3702, 4.8952) + rrc12 (50.1109, 8.6821)
        assert!(
            dist > 300.0 && dist < 420.0,
            "Got distance: {dist} km — adjust coords if needed"
        );
    }

    #[test]
    fn test_distance_amsterdam_newyork() {
        // Amsterdam to New York ~5900km
        let dist = distance_km(52.3702, 4.8952, 40.7128, -74.0060);
        assert!(dist > 5500.0 && dist < 6500.0);
    }

    #[test]
    fn test_transit_time_known() {
        // Frankfurt to Amsterdam ~360km → ~1.8ms (360/200 = 1.8)
        let transit = expected_transit_time("rrc12", "rrc00").expect("Both exist");
        assert!(
            transit > 1.5 && transit < 2.5,
            "Got transit time: {transit} ms"
        );
    }

    #[test]
    fn test_transit_time_same_collector() {
        // Same collector → 0ms
        let transit = expected_transit_time("rrc12", "rrc12").expect("Both exist");
        assert!(transit < 0.001);
    }

    #[test]
    fn test_is_known() {
        assert!(is_known("rrc12"));
        assert!(!is_known("rrc99"));
    }

    #[test]
    fn test_region_from_str() {
        assert_eq!(Region::parse("EU"), Some(Region::EU));
        assert_eq!(Region::parse("NA"), Some(Region::NA));
        assert_eq!(Region::parse("APAC"), Some(Region::APAC));
        assert_eq!(Region::parse("XX"), None);
    }
}
