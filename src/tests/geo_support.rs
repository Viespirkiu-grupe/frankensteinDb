use std::collections::BTreeMap;

use super::*;

pub(super) const VILNIUS: GeoPoint = GeoPoint {
    lat: 54.6872,
    lon: 25.2797,
};
pub(super) const KAUNAS: GeoPoint = GeoPoint {
    lat: 54.8985,
    lon: 23.9036,
};
pub(super) const KLAIPEDA: GeoPoint = GeoPoint {
    lat: 55.7033,
    lon: 21.1443,
};

pub(super) fn geo_database() -> (tempfile::TempDir, Database) {
    let (directory, mut database) = geo_database_without_rows();
    database
        .bulk_insert_json(
            "places",
            &[
                geo_row(1, "vilnius", VILNIUS, vec![VILNIUS, VILNIUS, KLAIPEDA]),
                geo_row(2, "kaunas", KAUNAS, vec![KAUNAS]),
                geo_row(
                    3,
                    "east",
                    GeoPoint {
                        lat: 10.0,
                        lon: 179.8,
                    },
                    vec![GeoPoint {
                        lat: 10.0,
                        lon: 179.8,
                    }],
                ),
                geo_row(
                    4,
                    "west",
                    GeoPoint {
                        lat: 10.0,
                        lon: -179.8,
                    },
                    vec![GeoPoint {
                        lat: 10.0,
                        lon: -179.8,
                    }],
                ),
                geo_row(
                    5,
                    "north",
                    GeoPoint {
                        lat: 89.0,
                        lon: 0.0,
                    },
                    vec![GeoPoint {
                        lat: 89.0,
                        lon: 0.0,
                    }],
                ),
            ],
        )
        .unwrap();
    (directory, database)
}

pub(super) fn geo_database_without_rows() -> (tempfile::TempDir, Database) {
    let (directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "places".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column("name", ColumnType::Text, false, false, Some(Analyzer::Raw)),
                test_column("location", ColumnType::GeoPoint, false, false, None),
                test_column("locations", ColumnType::GeoPointArray, false, false, None),
            ],
        })
        .unwrap();
    (directory, database)
}

fn geo_row(id: i64, name: &str, location: GeoPoint, locations: Vec<GeoPoint>) -> Vec<Value> {
    vec![json!(id), json!(name), json!(location), json!(locations)]
}

pub(super) fn radius(column: &str, center: GeoPoint, radius_meters: f64) -> Filter {
    Filter::GeoDistance {
        column: column.into(),
        center,
        radius_meters,
    }
}

pub(super) fn id_sort() -> Sort {
    Sort {
        column: "id".into(),
        json_path: None,
        json_type: None,
        descending: false,
        geo_distance_from: None,
        geo_distance_mode: GeoDistanceMode::Min,
    }
}

pub(super) fn geo_sort(column: &str, from: GeoPoint, mode: GeoDistanceMode) -> Sort {
    Sort {
        column: column.into(),
        json_path: None,
        json_type: None,
        descending: false,
        geo_distance_from: Some(from),
        geo_distance_mode: mode,
    }
}

pub(super) fn geo_tiles(
    search: &SearchService,
    zoom: u8,
    count_mode: GeoTileCountMode,
    bounds: Option<GeoBounds>,
    max_buckets: usize,
) -> Result<Value> {
    search.aggregate(
        "places",
        None,
        BTreeMap::from([(
            "heatmap".into(),
            Aggregation::GeoTileGrid {
                column: "locations".into(),
                zoom,
                max_buckets,
                count_mode,
                bounds,
            },
        )]),
    )
}
