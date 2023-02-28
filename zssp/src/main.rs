use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use zerotier_crypto::p384::{P384KeyPair, P384PublicKey};
use zerotier_crypto::secret::Secret;
use zerotier_utils::ms_monotonic;

const TEST_MTU: usize = 1500;

struct TestApplication {
    identity_key: P384KeyPair,
}

impl zssp::ApplicationLayer for TestApplication {
    type Data = ();

    type IncomingPacketBuffer = Vec<u8>;

    fn get_local_s_public_blob(&self) -> &[u8] {
        self.identity_key.public_key_bytes()
    }

    fn get_local_s_keypair(&self) -> &zerotier_crypto::p384::P384KeyPair {
        &self.identity_key
    }
}

fn alice_main(
    run: &AtomicBool,
    alice_app: &TestApplication,
    bob_app: &TestApplication,
    alice_out: mpsc::SyncSender<Vec<u8>>,
    alice_in: mpsc::Receiver<Vec<u8>>,
) {
    let context = zssp::Context::<TestApplication>::new(16);
    let mut data_buf = [0u8; 65536];
    let mut next_service = ms_monotonic() + 500;

    let alice_session = context
        .open(
            alice_app,
            |b| {
                let _ = alice_out.send(b.to_vec());
            },
            TEST_MTU,
            bob_app.identity_key.public_key(),
            Secret::default(),
            None,
            (),
            ms_monotonic(),
        )
        .unwrap();

    println!("[alice] opening session {}", alice_session.id.to_string());

    while run.load(Ordering::Relaxed) {
        let pkt = alice_in.try_recv();
        let current_time = ms_monotonic();

        if let Ok(pkt) = pkt {
            //println!("bob >> alice {}", pkt.len());
            match context.receive(
                alice_app,
                || true,
                |s_public, _| Some((P384PublicKey::from_bytes(s_public).unwrap(), Secret::default(), ())),
                |_, b| {
                    let _ = alice_out.send(b.to_vec());
                },
                &mut data_buf,
                pkt,
                TEST_MTU,
                current_time,
            ) {
                Ok(zssp::ReceiveResult::Ok) => {
                    println!("[alice] ok");
                }
                Ok(zssp::ReceiveResult::OkData(_, data)) => {
                    println!("[alice] received {}", data.len());
                }
                Ok(zssp::ReceiveResult::OkNewSession(s)) => {
                    println!("[alice] new session {}", s.id.to_string());
                }
                Ok(zssp::ReceiveResult::Rejected) => {}
                Err(e) => {
                    println!("[alice] ERROR {}", e.to_string());
                }
            }
        }

        if current_time >= next_service {
            next_service = current_time
                + context.service(
                    |_, b| {
                        let _ = alice_out.send(b.to_vec());
                    },
                    TEST_MTU,
                    current_time,
                );
        }
    }
}

fn bob_main(
    run: &AtomicBool,
    _alice_app: &TestApplication,
    bob_app: &TestApplication,
    bob_out: mpsc::SyncSender<Vec<u8>>,
    bob_in: mpsc::Receiver<Vec<u8>>,
) {
    let context = zssp::Context::<TestApplication>::new(16);
    let mut data_buf = [0u8; 65536];
    let mut next_service = ms_monotonic() + 500;

    while run.load(Ordering::Relaxed) {
        let pkt = bob_in.recv_timeout(Duration::from_millis(10));
        let current_time = ms_monotonic();

        if let Ok(pkt) = pkt {
            //println!("alice >> bob {}", pkt.len());
            match context.receive(
                bob_app,
                || true,
                |s_public, _| Some((P384PublicKey::from_bytes(s_public).unwrap(), Secret::default(), ())),
                |_, b| {
                    let _ = bob_out.send(b.to_vec());
                },
                &mut data_buf,
                pkt,
                TEST_MTU,
                current_time,
            ) {
                Ok(zssp::ReceiveResult::Ok) => {
                    println!("[bob] ok");
                }
                Ok(zssp::ReceiveResult::OkData(_, data)) => {
                    println!("[bob] received {}", data.len());
                }
                Ok(zssp::ReceiveResult::OkNewSession(s)) => {
                    println!("[bob] new session {}", s.id.to_string());
                }
                Ok(zssp::ReceiveResult::Rejected) => {}
                Err(e) => {
                    println!("[bob] ERROR {}", e.to_string());
                }
            }
        }

        if current_time >= next_service {
            next_service = current_time
                + context.service(
                    |_, b| {
                        let _ = bob_out.send(b.to_vec());
                    },
                    TEST_MTU,
                    current_time,
                );
        }
    }
}

fn main() {
    let run = AtomicBool::new(true);

    let alice_app = TestApplication { identity_key: P384KeyPair::generate() };
    let bob_app = TestApplication { identity_key: P384KeyPair::generate() };

    let (alice_out, bob_in) = mpsc::sync_channel::<Vec<u8>>(128);
    let (bob_out, alice_in) = mpsc::sync_channel::<Vec<u8>>(128);

    thread::scope(|ts| {
        let alice_thread = ts.spawn(|| alice_main(&run, &alice_app, &bob_app, alice_out, alice_in));
        let bob_thread = ts.spawn(|| bob_main(&run, &alice_app, &bob_app, bob_out, bob_in));

        thread::sleep(Duration::from_secs(60 * 10));

        run.store(false, Ordering::SeqCst);
        let _ = alice_thread.join();
        let _ = bob_thread.join();
    });

    std::process::exit(0);
}
