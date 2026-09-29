use std::time::Duration;

use super::testing::TestAuthority;
use super::*;

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("aseman-admin-http-{name}-{}", std::process::id()))
}

fn free_address() -> String {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}

fn echo_routes() -> Routes {
    Arc::new(|method, path, body| {
        (path == "/echo").then(|| (200, format!("{method} {}", body.len()).into_bytes()))
    })
}

#[test]
fn only_a_client_of_the_same_authority_with_the_token_is_served() {
    let directory = scratch("serve");
    let authority = TestAuthority::new("cluster");
    let server = MutualTls::load(&authority.identity(&directory, "server")).unwrap();
    let client = MutualTls::load(&authority.identity(&directory, "client")).unwrap();
    let stranger =
        MutualTls::load(&TestAuthority::new("other").identity(&directory, "stranger")).unwrap();
    let address = free_address();
    serve_routes(&address, &server, "secret", "test-admin", echo_routes()).unwrap();

    let https = client.blocking_http_client(Duration::from_secs(5)).unwrap();
    let url = format!(
        "https://localhost:{}/echo",
        address.rsplit(':').next().unwrap()
    );
    let served = https
        .post(&url)
        .bearer_auth("secret")
        .body("abc")
        .send()
        .unwrap();
    assert_eq!(served.status(), 200);
    assert_eq!(served.text().unwrap(), "POST 3");

    let wrong_token = https.post(&url).bearer_auth("guess").send().unwrap();
    assert_eq!(wrong_token.status(), 401);

    // A client whose certificate another authority issued never reaches a route.
    let refused = stranger
        .blocking_http_client(Duration::from_secs(5))
        .unwrap()
        .post(&url)
        .bearer_auth("secret")
        .send();
    assert!(refused.is_err());

    // Nor does plaintext.
    let plaintext = reqwest::blocking::Client::new()
        .post(format!("http://{address}/echo"))
        .bearer_auth("secret")
        .send();
    assert!(plaintext.is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_route_listener_needs_a_token() {
    let directory = scratch("token");
    let tls =
        MutualTls::load(&TestAuthority::new("cluster").identity(&directory, "server")).unwrap();
    assert!(serve_routes(&free_address(), &tls, "", "test-admin", echo_routes()).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn tokens_are_compared_whole() {
    assert!(token_matches("secret", "secret"));
    assert!(!token_matches("secre", "secret"));
    assert!(!token_matches("secretx", "secret"));
    assert!(!token_matches("", "secret"));
    assert!(!token_matches("Secret", "secret"));
}

#[test]
fn an_identity_whose_files_do_not_fit_is_refused() {
    let directory = scratch("files");
    let files = TestAuthority::new("cluster").identity(&directory, "server");
    let mut swapped = files.clone();
    swapped.key_secret = files.certificate.clone();
    assert!(MutualTls::load(&swapped).is_err());
    let mut missing = files;
    missing.ca = directory.join("absent.pem");
    assert!(MutualTls::load(&missing).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}
