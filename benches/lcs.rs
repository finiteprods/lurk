use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use p3_baby_bear::BabyBear;
use p3_field::AbstractField;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use sp1_stark::{
    air::MachineAir, baby_bear_poseidon2::BabyBearPoseidon2, CpuProver, MachineProver, SP1CoreOpts,
    StarkGenericConfig, StarkMachine,
};
use std::{sync::Arc, time::Duration};

use lurk::{
    core::{
        eval_direct::build_lurk_toplevel_native,
        zstore::{lurk_zstore, ZPtr},
    },
    lair::{
        chipset::{Chipset, NoChip},
        execute::{QueryRecord, Shard},
        func_chip::FuncChip,
        lair_chip::{build_chip_vector, build_lair_chip_vector, LairMachineProgram},
        toplevel::Toplevel,
        List,
    },
};

fn get_lcs_args() -> (&'static str, &'static str) {
    ( "When in the Course of human events, it becomes necessary for one people to dissolve the political bands which have connected them with another",
       "There must be some kind of way outta here Said the joker to the thief. There's too much confusion. I can't get no relief.")
}

fn build_lurk_expr(a: &str, b: &str) -> String {
    format!(
        r#"
(letrec ((lte (lambda (a b)
                (if (eq a "") t
                    (if (eq b "") nil
                        (lte (cdr a) (cdr b))))))
         (lcs (lambda (a b)
                (if (eq a "") ""
                    (if (eq b "") ""
                        (if (eq (car a) (car b)) (strcons (car a) (lcs (cdr a) (cdr b)))
                            (if (lte (lcs a (cdr b)) (lcs (cdr a) b)) (lcs (cdr a) b)
                                (lcs a (cdr b)))))))))
  (lcs "{a}" "{b}"))"#
    )
}

fn setup<'a, C: Chipset<BabyBear>>(
    a: &'a str,
    b: &'a str,
    toplevel: &'a Arc<Toplevel<BabyBear, C, NoChip>>,
) -> (
    List<BabyBear>,
    FuncChip<BabyBear, C, NoChip>,
    QueryRecord<BabyBear>,
) {
    let code = build_lurk_expr(a, b);
    let zstore = &mut lurk_zstore();
    let ZPtr { tag, digest } = zstore.read(&code, &Default::default());

    let mut record = QueryRecord::new(toplevel);
    record.inject_inv_queries("hash4", toplevel, &zstore.hashes4);

    let mut full_input = [BabyBear::zero(); 24];
    full_input[0] = tag.to_field();
    full_input[8..16].copy_from_slice(&digest);

    let args: List<_> = full_input.into();
    let lurk_main = FuncChip::from_name("lurk_main", toplevel);

    (args, lurk_main, record)
}

fn evaluation(c: &mut Criterion) {
    let args = get_lcs_args();
    c.bench_function("lcs-evaluation", |b| {
        let (toplevel, ..) = build_lurk_toplevel_native();
        let (args, lurk_main, record) = setup(args.0, args.1, &toplevel);
        b.iter_batched(
            || (args.clone(), record.clone()),
            |(args, mut queries)| {
                toplevel
                    .execute(lurk_main.func(), &args, &mut queries, None)
                    .unwrap();
            },
            BatchSize::SmallInput,
        )
    });
}

fn trace_generation(c: &mut Criterion) {
    let args = get_lcs_args();
    c.bench_function("lcs-trace-generation", |b| {
        let (toplevel, ..) = build_lurk_toplevel_native();
        let (args, lurk_main, mut record) = setup(args.0, args.1, &toplevel);
        toplevel
            .execute(lurk_main.func(), &args, &mut record, None)
            .unwrap();
        let record = Arc::new(record);
        let lair_chips = build_lair_chip_vector(&lurk_main);
        b.iter(|| {
            lair_chips.par_iter().for_each(|func_chip| {
                let shards = Shard::new_arc(&record);
                assert_eq!(shards.len(), 1);
                let shard = &shards[0];
                func_chip.generate_trace(shard, &mut Default::default());
            })
        })
    });
}

fn verification(c: &mut Criterion) {
    let args = get_lcs_args();
    c.bench_function("lcs-verification", |b| {
        let (toplevel, ..) = build_lurk_toplevel_native();
        let (args, lurk_main, mut record) = setup(args.0, args.1, &toplevel);

        toplevel
            .execute(lurk_main.func(), &args, &mut record, None)
            .unwrap();
        let machine = StarkMachine::new(
            BabyBearPoseidon2::new(),
            build_chip_vector(&lurk_main),
            record.expect_public_values().len(),
            true,
        );
        let (pk, vk) = machine.setup(&LairMachineProgram);
        let mut challenger_p = machine.config().challenger();
        let opts = SP1CoreOpts::default();
        let record = Arc::new(record);
        let shards = Shard::new_arc(&record);
        let prover = CpuProver::new(machine);
        let proof = prover.prove(&pk, shards, &mut challenger_p, opts).unwrap();

        b.iter_batched(
            || {
                StarkMachine::new(
                    BabyBearPoseidon2::new(),
                    build_chip_vector(&lurk_main),
                    record.expect_public_values().len(),
                    true,
                )
            },
            |machine| {
                let mut challenger = machine.config().challenger();
                machine.verify(&vk, &proof, &mut challenger).unwrap();
            },
            BatchSize::SmallInput,
        )
    });
}

fn e2e(c: &mut Criterion) {
    let args = get_lcs_args();
    c.bench_function("lcs-e2e", |b| {
        let (toplevel, ..) = build_lurk_toplevel_native();
        let (args, lurk_main, record) = setup(args.0, args.1, &toplevel);

        b.iter_batched(
            || (record.clone(), args.clone()),
            |(mut record, args)| {
                toplevel
                    .execute(lurk_main.func(), &args, &mut record, None)
                    .unwrap();
                let config = BabyBearPoseidon2::new();
                let machine = StarkMachine::new(
                    config,
                    build_chip_vector(&lurk_main),
                    record.expect_public_values().len(),
                    true,
                );
                let (pk, _) = machine.setup(&LairMachineProgram);
                let mut challenger_p = machine.config().challenger();
                let opts = SP1CoreOpts::default();
                let shards = Shard::new(record);
                let prover = CpuProver::new(machine);
                prover.prove(&pk, shards, &mut challenger_p, opts).unwrap();
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group! {
    name = lcs_benches;
    config = Criterion::default()
                .measurement_time(Duration::from_secs(15))
                .sample_size(10);
    targets =
        evaluation,
        trace_generation,
        verification,
        e2e,
}

// `cargo criterion --bench lcs
criterion_main!(lcs_benches);
