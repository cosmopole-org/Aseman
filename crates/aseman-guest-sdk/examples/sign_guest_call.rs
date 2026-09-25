use aseman_guest_sdk::GuestClient;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let encoded = std::env::args()
        .nth(1)
        .ok_or("usage: sign_guest_call <workload-credential>")?;
    let client = GuestClient::from_credential(&encoded)?;
    let request = client.call("getProgram", br#"{"programId":"example"}"#.to_vec(), 0)?;
    println!("{} {}", request.method, request.url);
    Ok(())
}
