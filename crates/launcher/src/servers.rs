use kotor_protocol::master::ServerListing;
use std::time::Duration;

pub fn fetch(master_url: &str) -> anyhow::Result<Vec<ServerListing>> {
    let url = format!("{}/api/servers", master_url.trim_end_matches('/'));
    let list = ureq::get(&url)
        .call()?
        .into_body()
        .read_json::<Vec<ServerListing>>()?;
    Ok(list)
}

/// TCP connect time in ms, `None` if unreachable.
pub fn ping(address: &str) -> Option<u64> {
    kotor_protocol::client::tcp_ping(address, Duration::from_secs(2)).map(|d| d.as_millis() as u64)
}
