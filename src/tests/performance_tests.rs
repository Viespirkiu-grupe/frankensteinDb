use super::geo_support::{VILNIUS, geo_database, geo_sort};
use super::*;

#[test]
fn field_sort_with_score_projection_uses_bounded_scored_collector() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mut request = read(
        "products",
        vec![
            Projection::Column {
                column: "id".into(),
                alias: None,
            },
            Projection::Score { alias: None },
        ],
        Some(Filter::Search {
            fields: vec!["title".into()],
            query: "wireless".into(),
        }),
        vec![Sort {
            column: "price".into(),
            json_path: None,
            json_type: None,
            descending: false,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
    );
    request.limit = 1;

    let plan = database.explain(&request).unwrap();
    let collector = plan
        .columns
        .iter()
        .position(|column| column == "collector")
        .unwrap();
    assert_eq!(plan.rows[0][collector], json!("scored_fast_field_top_docs"));

    let result = database.read(request.clone()).unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], json!(3));
    assert!(result.rows[0][1].as_f64().unwrap() > 0.0);

    request.min_score = Some(f32::MAX);
    assert!(database.read(request).unwrap().rows.is_empty());
}

#[test]
fn typed_group_orders_are_pushed_down_without_changing_results() {
    let (_directory, mut database) = database();
    products(&mut database);
    let grouped = |order_column: &str, descending: bool| ReadRequest {
        table: "products".into(),
        projection: vec![
            Projection::Column {
                column: "category".into(),
                alias: None,
            },
            Projection::Aggregate {
                function: Aggregate::Count,
                column: None,
                alias: "count".into(),
            },
            Projection::Aggregate {
                function: Aggregate::Average,
                column: Some("price".into()),
                alias: "average".into(),
            },
        ],
        filter: None,
        group_by: vec!["category".into()],
        order_by: vec![Sort {
            column: order_column.into(),
            json_path: None,
            json_type: None,
            descending,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
        limit: 1,
        offset: 0,
        search_after: None,
        min_score: None,
    };

    let by_count = database.read(grouped("count", false)).unwrap();
    assert_eq!(
        by_count.rows,
        vec![vec![json!("computers"), json!(1), json!(45.0)]]
    );

    let by_key = database.read(grouped("category", true)).unwrap();
    assert_eq!(by_key.rows[0][0], json!("computers"));

    let by_average = database.read(grouped("average", false)).unwrap();
    assert_eq!(by_average.rows[0][0], json!("computers"));
}

#[test]
fn geo_distance_sort_uses_block_top_k() {
    let (_directory, mut database) = geo_database();
    let request = read(
        "places",
        columns(&["id"]),
        None,
        vec![geo_sort("location", VILNIUS, GeoDistanceMode::Min)],
    );
    let plan = database.explain(&request).unwrap();
    let collector = plan
        .columns
        .iter()
        .position(|column| column == "collector")
        .unwrap();
    assert_eq!(plan.rows[0][collector], json!("block_fast_field_top_docs"));
    assert_eq!(database.read(request).unwrap().rows[0][0], json!(1));
}
