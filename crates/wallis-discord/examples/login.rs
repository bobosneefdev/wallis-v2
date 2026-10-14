//! Connectivity check without credentials: a deliberately invalid token must be rejected by real Discord.
#[tokio::main]
async fn main() {
    let settings = wallis_discord::Settings { token: "invalid.token.value".into(), allowed_users: vec![], read_channels: vec![], profile: String::new() };
    println!("{:?}", wallis_discord::run(settings, |_| {}, |_| {}).await);
}
