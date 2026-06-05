//! `engine` demo — a tiny **unpinned** smoke run of the assembled pipeline for the
//! README. It does NO measurement (no clock floor, no pinning, no CO accounting —
//! that is Benchmark 7's job in `bench`). It just proves the parts compose: drive a
//! synthetic corpus through `EngineProducer::<BTreeBook>::process`, drain it from a
//! keeping-up consumer, and from a deliberately-lapped one, and print a summary.
#![forbid(unsafe_code)]

use book::{BTreeBook, Px, Qty};
use engine::{Engine, Observed};
use feed::synthetic::{GenConfig, Profile, generate};

fn main() {
    let cfg = GenConfig {
        profile: Profile::Steady,
        seed: 1,
        events: 10_000,
        mid: Px(65_000),
        band: 64,
        max_qty: Qty(1_024),
        start_ts: 0,
    };
    let corpus = generate(&cfg);

    // A keeping-up consumer on a roomy ring: drain after every push, never lapped.
    {
        let (mut producer, handle) = Engine::<BTreeBook>::new(1 << 16);
        let mut consumer = handle.consumer();
        let mut delivered = 0u64;
        for ev in &corpus {
            producer.process(ev);
            if let Observed::Event(_) = consumer.poll() {
                delivered += 1;
            }
        }
        // Flush anything still resident.
        while let Observed::Event(_) = consumer.poll() {
            delivered += 1;
        }
        let top = producer.top_of_book();
        println!(
            "keeping-up : {delivered}/{} events delivered, 0 resyncs, \
             final top bid={} ask={} (stamp {})",
            corpus.len(),
            top.bid_px,
            top.ask_px,
            top.stamp,
        );
        assert_eq!(delivered, corpus.len() as u64, "keeping-up consumer saw every event");
    }

    // A lapped consumer on a tiny ring: the producer overruns it; it resyncs from
    // the seqlock and keeps deriving a mid from the snapshot.
    {
        let (mut producer, handle) = Engine::<BTreeBook>::new(8);
        let mut consumer = handle.consumer();
        for ev in &corpus {
            producer.process(ev);
        }
        // Drain whatever is left; every overrun resyncs from the seqlock snapshot.
        loop {
            match consumer.poll() {
                Observed::Idle => break,
                Observed::Event(_) | Observed::Overrun { .. } => {}
            }
        }
        println!(
            "lapped     : {} clean events, {} resyncs, last derived mid={}",
            consumer.seen, consumer.resyncs, consumer.last_mid,
        );
        assert!(consumer.resyncs > 0, "a tiny ring must lap the stalled consumer");
    }

    println!("engine demo ok");
}
