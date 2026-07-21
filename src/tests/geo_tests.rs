use std::collections::BTreeMap;

use super::geo_support::*;
use super::*;

#[test]
fn geo_points_round_trip_exclusively_through_tantivy() {
    let (_directory, mut database) = geo_database();
    let result = database
        .read(read(
            "places",
            columns(&["id", "location", "locations"]),
            None,
            vec![id_sort()],
        ))
        .unwrap();
    assert_eq!(result.rows.len(), 5);
    assert_eq!(result.rows[0][1], json!(VILNIUS));
    assert_eq!(result.rows[0][2], json!([VILNIUS, VILNIUS, KLAIPEDA]));
    assert_eq!(result.rows[1][1], json!(KAUNAS));
}

#[test]
fn geo_validation_rejects_invalid_coordinates_and_shapes() {
    let (_directory, database) = geo_database_without_rows();
    for invalid in [
        json!({"lat": 90.1, "lon": 0.0}),
        json!({"lat": -90.1, "lon": 0.0}),
        json!({"lat": 0.0, "lon": 180.1}),
        json!({"lat": 0.0, "lon": -180.1}),
        json!({"lat": 1.0}),
        json!({"lat": 1.0, "lon": 2.0, "alt": 3.0}),
    ] {
        let row = vec![json!(99), json!("invalid"), invalid, json!([])];
        assert!(database.validate_json_row("places", &row).is_err());
    }
    assert!(
        GeoPoint {
            lat: f64::NAN,
            lon: 0.0
        }
        .validate()
        .is_err()
    );
    assert!(
        GeoPoint {
            lat: 0.0,
            lon: f64::INFINITY
        }
        .validate()
        .is_err()
    );
}

#[test]
fn radius_filter_is_exact_and_arrays_use_any_semantics() {
    let (_directory, mut database) = geo_database();
    let scalar = database
        .read(read(
            "places",
            columns(&["id"]),
            Some(radius("location", VILNIUS, 100_000.0)),
            vec![id_sort()],
        ))
        .unwrap();
    assert_eq!(scalar.rows, vec![vec![json!(1)], vec![json!(2)]]);

    let array = database
        .read(read(
            "places",
            columns(&["id"]),
            Some(radius("locations", KLAIPEDA, 1_000.0)),
            vec![id_sort()],
        ))
        .unwrap();
    assert_eq!(array.rows, vec![vec![json!(1)]]);

    let zero = database
        .read(read(
            "places",
            columns(&["id"]),
            Some(radius("location", VILNIUS, 0.0)),
            vec![id_sort()],
        ))
        .unwrap();
    assert_eq!(zero.rows, vec![vec![json!(1)]]);
}

#[test]
fn earth_spanning_radius_includes_every_document() {
    let (_directory, mut database) = geo_database();
    let result = database
        .read(read(
            "places",
            columns(&["id"]),
            Some(radius(
                "location",
                VILNIUS,
                std::f64::consts::PI * 6_371_008.8,
            )),
            vec![id_sort()],
        ))
        .unwrap();
    assert_eq!(result.rows.len(), 5);
}

#[test]
fn bounding_boxes_handle_antimeridian_and_array_pairing() {
    let (_directory, mut database) = geo_database();
    let crossing = GeoBounds {
        top_left: GeoPoint {
            lat: 20.0,
            lon: 170.0,
        },
        bottom_right: GeoPoint {
            lat: 0.0,
            lon: -170.0,
        },
    };
    let result = database
        .read(read(
            "places",
            columns(&["id"]),
            Some(Filter::GeoBoundingBox {
                column: "location".into(),
                bounds: crossing,
            }),
            vec![id_sort()],
        ))
        .unwrap();
    assert_eq!(result.rows, vec![vec![json!(3)], vec![json!(4)]]);

    let invalid = GeoBounds {
        top_left: GeoPoint {
            lat: -10.0,
            lon: 0.0,
        },
        bottom_right: GeoPoint {
            lat: 10.0,
            lon: 1.0,
        },
    };
    assert!(
        database
            .read(read(
                "places",
                columns(&["id"]),
                Some(Filter::GeoBoundingBox {
                    column: "locations".into(),
                    bounds: invalid,
                }),
                vec![],
            ))
            .is_err()
    );
}

#[test]
fn known_city_distance_and_projection_are_accurate() {
    let distance = haversine_distance_meters(VILNIUS, KAUNAS);
    assert!((distance - 91_200.0).abs() < 2_000.0, "distance={distance}");

    let (_directory, mut database) = geo_database();
    let result = database
        .read(read(
            "places",
            vec![
                Projection::Column {
                    column: "id".into(),
                    alias: None,
                },
                Projection::GeoDistance {
                    column: "location".into(),
                    from: VILNIUS,
                    mode: GeoDistanceMode::Min,
                    alias: Some("meters".into()),
                },
            ],
            Some(equal("id", json!(2))),
            vec![],
        ))
        .unwrap();
    let projected = result.rows[0][1].as_f64().unwrap();
    assert!((projected - distance).abs() < 0.001);
}

#[test]
fn distance_sort_supports_scalar_and_array_reductions() {
    let (_directory, mut database) = geo_database();
    let nearest = database
        .read(read(
            "places",
            columns(&["id"]),
            None,
            vec![geo_sort("location", VILNIUS, GeoDistanceMode::Min)],
        ))
        .unwrap();
    assert_eq!(nearest.rows[0][0], json!(1));
    assert_eq!(nearest.rows[1][0], json!(2));

    let farthest_array = database
        .read(read(
            "places",
            columns(&["id"]),
            Some(Filter::In {
                column: "id".into(),
                values: vec![json!(1), json!(2)],
            }),
            vec![geo_sort("locations", VILNIUS, GeoDistanceMode::Max)],
        ))
        .unwrap();
    assert_eq!(farthest_array.rows, vec![vec![json!(2)], vec![json!(1)]]);
}

#[test]
fn geo_distance_sort_has_stable_search_after_pages() {
    let (_directory, mut database) = geo_database();
    let mut request = read(
        "places",
        columns(&["id"]),
        None,
        vec![geo_sort("location", VILNIUS, GeoDistanceMode::Min)],
    );
    request.limit = 1;
    let first = database.read(request.clone()).unwrap();
    assert_eq!(first.rows, vec![vec![json!(1)]]);
    assert_eq!(first.next_search_after.as_ref().unwrap().len(), 2);
    assert_eq!(first.next_search_after.as_ref().unwrap()[0], json!(0.0));

    request.search_after = first.next_search_after;
    let second = database.read(request).unwrap();
    assert_eq!(second.rows, vec![vec![json!(2)]]);
}

#[test]
fn geo_tile_grid_supports_dynamic_zoom_and_count_modes() {
    let (_directory, database) = geo_database();
    let search = database.search_service().unwrap();
    let documents = geo_tiles(&search, 0, GeoTileCountMode::Documents, None, 100).unwrap();
    assert_eq!(documents["heatmap"]["buckets"][0]["key"], json!("0/0/0"));
    assert_eq!(documents["heatmap"]["buckets"][0]["doc_count"], json!(5));

    let points = geo_tiles(&search, 0, GeoTileCountMode::Points, None, 100).unwrap();
    assert_eq!(points["heatmap"]["buckets"][0]["doc_count"], json!(7));

    let detailed = geo_tiles(&search, 31, GeoTileCountMode::Points, None, 100).unwrap();
    assert_eq!(detailed["heatmap"]["zoom"], json!(31));
    assert!(detailed["heatmap"]["buckets"].as_array().unwrap().len() >= 5);
}

#[test]
fn geo_tile_grid_applies_search_filter_bounds_and_bucket_limit() {
    let (_directory, database) = geo_database();
    let search = database.search_service().unwrap();
    let bounds = GeoBounds {
        top_left: GeoPoint {
            lat: 56.0,
            lon: 20.0,
        },
        bottom_right: GeoPoint {
            lat: 53.0,
            lon: 27.0,
        },
    };
    let aggregations = BTreeMap::from([(
        "heatmap".into(),
        Aggregation::GeoTileGrid {
            column: "locations".into(),
            zoom: 0,
            max_buckets: 100,
            count_mode: GeoTileCountMode::Points,
            bounds: Some(bounds),
        },
    )]);
    let result = search
        .aggregate(
            "places",
            Some(&Filter::Compare {
                column: "id".into(),
                operator: Comparison::LessOrEqual,
                value: json!(2),
            }),
            aggregations,
        )
        .unwrap();
    assert_eq!(result["heatmap"]["buckets"][0]["doc_count"], json!(4));

    let error = geo_tiles(&search, 31, GeoTileCountMode::Points, None, 1).unwrap_err();
    assert!(error.to_string().contains("max_buckets=1"));
}

#[test]
fn zoom_31_morton_prefixes_decode_to_xyz_parents() {
    let high = crate::geo::morton_z31(VILNIUS);
    let z31 = crate::geo::morton_at_zoom(high, 31).unwrap();
    let z20 = crate::geo::morton_at_zoom(high, 20).unwrap();
    let z1 = crate::geo::morton_at_zoom(high, 1).unwrap();
    assert_eq!(z31, high);
    assert_eq!(z20, high >> 22);
    assert_eq!(z1, high >> 60);
    assert_eq!(crate::geo::morton_at_zoom(high, 0).unwrap(), 0);
    assert!(crate::geo::morton_at_zoom(high, 32).is_err());

    let (x31, y31) = crate::geo::morton_xy(z31, 31).unwrap();
    let (x20, y20) = crate::geo::morton_xy(z20, 20).unwrap();
    assert_eq!(x20, x31 >> 11);
    assert_eq!(y20, y31 >> 11);
}

#[test]
fn poles_and_longitude_180_have_valid_zoom_31_tiles() {
    for point in [
        GeoPoint {
            lat: 90.0,
            lon: 0.0,
        },
        GeoPoint {
            lat: -90.0,
            lon: 0.0,
        },
        GeoPoint {
            lat: 0.0,
            lon: 180.0,
        },
        GeoPoint {
            lat: 0.0,
            lon: -180.0,
        },
    ] {
        let tile = crate::geo::morton_z31(point);
        let (x, y) = crate::geo::morton_xy(tile, 31).unwrap();
        assert!(x < (1u32 << 31));
        assert!(y < (1u32 << 31));
    }
    assert_eq!(
        crate::geo::morton_z31(GeoPoint {
            lat: 0.0,
            lon: 180.0
        }),
        crate::geo::morton_z31(GeoPoint {
            lat: 0.0,
            lon: -180.0
        })
    );
}

#[test]
fn geo_mutation_reindexes_distance_and_tile_values() {
    let (_directory, mut database) = geo_database();
    database
        .mutate_typed(Mutation::Update {
            table: "places".into(),
            values: BTreeMap::from([("location".into(), json!(VILNIUS))]),
            filter: equal("id", json!(2)),
        })
        .unwrap();
    let exact = database
        .read(read(
            "places",
            columns(&["id"]),
            Some(radius("location", VILNIUS, 0.0)),
            vec![id_sort()],
        ))
        .unwrap();
    assert_eq!(exact.rows, vec![vec![json!(1)], vec![json!(2)]]);
}

#[test]
fn geo_operations_reject_wrong_columns_and_unindexed_schema() {
    let (_directory, mut database) = geo_database();
    assert!(
        database
            .read(read(
                "places",
                columns(&["id"]),
                Some(radius("name", VILNIUS, 100.0)),
                vec![],
            ))
            .is_err()
    );

    let mut location = test_column("location", ColumnType::GeoPoint, false, false, None);
    location.index.indexed = false;
    assert!(
        database
            .create_table_def(TableDef {
                name: "bad_geo".into(),
                aliases: vec![],
                document_store: Default::default(),
                columns: vec![
                    test_column("id", ColumnType::Integer, true, false, None),
                    location,
                ],
            })
            .is_err()
    );
}
