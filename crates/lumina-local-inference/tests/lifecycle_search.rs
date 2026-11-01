use lumina_local_inference::lifecycle::*;
use serde_json::json;

fn request(page_number: u32, page_size: u32) -> CatalogPageRequest {
    CatalogPageRequest {
        query: "Wan GGUF".into(),
        page_number,
        page_size,
    }
}

fn response(ids: &[&str], total: u64, page: u32, size: u32) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "success": true,
        "data": {
            "models": ids.iter().map(|id| json!({"id":id})).collect::<Vec<_>>(),
            "total_count":total,"page_number":page,"page_size":size,
        }
    }))
    .unwrap()
}

#[test]
fn paginated_catalog_uses_reported_totals_and_preserves_unknown_fields() {
    let first = parse_modelscope_catalog_page(
        &response(&["owner/first", "owner/second"], 3, 1, 2),
        request(1, 2),
    )
    .unwrap();
    assert!(first.has_more());
    assert_eq!(first.models[0].downloads, None);
    assert_eq!(first.models[0].private, None);
    assert_eq!(first.models[0].gated, None);
    let next = first.next_request().unwrap();
    assert_eq!(next.page_number, 2);
    let last = parse_modelscope_catalog_page(&response(&["owner/third"], 3, 2, 2), next).unwrap();
    assert!(!last.has_more());
    assert_eq!(last.next_request(), None);
}

#[test]
fn service_window_limit_does_not_claim_a_complete_catalog() {
    let ids = (0..50)
        .map(|index| format!("owner/model-{index}"))
        .collect::<Vec<_>>();
    let refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let page =
        parse_modelscope_catalog_page(&response(&refs, 4000, 60, 50), request(60, 50)).unwrap();
    assert!(page.has_more());
    assert_eq!(page.next_request(), None);
    for req in [
        request(0, 50),
        request(1, 0),
        request(1, 51),
        request(61, 50),
        request(u32::MAX, 50),
    ] {
        assert!(req.validate().is_err());
    }
}

#[test]
fn incomplete_wrong_page_and_duplicate_catalog_results_are_failures() {
    for body in [
        response(&["owner/first"], 3, 1, 2),
        response(&["owner/first", "owner/second"], 3, 2, 2),
        response(&["owner/first", "owner/second"], 3, 1, 3),
        response(&["owner/same", "owner/same"], 2, 1, 2),
        response(&["../not-a-repository"], 1, 1, 2),
        br#"{"success":true,"data":{"models":[]}}"#.to_vec(),
        br#"{"success":false,"message":"private-server-message"}"#.to_vec(),
        br#"{"success":true}"#.to_vec(),
    ] {
        let error = parse_modelscope_catalog_page(&body, request(1, 2)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<ModelSourceError>().unwrap().code,
            LifecycleErrorCode::SourcePartialFailure
        );
        assert!(!format!("{error:?}").contains("private-server-message"));
    }
}

#[test]
fn actual_empty_catalog_is_separate_from_an_invalid_response() {
    let page = parse_modelscope_catalog_page(&response(&[], 0, 1, 2), request(1, 2)).unwrap();
    assert_eq!(page.total_count, 0);
    assert!(page.models.is_empty());
    assert!(!page.has_more());
    assert!(
        parse_modelscope_catalog_page(&vec![b' '; 4 * 1024 * 1024 + 1], request(1, 2)).is_err()
    );
}

#[test]
fn restricted_catalog_entries_remain_observed_without_access_claims() {
    let mut data: serde_json::Value =
        serde_json::from_slice(&response(&["owner/gated"], 1, 1, 2)).unwrap();
    data["data"]["models"][0]["gated"] = json!(true);
    data["data"]["models"][0]["private"] = json!(true);
    let page =
        parse_modelscope_catalog_page(&serde_json::to_vec(&data).unwrap(), request(1, 2)).unwrap();
    assert_eq!(page.models.len(), 1);
    assert_eq!(page.models[0].gated, Some(true));
    assert_eq!(page.models[0].private, Some(true));
}

#[test]
fn five_search_kinds_preserve_user_terms_and_do_not_create_repository_ids() {
    for kind in [
        ModelSearchKind::Name,
        ModelSearchKind::Series,
        ModelSearchKind::Architecture,
        ModelSearchKind::Quantization,
        ModelSearchKind::Component,
    ] {
        let query = ModelSearchQuery {
            text: " Qwen3  GGUF ".into(),
            kind,
        };
        assert_eq!(query.terms().unwrap(), vec!["Qwen3 GGUF", "Qwen3"]);
        assert_eq!(query.exact_repository(), None);
    }
    let query = ModelSearchQuery {
        text: "actual-owner/actual-model".into(),
        kind: ModelSearchKind::Name,
    };
    assert_eq!(query.exact_repository().unwrap().as_str(), query.text);
    assert_eq!(
        query.terms().unwrap(),
        vec![
            "actual-owner/actual-model GGUF",
            "actual-owner/actual-model"
        ]
    );
    for text in ["", "\nquery", "bad\0query"] {
        assert!(ModelSearchQuery {
            text: text.into(),
            kind: ModelSearchKind::Name
        }
        .terms()
        .is_err());
    }
}

#[test]
fn discovery_limits_bound_network_work_before_any_request() {
    let mut request = ModelDiscoveryRequest {
        query: ModelSearchQuery {
            text: "Qwen3".into(),
            kind: ModelSearchKind::Name,
        },
        limit: 20,
        page_budget: 4,
        verification_limit: 8,
    };
    request.validate().unwrap();
    request.verification_limit = 17;
    assert!(request.validate().is_err());
    request.verification_limit = 8;
    request.limit = 7;
    assert!(request.validate().is_err());
    request.limit = 101;
    assert!(request.validate().is_err());
    request.limit = 20;
    request.page_budget = 13;
    assert!(request.validate().is_err());
    request.page_budget = 1;
    request.query.text = "x".repeat(512);
    assert!(request.validate().is_err());
}
