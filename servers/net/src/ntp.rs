//! SNTP client: set the kernel's wall clock (TIME_SET) from the network.
//! The firmware RTC is the fallback until this succeeds.

use alloc::string::String;
use alloc::vec;
use log::{info, warn};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint};
use touchos_rt::{exec, sys, time};

use crate::{resolve, wait_configured, with_net};

const NTP_UNIX_OFFSET: u64 = 2_208_988_800; // seconds from 1900 to 1970

async fn query(server: &str) -> Option<u64> {
    query_ip(resolve(server).await?).await
}

async fn query_ip(ip: IpAddress) -> Option<u64> {
    let handle = with_net(|n| {
        let mut s = udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 2], vec![0; 256]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 2], vec![0; 256]),
        );
        let port = n.ephemeral_port();
        s.bind(port).ok()?;
        Some(n.sockets.add(s))
    })??;
    let mut req = [0u8; 48];
    req[0] = 0x23; // LI 0, version 4, mode 3 (client)
    let ep = IpEndpoint::new(ip, 123);
    let sent = with_net(|n| {
        n.sockets
            .get_mut::<udp::Socket>(handle)
            .send_slice(&req, ep)
            .is_ok()
    })
    .unwrap_or(false);
    let mut result = None;
    if sent {
        let end = time::now_ms() + 3000;
        while time::now_ms() < end {
            let r = with_net(|n| {
                let s = n.sockets.get_mut::<udp::Socket>(handle);
                let mut buf = [0u8; 64];
                s.recv_slice(&mut buf).ok().map(|(len, _)| (len, buf))
            })
            .flatten();
            if let Some((len, b)) = r {
                if len >= 48 && b[0] & 7 == 4 && b[1] != 0 {
                    let secs = u32::from_be_bytes([b[40], b[41], b[42], b[43]]) as u64;
                    result = secs.checked_sub(NTP_UNIX_OFFSET);
                }
                break;
            }
            exec::sleep_ms(20).await;
        }
    }
    with_net(|n| n.sockets.remove(handle));
    result
}

/// Sync at startup and every 6 hours: from the time servers DHCP named
/// (option 42) first, then from `time.ntp_server` (read for every try;
/// time.google.com unless set). `kick`: the server setting changed: try
/// again now.
pub async fn task(kick: usize) {
    let Some(system) = touchos_rt::cap("system") else {
        warn!("ntp: no system capability; not setting the clock");
        return;
    };
    let mut retry = 10_000;
    loop {
        wait_configured().await;
        let setting: String = touchos_config::client::settings::get().time.ntp_server.clone();
        let from_dhcp = with_net(|n| n.ntp_servers.clone()).unwrap_or_default();
        let before = time::unix_now();
        let mut got = None;
        for ip in &from_dhcp {
            if let Some(t) = query_ip(IpAddress::Ipv4(*ip)).await {
                got = Some((t, alloc::format!("{ip} (from DHCP)")));
                break;
            }
            warn!("ntp: no answer from {ip} (from DHCP)");
        }
        if got.is_none() {
            got = query(&setting).await.map(|t| (t, setting.clone()));
        }
        let server = got.as_ref().map_or(setting.clone(), |(_, s)| s.clone());
        match got.map(|(t, _)| t) {
            Some(t) if sys::time_set(system, t).is_ok() => {
                retry = 10_000;
                info!(
                    "ntp: clock set from {server} (was off by {} s)",
                    t as i64 - before as i64
                );
                exec::wait_bits_or_ms(kick, 6 * 3600 * 1000).await;
            }
            _ => {
                // Right after DHCP the link is busy (the boot log goes out):
                // try again soon, then once a minute.
                warn!(
                    "ntp: no answer from {server}; retrying in {} s",
                    retry / 1000
                );
                if exec::wait_bits_or_ms(kick, retry).await == 0 {
                    retry = 60_000;
                } else {
                    retry = 10_000;
                }
            }
        }
    }
}
