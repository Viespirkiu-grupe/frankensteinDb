use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::BytesColumn;
use tantivy::query::Query;
use tantivy::{DocId, Score, Searcher, SegmentOrdinal, SegmentReader, TantivyError};

use super::{
    GEO_MAX_ZOOM, GeoBounds, GeoTileCountMode, encoded_points, geo_coordinate_field,
    morton_at_zoom, morton_xy, validate_encoded_points,
};
use crate::query::column;
use crate::{Aggregation, ColumnType, TableDef};

struct GeoTileGridCollector {
    coordinate_field: String,
    zoom: u8,
    max_buckets: usize,
    count_mode: GeoTileCountMode,
    bounds: Option<GeoBounds>,
}

struct GeoTileSegmentCollector {
    coordinates: BytesColumn,
    zoom: u8,
    max_buckets: usize,
    count_mode: GeoTileCountMode,
    bounds: Option<GeoBounds>,
    counts: BTreeMap<u64, u64>,
    encoded: Vec<u8>,
    tiles: Vec<u64>,
    overflowed: bool,
    corrupt: bool,
}

struct GeoTileFruit {
    counts: BTreeMap<u64, u64>,
    overflowed: bool,
    corrupt: bool,
}

impl Collector for GeoTileGridCollector {
    type Fruit = BTreeMap<u64, u64>;
    type Child = GeoTileSegmentCollector;

    fn for_segment(
        &self,
        _segment_local_id: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let coordinates = segment
            .fast_fields()
            .bytes(&self.coordinate_field)?
            .ok_or_else(|| {
                TantivyError::SchemaError(format!(
                    "missing geo coordinate fast field: {}",
                    self.coordinate_field
                ))
            })?;
        Ok(GeoTileSegmentCollector {
            coordinates,
            zoom: self.zoom,
            max_buckets: self.max_buckets,
            count_mode: self.count_mode,
            bounds: self.bounds,
            counts: BTreeMap::new(),
            encoded: Vec::new(),
            tiles: Vec::new(),
            overflowed: false,
            corrupt: false,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<<Self::Child as SegmentCollector>::Fruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut counts = BTreeMap::new();
        for fruit in segment_fruits {
            if fruit.corrupt {
                return Err(TantivyError::InvalidArgument(
                    "corrupt geo coordinate fast field".into(),
                ));
            }
            if fruit.overflowed {
                return Err(bucket_limit_error(self.max_buckets));
            }
            for (tile, count) in fruit.counts {
                *counts.entry(tile).or_default() += count;
                if counts.len() > self.max_buckets {
                    return Err(bucket_limit_error(self.max_buckets));
                }
            }
        }
        Ok(counts)
    }
}

impl SegmentCollector for GeoTileSegmentCollector {
    type Fruit = GeoTileFruit;

    fn collect(&mut self, doc: DocId, _score: Score) {
        if self.overflowed || self.corrupt {
            return;
        }
        self.encoded.clear();
        let Some(ord) = self.coordinates.ords().first(doc) else {
            return;
        };
        if self.coordinates.ord_to_bytes(ord, &mut self.encoded).ok() != Some(true)
            || !validate_encoded_points(&self.encoded)
        {
            self.corrupt = true;
            return;
        }
        let mut tiles = std::mem::take(&mut self.tiles);
        tiles.clear();
        tiles.extend(
            encoded_points(&self.encoded)
                .filter(|(point, _)| self.bounds.is_none_or(|bounds| bounds.contains(*point)))
                .filter_map(|(_, tile)| morton_at_zoom(tile, self.zoom).ok()),
        );
        if self.count_mode == GeoTileCountMode::Documents {
            tiles.sort_unstable();
            tiles.dedup();
        }
        for tile in tiles.iter().copied() {
            self.increment(tile);
        }
        self.tiles = tiles;
    }

    fn harvest(self) -> Self::Fruit {
        GeoTileFruit {
            counts: self.counts,
            overflowed: self.overflowed,
            corrupt: self.corrupt,
        }
    }
}

impl GeoTileSegmentCollector {
    fn increment(&mut self, tile: u64) {
        if !self.counts.contains_key(&tile) && self.counts.len() >= self.max_buckets {
            self.overflowed = true;
            return;
        }
        *self.counts.entry(tile).or_default() += 1;
    }
}

pub(crate) fn collect_geo_tile_grid(
    searcher: &Searcher,
    query: &dyn Query,
    def: &TableDef,
    aggregation: &Aggregation,
) -> Result<Value> {
    let Aggregation::GeoTileGrid {
        column: column_name,
        zoom,
        max_buckets,
        count_mode,
        bounds,
    } = aggregation
    else {
        unreachable!("geo collector requires geo_tile_grid")
    };
    let column = column(def, column_name)?;
    ensure!(
        matches!(
            column.data_type,
            ColumnType::GeoPoint | ColumnType::GeoPointArray
        ),
        "geo_tile_grid requires a GEO_POINT or GEO_POINT[] column"
    );
    ensure!(*zoom <= GEO_MAX_ZOOM, "geo tile zoom must be in 0..=31");
    ensure!(
        (1..=100_000).contains(max_buckets),
        "geo max_buckets must be in 1..=100000"
    );
    let bounds = bounds.map(GeoBounds::validate).transpose()?;
    let collector = GeoTileGridCollector {
        coordinate_field: geo_coordinate_field(column_name),
        zoom: *zoom,
        max_buckets: *max_buckets,
        count_mode: *count_mode,
        bounds,
    };
    let counts = searcher.search(query, &collector)?;
    let buckets = counts
        .into_iter()
        .map(|(tile, document_count)| {
            let (x, y) = morton_xy(tile, *zoom)?;
            Ok(json!({
                "key": format!("{zoom}/{x}/{y}"),
                "x": x,
                "y": y,
                "doc_count": document_count
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "zoom": zoom,
        "count_mode": count_mode,
        "buckets": buckets
    }))
}

pub(crate) fn contains_geo_aggregation(aggregations: &BTreeMap<String, Aggregation>) -> bool {
    aggregations
        .values()
        .any(|aggregation| matches!(aggregation, Aggregation::GeoTileGrid { .. }))
}

fn bucket_limit_error(max_buckets: usize) -> TantivyError {
    TantivyError::InvalidArgument(format!(
        "geo tile aggregation exceeded max_buckets={max_buckets}"
    ))
}
