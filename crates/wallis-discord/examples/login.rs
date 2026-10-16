//! Connectivity check without credentials: a deliberately invalid token must be rejected by real Discord.
#[tokio::main]
async fn main() {
    let settings = wallis_discord::Settings { data_dir: std::env::temp_dir(), write_channels: vec![], token: "invalid.token.value".into(), allowed_users: vec![], read_channels: vec![], profile: std::sync::Arc::new(|| None) };
    println!("{:?}", wallis_discord::run(settings, |_| {}, |_| {}).await);
}
