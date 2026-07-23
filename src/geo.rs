use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Highest Web Mercator zoom represented by FrankensteinDB's 62-bit Morton key.
pub const GEO_MAX_ZOOM: u8 = 31;

/// A WGS84 longitude/latitude coordinate expressed in degrees.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GeoPoint {
    /// Latitude in the inclusive range `[-90, 90]`.
    pub lat: f64,
    /// Longitude in the inclusive range `[-180, 180]`.
    pub lon: f64,
}

impl GeoPoint {
    /// Validates that both coordinates are finite and inside WGS84 degree bounds.
    pub fn validate(self) -> Result<Self> {
        ensure!(self.lat.is_finite(), "geo latitude must be finite");
        ensure!(self.lon.is_finite(), "geo longitude must be finite");
        ensure!(
            (-90.0..=90.0).contains(&self.lat),
            "geo latitude must be in [-90, 90]"
        );
        ensure!(
            (-180.0..=180.0).contains(&self.lon),
            "geo longitude must be in [-180, 180]"
        );
        Ok(self)
    }
}

/// Geographic rectangle. West greater than east means the box crosses the antimeridian.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GeoBounds {
    pub top_left: GeoPoint,
    pub bottom_right: GeoPoint,
}

impl GeoBounds {
    /// Validates both corners and north-to-south latitude ordering.
    pub fn validate(self) -> Result<Self> {
        self.top_left.validate()?;
        self.bottom_right.validate()?;
        ensure!(
            self.top_left.lat >= self.bottom_right.lat,
            "geo bounds top latitude must not be south of bottom latitude"
        );
        Ok(self)
    }

    pub(crate) fn contains(self, point: GeoPoint) -> bool {
        let latitude = point.lat <= self.top_left.lat && point.lat >= self.bottom_right.lat;
        let longitude = if self.crosses_antimeridian() {
            point.lon >= self.top_left.lon || point.lon <= self.bottom_right.lon
        } else {
            point.lon >= self.top_left.lon && point.lon <= self.bottom_right.lon
        };
        latitude && longitude
    }

    pub(crate) fn crosses_antimeridian(self) -> bool {
        self.top_left.lon > self.bottom_right.lon
    }
}

/// Reduction used when one document contains several geographic points.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GeoDistanceMode {
    /// Distance to the nearest point.
    #[default]
    Min,
    /// Distance to the farthest point.
    Max,
    /// Arithmetic mean of all point distances.
    Average,
}

/// Determines whether a geo-tile bucket counts documents or individual points.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GeoTileCountMode {
    /// Count a document at most once in each tile.
    #[default]
    Documents,
    /// Count every point, including multiple points from one document in the same tile.
    Points,
}

pub(crate) fn geo_coordinate_field(column: &str) -> String {
    format!("__aq_geo_coordinates_{column}")
}

pub(crate) fn geo_latitude_field(column: &str) -> String {
    format!("__aq_geo_latitude_{column}")
}

pub(crate) fn geo_longitude_field(column: &str) -> String {
    format!("__aq_geo_longitude_{column}")
}

mod encoding;
pub use encoding::haversine_distance_meters;
#[cfg(test)]
pub(crate) use encoding::morton_z31;
pub(crate) use encoding::{
    decode_points, distance_for_encoded_points, distance_for_points, encoded_points,
    encoded_points_match, index_geo_points, morton_at_zoom, morton_xy, parse_geo_json,
    validate_encoded_points,
};
mod query;
pub(crate) use query::*;
mod aggregation;
pub(crate) use aggregation::*;
