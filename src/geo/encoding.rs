use std::f64::consts::PI;

use anyhow::{Result, bail, ensure};
use tantivy::schema::{Field, TantivyDocument};

use super::{GEO_MAX_ZOOM, GeoDistanceMode, GeoPoint};

const EARTH_RADIUS_METERS: f64 = 6_371_008.8;
const WEB_MERCATOR_MAX_LATITUDE: f64 = 85.051_128_779_806_6;
const ENCODED_POINT_BYTES: usize = 24;

/// Calculates great-circle distance using the mean Earth radius.
pub fn haversine_distance_meters(first: GeoPoint, second: GeoPoint) -> f64 {
    let first_latitude = first.lat.to_radians();
    let second_latitude = second.lat.to_radians();
    let latitude_delta = second_latitude - first_latitude;
    let longitude_delta = normalize_longitude_radians((second.lon - first.lon).to_radians());
    let a = (latitude_delta / 2.0).sin().powi(2)
        + first_latitude.cos() * second_latitude.cos() * (longitude_delta / 2.0).sin().powi(2);
    let a = a.clamp(0.0, 1.0);
    EARTH_RADIUS_METERS * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

pub(crate) fn distance_for_points(
    points: &[GeoPoint],
    origin: GeoPoint,
    mode: GeoDistanceMode,
) -> Option<f64> {
    let mut distances = points
        .iter()
        .map(|point| haversine_distance_meters(origin, *point));
    let first = distances.next()?;
    Some(match mode {
        GeoDistanceMode::Min => distances.fold(first, f64::min),
        GeoDistanceMode::Max => distances.fold(first, f64::max),
        GeoDistanceMode::Average => {
            let (sum, count) = distances.fold((first, 1usize), |(sum, count), distance| {
                (sum + distance, count + 1)
            });
            sum / count as f64
        }
    })
}

pub(crate) fn distance_for_encoded_points(
    encoded: &[u8],
    origin: GeoPoint,
    mode: GeoDistanceMode,
) -> Result<Option<f64>> {
    ensure!(
        encoded.len().is_multiple_of(ENCODED_POINT_BYTES),
        "corrupt geo coordinate fast field"
    );
    let mut count = 0usize;
    let mut distance = None;
    for chunk in encoded.chunks_exact(ENCODED_POINT_BYTES) {
        let point = decode_point(chunk)?;
        let current = haversine_distance_meters(origin, point);
        count += 1;
        distance = Some(match (mode, distance) {
            (_, None) => current,
            (GeoDistanceMode::Min, Some(value)) => f64::min(value, current),
            (GeoDistanceMode::Max, Some(value)) => f64::max(value, current),
            (GeoDistanceMode::Average, Some(value)) => value + current,
        });
    }
    Ok(match (mode, distance) {
        (GeoDistanceMode::Average, Some(sum)) => Some(sum / count as f64),
        (_, value) => value,
    })
}

pub(crate) fn encoded_points_match(
    encoded: &[u8],
    mut predicate: impl FnMut(GeoPoint) -> bool,
) -> Result<bool> {
    ensure!(
        encoded.len().is_multiple_of(ENCODED_POINT_BYTES),
        "corrupt geo coordinate fast field"
    );
    for chunk in encoded.chunks_exact(ENCODED_POINT_BYTES) {
        if predicate(decode_point(chunk)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn encode_points(points: &[GeoPoint]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(points.len() * ENCODED_POINT_BYTES);
    for point in points {
        encoded.extend_from_slice(&point.lat.to_le_bytes());
        encoded.extend_from_slice(&point.lon.to_le_bytes());
        encoded.extend_from_slice(&morton_z31(*point).to_le_bytes());
    }
    encoded
}

pub(crate) fn add_points_to_document(
    document: &mut TantivyDocument,
    tile_field: Field,
    latitude_field: Field,
    longitude_field: Field,
    coordinate_field: Field,
    points: &[GeoPoint],
) {
    for point in points {
        document.add_u64(tile_field, morton_z31(*point));
        document.add_f64(latitude_field, point.lat);
        document.add_f64(longitude_field, point.lon);
    }
    document.add_bytes(coordinate_field, &encode_points(points));
}

pub(crate) fn index_geo_points(
    document: &mut TantivyDocument,
    fields: &crate::IndexFields,
    column: &crate::ColumnDef,
    points: &[GeoPoint],
) {
    add_points_to_document(
        document,
        fields.values[&column.name],
        fields.geo_latitudes[&column.name],
        fields.geo_longitudes[&column.name],
        fields.geo_coordinates[&column.name],
        points,
    );
}

pub(crate) fn decode_points(encoded: &[u8]) -> Result<Vec<GeoPoint>> {
    ensure!(
        encoded.len().is_multiple_of(ENCODED_POINT_BYTES),
        "corrupt geo coordinate fast field"
    );
    encoded
        .chunks_exact(ENCODED_POINT_BYTES)
        .map(decode_point)
        .collect()
}

fn decode_point(chunk: &[u8]) -> Result<GeoPoint> {
    let lat = f64::from_le_bytes(chunk[0..8].try_into().expect("eight bytes"));
    let lon = f64::from_le_bytes(chunk[8..16].try_into().expect("eight bytes"));
    GeoPoint { lat, lon }.validate()
}

pub(crate) fn encoded_points(encoded: &[u8]) -> impl Iterator<Item = (GeoPoint, u64)> + '_ {
    encoded
        .chunks_exact(ENCODED_POINT_BYTES)
        .filter_map(|chunk| {
            let point = GeoPoint {
                lat: f64::from_le_bytes(chunk[0..8].try_into().ok()?),
                lon: f64::from_le_bytes(chunk[8..16].try_into().ok()?),
            };
            let tile = u64::from_le_bytes(chunk[16..24].try_into().ok()?);
            Some((point, tile))
        })
}

pub(crate) fn validate_encoded_points(encoded: &[u8]) -> bool {
    encoded.len().is_multiple_of(ENCODED_POINT_BYTES)
}

pub(crate) fn morton_z31(point: GeoPoint) -> u64 {
    let (x, y) = web_mercator_xy(point, GEO_MAX_ZOOM);
    interleave(x, y)
}

pub(crate) fn morton_at_zoom(morton_z31: u64, zoom: u8) -> Result<u64> {
    ensure!(zoom <= GEO_MAX_ZOOM, "geo tile zoom must be in 0..=31");
    Ok(if zoom == 0 {
        0
    } else {
        morton_z31 >> (2 * (GEO_MAX_ZOOM - zoom))
    })
}

pub(crate) fn morton_xy(value: u64, zoom: u8) -> Result<(u32, u32)> {
    ensure!(zoom <= GEO_MAX_ZOOM, "geo tile zoom must be in 0..=31");
    let mut x = 0u32;
    let mut y = 0u32;
    for bit in 0..zoom {
        x |= (((value >> (2 * bit)) & 1) as u32) << bit;
        y |= (((value >> (2 * bit + 1)) & 1) as u32) << bit;
    }
    Ok((x, y))
}

fn web_mercator_xy(point: GeoPoint, zoom: u8) -> (u32, u32) {
    let tiles = (1u64 << zoom) as f64;
    let longitude = if point.lon == 180.0 {
        -180.0
    } else {
        point.lon
    };
    let latitude = point
        .lat
        .clamp(-WEB_MERCATOR_MAX_LATITUDE, WEB_MERCATOR_MAX_LATITUDE)
        .to_radians();
    let x = ((longitude + 180.0) / 360.0 * tiles).floor();
    let y = ((1.0 - latitude.tan().asinh() / PI) / 2.0 * tiles).floor();
    let maximum = tiles - 1.0;
    (x.clamp(0.0, maximum) as u32, y.clamp(0.0, maximum) as u32)
}

fn interleave(x: u32, y: u32) -> u64 {
    let mut value = 0u64;
    for bit in 0..GEO_MAX_ZOOM {
        value |= u64::from((x >> bit) & 1) << (2 * bit);
        value |= u64::from((y >> bit) & 1) << (2 * bit + 1);
    }
    value
}

fn normalize_longitude_radians(value: f64) -> f64 {
    (value + PI).rem_euclid(2.0 * PI) - PI
}

pub(crate) fn parse_geo_json(value: &serde_json::Value, array: bool) -> Result<Vec<GeoPoint>> {
    let points = if array {
        serde_json::from_value::<Vec<GeoPoint>>(value.clone())?
    } else {
        vec![serde_json::from_value::<GeoPoint>(value.clone())?]
    };
    ensure!(
        points.len() <= 10_000,
        "geo arrays support at most 10000 points"
    );
    for point in &points {
        point.validate()?;
    }
    if !array && points.len() != 1 {
        bail!("GEO_POINT requires exactly one point");
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: GeoPoint = GeoPoint { lat: 0.0, lon: 0.0 };

    #[test]
    fn identical_points_have_zero_distance() {
        assert_eq!(haversine_distance_meters(ORIGIN, ORIGIN), 0.0);
    }

    #[test]
    fn distance_is_symmetric_across_the_antimeridian() {
        let east = GeoPoint {
            lat: 0.0,
            lon: 179.9,
        };
        let west = GeoPoint {
            lat: 0.0,
            lon: -179.9,
        };
        let forward = haversine_distance_meters(east, west);
        let reverse = haversine_distance_meters(west, east);
        assert!((forward - 22_239.0).abs() < 100.0);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn antipodal_distance_is_half_the_earth_circumference() {
        let distance = haversine_distance_meters(
            ORIGIN,
            GeoPoint {
                lat: 0.0,
                lon: 180.0,
            },
        );
        assert!((distance - std::f64::consts::PI * EARTH_RADIUS_METERS).abs() < 0.001);
    }

    #[test]
    fn binary_encoding_preserves_exact_f64_coordinates() {
        let points = [
            GeoPoint {
                lat: 12.345_678_901,
                lon: -98.765_432_109,
            },
            GeoPoint {
                lat: -90.0,
                lon: 180.0,
            },
        ];
        let encoded = encode_points(&points);
        assert_eq!(encoded.len(), points.len() * ENCODED_POINT_BYTES);
        assert_eq!(decode_points(&encoded).unwrap(), points);
    }

    #[test]
    fn malformed_binary_encoding_is_rejected() {
        assert!(decode_points(&[0; ENCODED_POINT_BYTES - 1]).is_err());
        assert!(!validate_encoded_points(&[0; ENCODED_POINT_BYTES + 1]));
    }

    #[test]
    fn distance_reductions_have_expected_values() {
        let points = [ORIGIN, GeoPoint { lat: 0.0, lon: 1.0 }];
        let far = haversine_distance_meters(ORIGIN, points[1]);
        assert_eq!(
            distance_for_points(&points, ORIGIN, GeoDistanceMode::Min),
            Some(0.0)
        );
        assert_eq!(
            distance_for_points(&points, ORIGIN, GeoDistanceMode::Max),
            Some(far)
        );
        assert_eq!(
            distance_for_points(&points, ORIGIN, GeoDistanceMode::Average),
            Some(far / 2.0)
        );
        assert_eq!(distance_for_points(&[], ORIGIN, GeoDistanceMode::Min), None);
    }

    #[test]
    fn every_lower_zoom_is_a_morton_prefix() {
        let tile = morton_z31(GeoPoint {
            lat: 41.9,
            lon: 12.5,
        });
        for zoom in 1..=GEO_MAX_ZOOM {
            assert_eq!(
                morton_at_zoom(tile, zoom).unwrap(),
                tile >> (2 * (GEO_MAX_ZOOM - zoom))
            );
        }
    }

    #[test]
    fn decoded_parent_coordinates_match_shifted_children() {
        let tile = morton_z31(GeoPoint {
            lat: -33.86,
            lon: 151.21,
        });
        let (full_x, full_y) = morton_xy(tile, GEO_MAX_ZOOM).unwrap();
        for zoom in [1, 5, 12, 20, 30, 31] {
            let parent = morton_at_zoom(tile, zoom).unwrap();
            let (x, y) = morton_xy(parent, zoom).unwrap();
            assert_eq!(x, full_x >> (GEO_MAX_ZOOM - zoom));
            assert_eq!(y, full_y >> (GEO_MAX_ZOOM - zoom));
        }
    }

    #[test]
    fn json_parser_distinguishes_scalar_and_array_shapes() {
        let point = serde_json::json!({"lat": 1.0, "lon": 2.0});
        let array = serde_json::json!([{"lat": 1.0, "lon": 2.0}]);
        assert_eq!(parse_geo_json(&point, false).unwrap().len(), 1);
        assert_eq!(parse_geo_json(&array, true).unwrap().len(), 1);
        assert!(parse_geo_json(&point, true).is_err());
        assert!(parse_geo_json(&array, false).is_err());
    }
}
