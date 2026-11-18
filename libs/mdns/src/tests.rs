use super::*;
use alloc::string::ToString;
use wire::{parse, put_name};

const IP: [u8; 4] = [192, 168, 1, 50];
const PEER: [u8; 4] = [192, 168, 1, 20];

fn n(s: &str) -> Name {
    s.split('.').map(|l| l.as_bytes().to_vec()).collect()
}

fn name_rdata(s: &str) -> Vec<u8> {
    let mut v = Vec::new();
    put_name(&mut v, &n(s));
    v
}

fn http() -> Service {
    Service { kind: "_http._tcp".into(), instance: String::new(), port: 80, txt: vec!["path=/".into()] }
}

fn query(id: u16, qs: &[(&str, u16, bool)]) -> Vec<u8> {
    let mut w = Writer::new(id, 0);
    for &(name, qtype, unicast) in qs {
        w.question(&Question { name: n(name), qtype, class: CLASS_IN, unicast });
    }
    w.finish()
}

/// Run the responder until its name is ours; returns the time then.
fn ready(r: &mut Responder) -> u64 {
    r.set_ip(Some(IP), 0);
    let mut now = 0;
    for _ in 0..20 {
        while r.poll(now).is_some() {}
        if r.poll_at().is_none() {
            return now;
        }
        now = r.poll_at().unwrap().max(now);
    }
    panic!("not ready");
}

fn answer_of(p: &Packet) -> Message {
    parse(&p.data).unwrap()
}

#[test]
fn probes_then_announces() {
    let mut r = Responder::new("touchos-1a2b");
    assert_eq!(r.poll_at(), None, "no address, nothing to do");
    r.set_ip(Some(IP), 1000);
    let mut sent = Vec::new();
    let mut now = 1000;
    while let Some(at) = r.poll_at() {
        now = at.max(now);
        if let Some(p) = r.poll(now) {
            sent.push((now, answer_of(&p)));
        }
    }
    assert_eq!(sent.len(), 5);
    let first = sent[0].0;
    assert!((1000..1250).contains(&first));
    for (i, (t, m)) in sent[..3].iter().enumerate() {
        assert_eq!(*t, first + 250 * i as u64);
        assert!(!m.is_response());
        assert_eq!(m.questions[0].name, n("touchos-1a2b.local"));
        assert_eq!(m.questions[0].qtype, TYPE_ANY);
        assert!(m.questions[0].unicast);
        assert_eq!(m.authority[0].rdata, IP);
    }
    // Announcements: 250 ms after the last probe, then a second later.
    assert_eq!(sent[3].0, first + 750);
    assert_eq!(sent[4].0, first + 1750);
    let a = &sent[3].1;
    assert!(a.is_response());
    assert!(a.answers.iter().any(|x| x.rtype == TYPE_A && x.flush && x.rdata == IP));
    assert!(r.is_ready());
}

#[test]
fn quiet_while_probing() {
    let mut r = Responder::new("touchos-1a2b");
    r.set_ip(Some(IP), 0);
    let q = query(0, &[("touchos-1a2b.local", TYPE_A, false)]);
    assert_eq!(r.on_packet(PEER, PORT, &q, 0), None);
    ready(&mut r);
    assert!(r.on_packet(PEER, PORT, &q, 5000).is_some());
}

#[test]
fn answers_for_the_host() {
    let mut r = Responder::new("touchos-1a2b");
    let t = ready(&mut r);
    // A multicast question gets a multicast answer, with the NSEC along.
    let p = r.on_packet(PEER, PORT, &query(0, &[("TouchOS-1A2B.local", TYPE_A, false)]), t).unwrap();
    assert_eq!(p.dest, Dest::Multicast);
    let m = answer_of(&p);
    assert_eq!((m.id, m.flags, m.questions.len()), (0, FLAGS_RESPONSE, 0));
    assert_eq!(m.answers.len(), 1);
    assert_eq!((m.answers[0].rtype, m.answers[0].ttl, m.answers[0].flush), (TYPE_A, 120, true));
    assert_eq!(m.answers[0].rdata, IP);
    assert_eq!(m.additional[0].rtype, TYPE_NSEC);
    // QU: straight back to the asker.
    let p = r.on_packet(PEER, PORT, &query(0, &[("touchos-1a2b.local", TYPE_A, true)]), t).unwrap();
    assert_eq!(p.dest, Dest::Unicast(PEER, PORT));
    // No AAAA: the NSEC says so.
    let m = answer_of(&r.on_packet(PEER, PORT, &query(0, &[("touchos-1a2b.local", TYPE_AAAA, false)]), t).unwrap());
    assert_eq!(m.answers[0].rtype, TYPE_NSEC);
    assert_eq!(m.answers[0].rdata, [name_rdata("touchos-1a2b.local"), vec![0, 1, 0x40]].concat());
    // The reverse name.
    let m = answer_of(&r.on_packet(PEER, PORT, &query(0, &[("50.1.168.192.in-addr.arpa", TYPE_PTR, false)]), t).unwrap());
    assert_eq!(m.answers[0].rdata, name_rdata("touchos-1a2b.local"));
    // Other names and types: nothing.
    assert_eq!(r.on_packet(PEER, PORT, &query(0, &[("other.local", TYPE_A, false)]), t), None);
    assert_eq!(r.on_packet(PEER, PORT, &query(0, &[("touchos-1a2b.local", TYPE_TXT, false)]), t), None);
}

#[test]
fn legacy_unicast() {
    let mut r = Responder::new("touchos-1a2b");
    let t = ready(&mut r);
    let p = r.on_packet(PEER, 40000, &query(0xbeef, &[("touchos-1a2b.local", TYPE_A, false)]), t).unwrap();
    assert_eq!(p.dest, Dest::Unicast(PEER, 40000));
    let m = answer_of(&p);
    assert_eq!(m.id, 0xbeef);
    assert_eq!(m.questions[0].name, n("touchos-1a2b.local"));
    assert_eq!((m.answers[0].ttl, m.answers[0].flush), (10, false));
}

#[test]
fn services() {
    let mut r = Responder::new("touchos-1a2b");
    r.add_service(http(), 0);
    let t = ready(&mut r);
    // Browsing: the PTR, with SRV, TXT, A and NSEC along.
    let m = answer_of(&r.on_packet(PEER, PORT, &query(0, &[("_http._tcp.local", TYPE_PTR, false)]), t).unwrap());
    assert_eq!(m.answers.len(), 1);
    assert_eq!(m.answers[0].rdata, name_rdata("touchos-1a2b._http._tcp.local"));
    assert_eq!((m.answers[0].ttl, m.answers[0].flush), (4500, false));
    let types: Vec<u16> = m.additional.iter().map(|x| x.rtype).collect();
    assert_eq!(types, vec![TYPE_SRV, TYPE_TXT, TYPE_A, TYPE_NSEC]);
    let srv = &m.additional[0];
    assert_eq!(srv.name, n("touchos-1a2b._http._tcp.local"));
    assert_eq!(srv.rdata, [vec![0, 0, 0, 0, 0, 80], name_rdata("touchos-1a2b.local")].concat());
    assert_eq!(m.additional[1].rdata, b"\x06path=/");
    // The list of service types.
    let m = answer_of(&r.on_packet(PEER, PORT, &query(0, &[("_services._dns-sd._udp.local", TYPE_PTR, false)]), t).unwrap());
    assert_eq!(m.answers[0].rdata, name_rdata("_http._tcp.local"));
    // A named instance, and the same type and port again replaces it.
    let named = Service { instance: "Kitchen panel".into(), ..http() };
    r.add_service(named, t);
    assert_eq!(r.services().len(), 1);
    let m = answer_of(&r.on_packet(PEER, PORT, &query(0, &[("_http._tcp.local", TYPE_PTR, false)]), t).unwrap());
    assert_eq!(m.answers[0].rdata, name_rdata("Kitchen panel._http._tcp.local"));
    // A new service is announced again.
    assert!(r.poll_at().is_some());
}

#[test]
fn known_answers_are_left_out() {
    let mut r = Responder::new("touchos-1a2b");
    r.add_service(http(), 0);
    let t = ready(&mut r);
    let mut w = Writer::new(0, 0);
    w.question(&Question { name: n("_http._tcp.local"), qtype: TYPE_PTR, class: CLASS_IN, unicast: false });
    let known = |ttl| Record {
        name: n("_http._tcp.local"),
        rtype: TYPE_PTR,
        class: CLASS_IN,
        flush: false,
        ttl,
        rdata: name_rdata("TOUCHOS-1A2B._http._tcp.local"),
    };
    let mut w2 = Writer::new(0, 0);
    w2.question(&Question { name: n("_http._tcp.local"), qtype: TYPE_PTR, class: CLASS_IN, unicast: false });
    w.record(1, &known(4000));
    w2.record(1, &known(1000));
    assert_eq!(r.on_packet(PEER, PORT, &w.finish(), t), None, "fresh enough");
    assert!(r.on_packet(PEER, PORT, &w2.finish(), t).is_some(), "less than half the TTL left");
}

#[test]
fn removed_service_says_goodbye() {
    let mut r = Responder::new("touchos-1a2b");
    r.add_service(http(), 0);
    let t = ready(&mut r);
    r.remove_service("_http._tcp", 80);
    assert!(r.services().is_empty());
    let m = answer_of(&r.poll(t).unwrap());
    assert_eq!((m.answers[0].rtype, m.answers[0].ttl), (TYPE_PTR, 0));
    assert_eq!(r.on_packet(PEER, PORT, &query(0, &[("_http._tcp.local", TYPE_PTR, false)]), t), None);
}

fn response_claiming(name: &str, ip: [u8; 4]) -> Vec<u8> {
    let mut w = Writer::new(0, FLAGS_RESPONSE);
    w.record(1, &Record { name: n(name), rtype: TYPE_A, class: CLASS_IN, flush: true, ttl: 120, rdata: ip.to_vec() });
    w.finish()
}

#[test]
fn conflict_renames() {
    let mut r = Responder::new("touchos-1a2b");
    let t = ready(&mut r);
    // Our own address for our name is no conflict.
    r.on_packet(PEER, PORT, &response_claiming("touchos-1a2b.local", IP), t);
    assert_eq!(r.host(), "touchos-1a2b");
    // Another address is.
    r.on_packet(PEER, PORT, &response_claiming("touchos-1a2b.local", PEER), t);
    assert_eq!(r.host(), "touchos-1a2b-2");
    assert!(!r.is_ready());
    // While probing for the new name, an answer for it: -3.
    r.on_packet(PEER, PORT, &response_claiming("touchos-1a2b-2.local", PEER), t);
    assert_eq!(r.host(), "touchos-1a2b-3");
    // A long name keeps 63 bytes.
    let mut r = Responder::new(&"a".repeat(63));
    let t = ready(&mut r);
    r.on_packet(PEER, PORT, &response_claiming(&alloc::format!("{}.local", "a".repeat(63)), PEER), t);
    assert_eq!(r.host(), alloc::format!("{}-2", "a".repeat(61)));
}

#[test]
fn many_conflicts_slow_down() {
    let mut r = Responder::new("p");
    let t = ready(&mut r);
    for _ in 0..16 {
        let host = alloc::format!("{}.local", r.host());
        r.on_packet(PEER, PORT, &response_claiming(&host, PEER), t);
    }
    assert_eq!(r.host(), "p-17");
    assert!(r.poll_at().unwrap() >= t + CONFLICT_PAUSE_MS);
}

#[test]
fn our_own_packets_are_ignored() {
    let mut r = Responder::new("touchos-1a2b");
    let t = ready(&mut r);
    // The access point sends our announcements back to us.
    r.on_packet(IP, PORT, &response_claiming("touchos-1a2b.local", PEER), t);
    assert_eq!(r.host(), "touchos-1a2b");
    assert_eq!(r.on_packet(IP, PORT, &query(0, &[("touchos-1a2b.local", TYPE_A, false)]), t), None);
}

fn probe_from(ip: [u8; 4]) -> Vec<u8> {
    let mut w = Writer::new(0, 0);
    w.question(&Question { name: n("touchos-1a2b.local"), qtype: TYPE_ANY, class: CLASS_IN, unicast: true });
    w.record(2, &Record { name: n("touchos-1a2b.local"), rtype: TYPE_A, class: CLASS_IN, flush: false, ttl: 120, rdata: ip.to_vec() });
    w.finish()
}

#[test]
fn simultaneous_probes() {
    // Someone with a higher address probes too: we lose, wait a second and
    // probe again from the start.
    let mut r = Responder::new("touchos-1a2b");
    r.set_ip(Some(IP), 0);
    let first = r.poll_at().unwrap();
    r.poll(first).unwrap();
    r.on_packet([192, 168, 1, 60], PORT, &probe_from([192, 168, 1, 60]), first + 10);
    assert_eq!(r.poll_at(), Some(first + 10 + 1000));
    assert_eq!(r.host(), "touchos-1a2b");
    // A lower address loses to us: we go on.
    let mut r = Responder::new("touchos-1a2b");
    r.set_ip(Some(IP), 0);
    let first = r.poll_at().unwrap();
    r.poll(first).unwrap();
    r.on_packet(PEER, PORT, &probe_from(PEER), first + 10);
    assert_eq!(r.poll_at(), Some(first + 250));
}

#[test]
fn new_address_and_name() {
    let mut r = Responder::new("touchos-1a2b");
    let t = ready(&mut r);
    r.set_ip(Some([10, 0, 0, 9]), t);
    assert!(!r.is_ready());
    let t = ready_from(&mut r, t);
    let m = answer_of(&r.on_packet(PEER, PORT, &query(0, &[("touchos-1a2b.local", TYPE_A, false)]), t).unwrap());
    assert_eq!(m.answers[0].rdata, [10, 0, 0, 9]);
    // A new name: a goodbye for the old one first.
    r.set_host("kitchen", t);
    let m = answer_of(&r.poll(t).unwrap());
    assert_eq!((m.answers[0].name.clone(), m.answers[0].ttl), (n("touchos-1a2b.local"), 0));
    assert_eq!(r.host(), "kitchen");
    // The address gone: silence.
    r.set_ip(None, t);
    assert_eq!(r.poll_at(), None);
}

#[test]
fn new_name_says_goodbye_to_services_named_after_it() {
    let mut r = Responder::new("touchos-1a2b");
    r.add_service(http(), 0);
    r.add_service(Service { kind: "_esphomelib._tcp".into(), instance: "panel".into(), port: 6053, txt: vec![] }, 0);
    let t = ready(&mut r);
    r.set_host("kitchen", t);
    let m = answer_of(&r.poll(t).unwrap());
    assert!(m.answers.iter().all(|a| a.ttl == 0));
    let has = |name: &str, rtype| m.answers.iter().any(|a| a.rtype == rtype && name_eq(&a.name, &n(name)));
    assert!(has("touchos-1a2b.local", TYPE_A));
    // The instance named after the host: its PTR (shared), SRV and TXT.
    assert!(has("_http._tcp.local", TYPE_PTR));
    assert!(has("touchos-1a2b._http._tcp.local", TYPE_SRV));
    assert!(has("touchos-1a2b._http._tcp.local", TYPE_TXT));
    // One with its own name stays; nor does the service type go.
    assert!(!has("_esphomelib._tcp.local", TYPE_PTR));
    assert!(!has("_services._dns-sd._udp.local", TYPE_PTR));
}

fn ready_from(r: &mut Responder, mut now: u64) -> u64 {
    while let Some(at) = r.poll_at() {
        now = at.max(now);
        r.poll(now);
    }
    now
}

#[test]
fn service_checks() {
    assert!(http().is_valid());
    assert!(Service { kind: "_esphomelib._tcp".into(), ..http() }.is_valid());
    for kind in ["http._tcp", "_http", "_http._sctp", "_a._tcp.local", "_waytoolongservicename._tcp"] {
        assert!(!Service { kind: kind.to_string(), ..http() }.is_valid(), "{kind}");
    }
    assert!(!Service { instance: "x".repeat(64), ..http() }.is_valid());
    assert!(!Service { txt: vec![String::new()], ..http() }.is_valid());
}
