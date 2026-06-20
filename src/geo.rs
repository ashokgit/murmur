use std::str::FromStr;
use h3o::{CellIndex, LatLng, Resolution};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Coords {
    pub lat: f64,
    pub lon: f64,
}

pub fn coarsen(lat: f64, lon: f64) -> (f64, f64) {
    let grid = 0.009; // ~1km in degrees
    ((lat / grid).round() * grid, (lon / grid).round() * grid)
}

pub fn step_to_resolution(step: u32) -> Resolution {
    match step {
        1 => Resolution::Ten,
        2 => Resolution::Nine,
        3 => Resolution::Eight,
        4 => Resolution::Seven,
        _ => Resolution::Six,
    }
}

pub fn get_cells_for_coords(lat: f64, lon: f64, resolution: Resolution) -> Vec<CellIndex> {
    let (c_lat, c_lon) = coarsen(lat, lon);
    if let Ok(latlng) = LatLng::new(c_lat, c_lon) {
        let center = latlng.to_cell(resolution);
        center.grid_disk::<Vec<CellIndex>>(1)
    } else {
        vec![]
    }
}

pub async fn resolve_ip_location() -> Option<Coords> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    
    #[derive(Deserialize)]
    struct IpApiResponse {
        lat: f64,
        lon: f64,
        status: String,
    }

    let resp = client.get("http://ip-api.com/json/")
        .send()
        .await
        .ok()?
        .json::<IpApiResponse>()
        .await
        .ok()?;

    if resp.status == "success" {
        Some(Coords {
            lat: resp.lat,
            lon: resp.lon,
        })
    } else {
        None
    }
}

pub fn resolve_maxmind_location(db_path: &str, ip: &str) -> Option<Coords> {
    let reader = maxminddb::Reader::open_readfile(db_path).ok()?;
    let ip_addr = std::net::IpAddr::from_str(ip).ok()?;
    let city: maxminddb::geoip2::City = reader.lookup(ip_addr).ok()?;
    let location = city.location?;
    Some(Coords {
        lat: location.latitude?,
        lon: location.longitude?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinate_coarsening() {
        let (lat1, lon1) = coarsen(27.7172, 85.3240);
        let (lat2, lon2) = coarsen(27.7190, 85.3220);
        assert_eq!((lat1, lon1), (lat2, lon2), "Nearby coordinates should coarsen to the same values");
    }

    #[test]
    fn test_resolution_mapping() {
        use h3o::Resolution;
        assert_eq!(step_to_resolution(1), Resolution::Ten);
        assert_eq!(step_to_resolution(3), Resolution::Eight);
        assert_eq!(step_to_resolution(5), Resolution::Six);
    }
}
