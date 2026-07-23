use std::fmt;
use std::ops::Bound;

use anyhow::{Context, Result, ensure};
use tantivy::columnar::BytesColumn;
use tantivy::query::{
    BooleanQuery, EnableScoring, ExistsQuery, Explanation, Occur, Query, RangeQuery, Scorer, Weight,
};
use tantivy::schema::Term;
use tantivy::{DocId, DocSet, Index, Score, SegmentReader, TERMINATED, TantivyError};

use super::{
    GeoBounds, GeoDistanceMode, GeoPoint, distance_for_encoded_points, encoded_points_match,
    geo_coordinate_field, geo_latitude_field, geo_longitude_field, haversine_distance_meters,
};
use crate::query::column;
use crate::{ColumnType, Comparison, TableDef};

#[derive(Clone, Debug)]
enum GeoPredicate {
    Radius {
        center: GeoPoint,
        radius_meters: f64,
    },
    Bounds(GeoBounds),
    DistanceCompare {
        center: GeoPoint,
        mode: GeoDistanceMode,
        operator: Comparison,
        distance_meters: f64,
    },
}

impl GeoPredicate {
    fn matches_encoded(&self, encoded: &[u8]) -> Result<bool> {
        match self {
            Self::Radius {
                center,
                radius_meters,
            } => encoded_points_match(encoded, |point| {
                haversine_distance_meters(*center, point) <= *radius_meters
            }),
            Self::Bounds(bounds) => encoded_points_match(encoded, |point| bounds.contains(point)),
            Self::DistanceCompare {
                center,
                mode,
                operator,
                distance_meters,
            } => Ok(distance_for_encoded_points(encoded, *center, *mode)?
                .is_some_and(|distance| compare_distance(distance, *operator, *distance_meters))),
        }
    }
}

struct GeoPredicateQuery {
    candidate: Box<dyn Query>,
    coordinate_field: String,
    predicate: GeoPredicate,
}

impl Clone for GeoPredicateQuery {
    fn clone(&self) -> Self {
        Self {
            candidate: self.candidate.box_clone(),
            coordinate_field: self.coordinate_field.clone(),
            predicate: self.predicate.clone(),
        }
    }
}

impl fmt::Debug for GeoPredicateQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeoPredicateQuery")
            .field("candidate", &self.candidate)
            .field("coordinate_field", &self.coordinate_field)
            .field("predicate", &self.predicate)
            .finish()
    }
}

impl Query for GeoPredicateQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(GeoPredicateWeight {
            candidate: self.candidate.weight(scoring)?,
            coordinate_field: self.coordinate_field.clone(),
            predicate: self.predicate.clone(),
        }))
    }
}

struct GeoPredicateWeight {
    candidate: Box<dyn Weight>,
    coordinate_field: String,
    predicate: GeoPredicate,
}

impl Weight for GeoPredicateWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let coordinates = reader
            .fast_fields()
            .bytes(&self.coordinate_field)?
            .ok_or_else(|| missing_geo_field(&self.coordinate_field))?;
        let candidate = self.candidate.scorer(reader, boost)?;
        Ok(Box::new(GeoPredicateScorer::new(
            candidate,
            coordinates,
            self.predicate.clone(),
        )))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(TantivyError::InvalidArgument(format!(
                "Document #({doc}) does not match geo predicate"
            )));
        }
        let mut explanation = Explanation::new("GeoPredicateQuery", scorer.score());
        explanation.add_detail(self.candidate.explain(reader, doc)?);
        Ok(explanation)
    }
}

struct GeoPredicateScorer {
    candidate: Box<dyn Scorer>,
    coordinates: BytesColumn,
    predicate: GeoPredicate,
    encoded: Vec<u8>,
}

impl GeoPredicateScorer {
    fn new(candidate: Box<dyn Scorer>, coordinates: BytesColumn, predicate: GeoPredicate) -> Self {
        let mut scorer = Self {
            candidate,
            coordinates,
            predicate,
            encoded: Vec::new(),
        };
        scorer.skip_non_matching();
        scorer
    }

    fn skip_non_matching(&mut self) {
        while self.candidate.doc() != TERMINATED && !self.current_matches() {
            self.candidate.advance();
        }
    }

    fn current_matches(&mut self) -> bool {
        self.encoded.clear();
        let Some(ord) = self.coordinates.ords().first(self.candidate.doc()) else {
            return false;
        };
        if self.coordinates.ord_to_bytes(ord, &mut self.encoded).ok() != Some(true) {
            return false;
        }
        self.predicate
            .matches_encoded(&self.encoded)
            .unwrap_or(false)
    }
}

impl DocSet for GeoPredicateScorer {
    fn advance(&mut self) -> DocId {
        self.candidate.advance();
        self.skip_non_matching();
        self.doc()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.candidate.seek(target);
        self.skip_non_matching();
        self.doc()
    }

    fn doc(&self) -> DocId {
        self.candidate.doc()
    }

    fn size_hint(&self) -> u32 {
        self.candidate.size_hint()
    }
}

impl Scorer for GeoPredicateScorer {
    fn score(&mut self) -> Score {
        self.candidate.score()
    }
}

pub(crate) fn compile_geo_radius(
    index: &Index,
    def: &TableDef,
    column_name: &str,
    center: GeoPoint,
    radius_meters: f64,
) -> Result<Box<dyn Query>> {
    geo_column(def, column_name)?;
    center.validate()?;
    ensure!(
        radius_meters.is_finite() && radius_meters >= 0.0,
        "geo radius_meters must be finite and non-negative"
    );
    let bounds = radius_bounds(center, radius_meters);
    predicate_query(
        index,
        column_name,
        bounds_candidate(index, column_name, bounds)?,
        GeoPredicate::Radius {
            center,
            radius_meters,
        },
    )
}

pub(crate) fn compile_geo_bounds(
    index: &Index,
    def: &TableDef,
    column_name: &str,
    bounds: GeoBounds,
) -> Result<Box<dyn Query>> {
    geo_column(def, column_name)?;
    let bounds = bounds.validate()?;
    predicate_query(
        index,
        column_name,
        bounds_candidate(index, column_name, bounds)?,
        GeoPredicate::Bounds(bounds),
    )
}

pub(crate) fn compile_geo_distance_comparison(
    index: &Index,
    def: &TableDef,
    column_name: &str,
    center: GeoPoint,
    mode: GeoDistanceMode,
    operator: Comparison,
    distance_meters: f64,
) -> Result<Box<dyn Query>> {
    geo_column(def, column_name)?;
    center.validate()?;
    ensure!(
        distance_meters.is_finite() && distance_meters >= 0.0,
        "geo distance comparison requires a finite non-negative distance"
    );
    let candidate: Box<dyn Query> = if matches!(
        operator,
        Comparison::Less | Comparison::LessOrEqual | Comparison::Equal
    ) {
        bounds_candidate(index, column_name, radius_bounds(center, distance_meters))?
    } else {
        Box::new(ExistsQuery::new(column_name.to_owned(), false))
    };
    predicate_query(
        index,
        column_name,
        candidate,
        GeoPredicate::DistanceCompare {
            center,
            mode,
            operator,
            distance_meters,
        },
    )
}

fn geo_column<'a>(def: &'a TableDef, name: &str) -> Result<&'a crate::ColumnDef> {
    let column = column(def, name)?;
    ensure!(
        matches!(
            column.data_type,
            ColumnType::GeoPoint | ColumnType::GeoPointArray
        ),
        "geo operation requires a GEO_POINT or GEO_POINT[] column"
    );
    ensure!(column.index.indexed, "geo column is not indexed: {name}");
    Ok(column)
}

fn predicate_query(
    index: &Index,
    column: &str,
    candidate: Box<dyn Query>,
    predicate: GeoPredicate,
) -> Result<Box<dyn Query>> {
    index
        .schema()
        .get_field(&geo_coordinate_field(column))
        .with_context(|| format!("missing geo coordinates for {column}"))?;
    Ok(Box::new(GeoPredicateQuery {
        candidate,
        coordinate_field: geo_coordinate_field(column),
        predicate,
    }))
}

fn bounds_candidate(index: &Index, column: &str, bounds: GeoBounds) -> Result<Box<dyn Query>> {
    let schema = index.schema();
    let latitude = schema.get_field(&geo_latitude_field(column))?;
    let longitude = schema.get_field(&geo_longitude_field(column))?;
    let latitude_query: Box<dyn Query> = Box::new(RangeQuery::new(
        Bound::Included(Term::from_field_f64(latitude, bounds.bottom_right.lat)),
        Bound::Included(Term::from_field_f64(latitude, bounds.top_left.lat)),
    ));
    let longitude_query = longitude_bounds_query(longitude, bounds);
    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Must, latitude_query),
        (Occur::Must, longitude_query),
    ])))
}

fn longitude_bounds_query(field: tantivy::schema::Field, bounds: GeoBounds) -> Box<dyn Query> {
    let range = |west, east| {
        Box::new(RangeQuery::new(
            Bound::Included(Term::from_field_f64(field, west)),
            Bound::Included(Term::from_field_f64(field, east)),
        )) as Box<dyn Query>
    };
    if bounds.crosses_antimeridian() {
        Box::new(BooleanQuery::new(vec![
            (Occur::Should, range(bounds.top_left.lon, 180.0)),
            (Occur::Should, range(-180.0, bounds.bottom_right.lon)),
        ]))
    } else {
        range(bounds.top_left.lon, bounds.bottom_right.lon)
    }
}

fn radius_bounds(center: GeoPoint, radius_meters: f64) -> GeoBounds {
    if radius_meters == 0.0 {
        return GeoBounds {
            top_left: center,
            bottom_right: center,
        };
    }
    let angular = (radius_meters / 6_371_008.8).min(std::f64::consts::PI);
    let latitude = center.lat.to_radians();
    let south = (latitude - angular).max(-std::f64::consts::FRAC_PI_2);
    let north = (latitude + angular).min(std::f64::consts::FRAC_PI_2);
    let longitude_delta =
        if south <= -std::f64::consts::FRAC_PI_2 || north >= std::f64::consts::FRAC_PI_2 {
            std::f64::consts::PI
        } else {
            (angular.sin() / latitude.cos())
                .clamp(-1.0, 1.0)
                .asin()
                .abs()
        };
    let full_longitude = longitude_delta >= std::f64::consts::PI;
    GeoBounds {
        top_left: GeoPoint {
            lat: north.to_degrees(),
            lon: if full_longitude {
                -180.0
            } else {
                normalize_longitude_degrees(center.lon - longitude_delta.to_degrees())
            },
        },
        bottom_right: GeoPoint {
            lat: south.to_degrees(),
            lon: if full_longitude {
                180.0
            } else {
                normalize_longitude_degrees(center.lon + longitude_delta.to_degrees())
            },
        },
    }
}

fn normalize_longitude_degrees(value: f64) -> f64 {
    (value + 180.0).rem_euclid(360.0) - 180.0
}

fn compare_distance(left: f64, operator: Comparison, right: f64) -> bool {
    match operator {
        Comparison::Equal => left == right,
        Comparison::NotEqual => left != right,
        Comparison::Greater => left > right,
        Comparison::GreaterOrEqual => left >= right,
        Comparison::Less => left < right,
        Comparison::LessOrEqual => left <= right,
    }
}

fn missing_geo_field(field: &str) -> TantivyError {
    TantivyError::SchemaError(format!("missing geo coordinate fast field: {field}"))
}
