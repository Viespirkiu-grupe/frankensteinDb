use std::collections::BTreeMap;

use super::geo_support::*;
use super::*;

#[test]
fn generic_terms_rejects_geo_columns() {
    let (_directory, database) = geo_database();
    let error = database
        .search_service()
        .unwrap()
        .aggregate(
            "places",
            None,
            BTreeMap::from([(
                "wrong".into(),
                Aggregation::Terms {
                    column: "location".into(),
                    size: 10,
                    segment_size: None,
                    min_doc_count: None,
                    missing: None,
                    order: None,
                    aggregations: BTreeMap::new(),
                },
            )]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("geo_tile_grid"));
}

#[test]
fn tabular_grouping_rejects_geo_columns() {
    let (_directory, mut database) = geo_database();
    let mut request = read("places", columns(&["location"]), None, vec![]);
    request.group_by = vec!["location".into()];
    let error = database.read(request).unwrap_err();
    assert!(error.to_string().contains("geo columns cannot be grouped"));
}

#[test]
fn top_hits_rejects_geo_distance_sort() {
    let (_directory, database) = geo_database();
    let child = Aggregation::TopHits {
        size: 2,
        sort: vec![geo_sort("location", VILNIUS, GeoDistanceMode::Min)],
        columns: vec!["id".into()],
    };
    let parent = Aggregation::Terms {
        column: "name".into(),
        size: 10,
        segment_size: None,
        min_doc_count: None,
        missing: None,
        order: None,
        aggregations: BTreeMap::from([("nearest".into(), child)]),
    };
    let error = database
        .search_service()
        .unwrap()
        .aggregate("places", None, BTreeMap::from([("names".into(), parent)]))
        .unwrap_err();
    assert!(error.to_string().contains("top_hits does not support geo"));
}

#[test]
fn geo_tile_grid_rejects_invalid_zoom_and_bucket_limits() {
    let (_directory, database) = geo_database();
    let search = database.search_service().unwrap();
    for (zoom, max_buckets) in [(32, 100), (0, 0), (0, 100_001)] {
        assert!(
            geo_tiles(
                &search,
                zoom,
                GeoTileCountMode::Documents,
                None,
                max_buckets
            )
            .is_err()
        );
    }
}

#[test]
fn radius_rejects_negative_and_non_finite_values() {
    let (_directory, mut database) = geo_database();
    for invalid in [-1.0, f64::NAN, f64::INFINITY] {
        let error = database
            .read(read(
                "places",
                columns(&["id"]),
                Some(radius("location", VILNIUS, invalid)),
                vec![],
            ))
            .unwrap_err();
        assert!(error.to_string().contains("radius"));
    }
}

#[test]
fn geo_array_enforces_point_count_limit() {
    let (_directory, database) = geo_database_without_rows();
    let too_many = vec![VILNIUS; 10_001];
    let row = vec![json!(99), json!("large"), json!(VILNIUS), json!(too_many)];
    let error = database.validate_json_row("places", &row).unwrap_err();
    assert!(error.to_string().contains("at most 10000 points"));
}

#[test]
fn geo_wire_types_use_expected_tagged_json() {
    let filter = serde_json::to_value(radius("location", VILNIUS, 42.0)).unwrap();
    assert_eq!(filter["kind"], json!("geo_distance"));
    assert_eq!(filter["center"], json!(VILNIUS));

    let aggregation: Aggregation = serde_json::from_value(json!({
        "kind": "geo_tile_grid",
        "column": "locations",
        "zoom": 31
    }))
    .unwrap();
    let Aggregation::GeoTileGrid {
        max_buckets,
        count_mode,
        bounds,
        ..
    } = aggregation
    else {
        panic!("wrong aggregation variant")
    };
    assert_eq!(max_buckets, 10_000);
    assert_eq!(count_mode, GeoTileCountMode::Documents);
    assert_eq!(bounds, None);
}

#[test]
fn geo_grid_can_share_a_request_with_standard_aggregations() {
    let (_directory, database) = geo_database();
    let aggregations = BTreeMap::from([
        (
            "count".into(),
            Aggregation::Metric {
                function: Metric::Count,
                column: Some("id".into()),
                json_path: None,
                percents: None,
                missing: None,
            },
        ),
        (
            "tiles".into(),
            Aggregation::GeoTileGrid {
                column: "location".into(),
                zoom: 0,
                max_buckets: 10,
                count_mode: GeoTileCountMode::Documents,
                bounds: None,
            },
        ),
    ]);
    let result = database
        .search_service()
        .unwrap()
        .aggregate("places", None, aggregations)
        .unwrap();
    assert_eq!(result["count"]["value"], json!(5.0));
    assert_eq!(result["tiles"]["buckets"][0]["doc_count"], json!(5));
}

#[test]
fn geo_grid_rejects_nesting_and_distributed_collection() {
    let (_directory, database) = geo_database();
    let search = database.search_service().unwrap();
    let geo = Aggregation::GeoTileGrid {
        column: "location".into(),
        zoom: 4,
        max_buckets: 100,
        count_mode: GeoTileCountMode::Documents,
        bounds: None,
    };
    let nested = Aggregation::Terms {
        column: "name".into(),
        size: 10,
        segment_size: None,
        min_doc_count: None,
        missing: None,
        order: None,
        aggregations: BTreeMap::from([("tiles".into(), geo.clone())]),
    };
    let error = search
        .aggregate("places", None, BTreeMap::from([("names".into(), nested)]))
        .unwrap_err();
    assert!(error.to_string().contains("top-level"));

    let error = search
        .aggregate_intermediate("places", None, BTreeMap::from([("tiles".into(), geo)]))
        .unwrap_err();
    assert!(error.to_string().contains("distributed"));
}
