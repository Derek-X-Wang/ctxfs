#[test]
fn authenticate_request_variant_exists() {
    use fskit_rs::protocol::{request, AuthenticateRequest, Request};
    let token = vec![0u8; 32];
    let req = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest { token })),
    };
    assert_eq!(req.id, 1);
    match req.content {
        Some(request::Content::Authenticate(a)) => assert_eq!(a.token.len(), 32),
        _ => panic!("expected Authenticate variant"),
    }
}
