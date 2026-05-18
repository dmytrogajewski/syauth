//! One-shot probe: print session_uuid_for(&[0u8; 32], minute) and
//! session_uuid_for_bond(vec![0; 32], minute) for a specified minute.
//! Run with: cargo test -p syauth -- probe_pair_uuid --nocapture --ignored
//!
//! No assertions — output is meant for manual comparison against
//! phone-side CDM logcat entries.
use syauth_mobile::session_uuid_for_bond;
use syauth_transport::session_uuid_for;

#[test]
#[ignore]
fn probe_pair_uuid() {
    let bond_key = [0u8; 32];
    let minute_env = std::env::var("SYAUTH_PROBE_MINUTE").ok();
    let minute: i64 = match minute_env {
        Some(s) => s.parse().expect("SYAUTH_PROBE_MINUTE must be an integer"),
        None => {
            let now_sec = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before epoch")
                .as_secs() as i64;
            now_sec / 60
        }
    };

    let transport_bytes = session_uuid_for(&bond_key, minute);
    let mobile_bytes = session_uuid_for_bond(bond_key.to_vec(), minute).expect("mobile uuid");
    let transport_uuid = uuid::Uuid::from_bytes(transport_bytes);
    let mobile_uuid_bytes: [u8; 16] = mobile_bytes.as_slice().try_into().expect("16 bytes");
    let mobile_uuid = uuid::Uuid::from_bytes(mobile_uuid_bytes);

    println!("minute = {minute}");
    println!("transport bytes  = {:02x?}", &transport_bytes);
    println!("mobile bytes     = {:02x?}", &mobile_bytes);
    println!("transport UUID   = {transport_uuid}");
    println!("mobile UUID      = {mobile_uuid}");
    println!("byte-equal       = {}", transport_bytes.as_slice() == mobile_bytes.as_slice());
    println!("minute - 1       = {}", minute - 1);
    let prev_bytes = session_uuid_for(&bond_key, minute - 1);
    println!("transport (M-1)  = {}", uuid::Uuid::from_bytes(prev_bytes));
}
