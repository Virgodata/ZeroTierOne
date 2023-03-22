use std::iter::ExactSizeIterator;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use zerotier_crypto::p384::{P384KeyPair, P384PublicKey};
use zerotier_crypto::random;
use zerotier_crypto::secret::Secret;
use zerotier_utils::hex;
use zerotier_utils::ms_monotonic;

const TEST_MTU: usize = 1500;

struct TestApplication {
    identity_key: P384KeyPair,
}

impl zssp::ApplicationLayer for TestApplication {
    const REKEY_AFTER_USES: u64 = 300000;
    const EXPIRE_AFTER_USES: u64 = 2147483648;
    const REKEY_AFTER_TIME_MS: i64 = 1000 * 60 * 60 * 2;
    const REKEY_AFTER_TIME_MS_MAX_JITTER: u32 = 1000 * 60 * 10;
    const INCOMING_SESSION_NEGOTIATION_TIMEOUT_MS: i64 = 2000;
    const RETRY_INTERVAL: i64 = 500;

    type Data = ();
    type IncomingPacketBuffer = Vec<u8>;
    type PhysicalPath = usize;

    fn get_local_s_public_blob(&self) -> &[u8] {
        self.identity_key.public_key_bytes()
    }

    fn get_local_s_keypair(&self) -> &zerotier_crypto::p384::P384KeyPair {
        &self.identity_key
    }
}

fn alice_main(
    run: &AtomicBool,
    packet_success_rate: u32,
    alice_app: &TestApplication,
    bob_app: &TestApplication,
    alice_out: mpsc::SyncSender<Vec<u8>>,
    alice_in: mpsc::Receiver<Vec<u8>>,
) {
    let context = zssp::Context::<TestApplication>::new(alice_app.identity_key.public_key_bytes(), TEST_MTU);
    let mut data_buf = [0u8; 65536];
    let mut next_service = ms_monotonic() + 500;
    let mut last_ratchet_count = 0;
    let test_data = [1u8; TEST_MTU * 10];
    let mut up = false;
    let mut last_error = zssp::Error::UnknownProtocolVersion;

    let alice_session = context
        .open(
            alice_app,
            |b| {
                let _ = alice_out.send(b.to_vec());
            },
            TEST_MTU,
            bob_app.identity_key.public_key_bytes(),
            bob_app.identity_key.public_key().clone(),
            Secret::default(),
            None,
            (),
            ms_monotonic(),
        )
        .unwrap();

    println!("[alice] opening session {}", alice_session.id.to_string());

    while run.load(Ordering::Relaxed) {
        let current_time = ms_monotonic();
        loop {
            let pkt = alice_in.try_recv();
            if let Ok(pkt) = pkt {
                if (random::xorshift64_random() as u32) <= packet_success_rate {
                    match context.receive(
                        alice_app,
                        || true,
                        |s_public| Some((P384PublicKey::from_bytes(s_public).unwrap(), Secret::default(), ())),
                        |_, b| {
                            let _ = alice_out.send(b.to_vec());
                        },
                        0,
                        &mut data_buf,
                        pkt,
                        current_time,
                    ) {
                        Ok(zssp::ReceiveResult::Ok(_)) => {
                            //println!("[alice] ok");
                        }
                        Ok(zssp::ReceiveResult::OkData(_, _)) => {
                            //println!("[alice] received {}", data.len());
                        }
                        Ok(zssp::ReceiveResult::OkNewSession(s, _)) => {
                            println!("[alice] new session {}", s.id.to_string());
                        }
                        Ok(zssp::ReceiveResult::Rejected) => {}
                        Err(e) => {
                            if e != last_error {
                                println!("[alice] ERROR {}", e.to_string());
                                last_error = e;
                            }
                        }
                    }
                }
            } else {
                break;
            }
        }

        if up {
            let ki = alice_session.key_info().unwrap();
            if ki.0 > last_ratchet_count {
                last_ratchet_count = ki.0;
                println!("[alice] new key! ratchet count {} fp {}", ki.0, hex::to_string(&ki.1[..16]));
            }

            assert!(alice_session
                .send(
                    |b| {
                        let _ = alice_out.send(b.to_vec());
                    },
                    &mut data_buf[..TEST_MTU],
                    &test_data[..1400 + ((random::xorshift64_random() as usize) % (test_data.len() - 1400))],
                )
                .is_ok());
        } else {
            if alice_session.established() {
                up = true;
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }

        if current_time >= next_service {
            next_service = current_time
                + context.service(
                    |_, b| {
                        let _ = alice_out.send(b.to_vec());
                    },
                    current_time,
                );
        }
    }
}

fn bob_main(
    run: &AtomicBool,
    packet_success_rate: u32,
    _alice_app: &TestApplication,
    bob_app: &TestApplication,
    bob_out: mpsc::SyncSender<Vec<u8>>,
    bob_in: mpsc::Receiver<Vec<u8>>,
) {
    let context = zssp::Context::<TestApplication>::new(bob_app.identity_key.public_key_bytes(), TEST_MTU);
    let mut data_buf = [0u8; 65536];
    let mut data_buf_2 = [0u8; TEST_MTU];
    let mut last_ratchet_count = 0;
    let mut last_speed_metric = ms_monotonic();
    let mut next_service = last_speed_metric + 500;
    let mut transferred = 0u64;
    let mut last_error = zssp::Error::UnknownProtocolVersion;

    let mut bob_session = None;

    while run.load(Ordering::Relaxed) {
        let pkt = bob_in.recv_timeout(Duration::from_millis(100));
        let current_time = ms_monotonic();

        if let Ok(pkt) = pkt {
            if (random::xorshift64_random() as u32) <= packet_success_rate {
                match context.receive(
                    bob_app,
                    || true,
                    |s_public| Some((P384PublicKey::from_bytes(s_public).unwrap(), Secret::default(), ())),
                    |_, b| {
                        let _ = bob_out.send(b.to_vec());
                    },
                    0,
                    &mut data_buf,
                    pkt,
                    current_time,
                ) {
                    Ok(zssp::ReceiveResult::Ok(_)) => {
                        //println!("[bob] ok");
                    }
                    Ok(zssp::ReceiveResult::OkData(s, data)) => {
                        //println!("[bob] received {}", data.len());
                        assert!(s
                            .send(
                                |b| {
                                    let _ = bob_out.send(b.to_vec());
                                },
                                &mut data_buf_2,
                                data.as_mut(),
                            )
                            .is_ok());
                        transferred += data.len() as u64 * 2; // *2 because we are also sending this many bytes back
                    }
                    Ok(zssp::ReceiveResult::OkNewSession(s, _)) => {
                        println!("[bob] new session {}", s.id.to_string());
                        let _ = bob_session.replace(s);
                    }
                    Ok(zssp::ReceiveResult::Rejected) => {}
                    Err(e) => {
                        if e != last_error {
                            println!("[bob] ERROR {}", e.to_string());
                            last_error = e;
                        }
                    }
                }
            }
        }

        if let Some(bob_session) = bob_session.as_ref() {
            let ki = bob_session.key_info().unwrap();
            if ki.0 > last_ratchet_count {
                last_ratchet_count = ki.0;
                println!("[bob] new key! ratchet count {} fp {}", ki.0, hex::to_string(&ki.1[..16]));
            }
        }

        let speed_metric_elapsed = current_time - last_speed_metric;
        if speed_metric_elapsed >= 1000 {
            last_speed_metric = current_time;
            if transferred > 0 {
                println!(
                    "[bob] throughput: {} MiB/sec (combined input and output)",
                    ((transferred as f64) / 1048576.0) / ((speed_metric_elapsed as f64) / 1000.0)
                );
            }
            transferred = 0;
        }

        if current_time >= next_service {
            next_service = current_time
                + context.service(
                    |_, b| {
                        let _ = bob_out.send(b.to_vec());
                    },
                    current_time,
                );
        }
    }
}

fn main() {
    let run = AtomicBool::new(true);

    let alice_app = TestApplication { identity_key: P384KeyPair::generate() };
    let bob_app = TestApplication { identity_key: P384KeyPair::generate() };

    let (alice_out, bob_in) = mpsc::sync_channel::<Vec<u8>>(1024);
    let (bob_out, alice_in) = mpsc::sync_channel::<Vec<u8>>(1024);

    let args = std::env::args();
    let packet_success_rate = if args.len() <= 1 {
        u32::MAX
    } else {
        ((u32::MAX as f64) * f64::from_str(args.last().unwrap().as_str()).unwrap()) as u32
    };

    thread::scope(|ts| {
        let alice_thread = ts.spawn(|| alice_main(&run, packet_success_rate, &alice_app, &bob_app, alice_out, alice_in));
        let bob_thread = ts.spawn(|| bob_main(&run, packet_success_rate, &alice_app, &bob_app, bob_out, bob_in));

        thread::sleep(Duration::from_secs(60 * 60));

        run.store(false, Ordering::SeqCst);
        let _ = alice_thread.join();
        let _ = bob_thread.join();
    });

    std::process::exit(0);
}
